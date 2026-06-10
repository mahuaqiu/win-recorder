//! 基于 IMFTransform 的 H.264 编码器
//!
//! 直接使用 Media Foundation Transform (MFT) 接口进行 H264 编码，
//! 不依赖 MFSinkWriter，可以直接在内存中获取编码后的 NAL 单元数据。
//!
//! # 架构
//! - 输入: RGB32 (BGRA) 格式的帧数据
//! - 编码: H264 编码器 MFT（内部自动处理 RGB32 -> NV12 颜色转换）
//! - 输出: Annex-B 格式的 H264 码流

use crate::error::RecorderError;
use crate::memory_byte_stream::{extract_nal_units, get_nal_type};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::mem::ManuallyDrop;
use std::ptr;
use windows::core::GUID;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;

/// CMSH264EncoderMFT 的 CLSID
const CLSID_MSH264_ENCODER_MFT: GUID = GUID::from_values(
    0x6CA50344,
    0x051A,
    0x4DED,
    [0x97, 0x79, 0xA4, 0x33, 0x05, 0x16, 0x5E, 0x35],
);

/// H264 编码参数
#[derive(Debug, Clone)]
pub struct H264EncodeParams {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub profile: u32,
}

impl Default for H264EncodeParams {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 10,
            bitrate: 2_000_000,
            profile: 66,
        }
    }
}

/// 编码帧类型
#[derive(Debug, Clone, PartialEq)]
pub enum FrameType {
    IDR,
    PFrame,
    SPS,
    PPS,
    Unknown,
}

/// 编码后的帧数据
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub frame_type: FrameType,
    pub data: Vec<u8>,
}

/// 基于 IMFTransform 的 H264 编码器
///
/// 直接使用 RGB32 输入，H264 编码器 MFT 内部自动处理颜色转换（同 WinRecorder 的 MFSinkWriter 方式）。
#[pyclass]
pub struct H264Encoder {
    h264_encoder: Option<IMFTransform>,
    params: H264EncodeParams,
    initialized: bool,
    com_initialized: bool,
    frame_duration: i64,
    frame_count: u64,
    sps: Vec<u8>,
    pps: Vec<u8>,
    encoder_input_id: u32,
    encoder_output_id: u32,
}

unsafe impl Send for H264Encoder {}

impl H264Encoder {
    pub fn from_params(params: H264EncodeParams) -> Result<Self, RecorderError> {
        if params.fps == 0 {
            return Err(RecorderError::InvalidParam(
                "fps must be greater than 0".into(),
            ));
        }
        if params.width == 0 || params.height == 0 {
            return Err(RecorderError::InvalidParam(
                "width and height must be greater than 0".into(),
            ));
        }

        let aligned_width = (params.width + 15) & !15;
        let aligned_height = (params.height + 15) & !15;
        let frame_duration = 10_000_000_i64 / params.fps as i64;

        let mut params = params;
        params.width = aligned_width;
        params.height = aligned_height;

        Ok(Self {
            h264_encoder: None,
            params,
            initialized: false,
            com_initialized: false,
            frame_duration,
            frame_count: 0,
            sps: Vec::new(),
            pps: Vec::new(),
            encoder_input_id: 0,
            encoder_output_id: 0,
        })
    }

    /// 初始化并启动编码器
    pub fn start_encoding(&mut self) -> Result<Vec<EncodedFrame>, RecorderError> {
        if self.initialized {
            return Err(RecorderError::AlreadyRecording);
        }

        let start_result = unsafe {
            if let Err(e) = CoInitializeEx(None, COINIT_MULTITHREADED).ok() {
                if e.code() != RPC_E_CHANGED_MODE {
                    return Err(RecorderError::MFError(format!(
                        "CoInitializeEx 失败: {}",
                        e
                    )));
                }
                self.com_initialized = false;
            } else {
                self.com_initialized = true;
            }

            MFStartup(MFSTARTUP_LITE, 0)
                .map_err(|e| RecorderError::MFError(format!("MFStartup 失败: {}", e)))?;

            let h264_encoder = self.create_h264_encoder()?;
            self.h264_encoder = Some(h264_encoder);

            // 配置管线：RGB32 输入 -> H264 编码器（内部自动转换颜色）
            self.configure_pipeline()?;

            self.send_stream_messages()?;

            self.initialized = true;

            self.extract_sps_pps_from_attributes()?;

            println!(
                "[H264Encoder] 编码器初始化成功: {}x{} @ {}fps, profile={}",
                self.params.width, self.params.height, self.params.fps, self.params.profile
            );
            Ok::<(), RecorderError>(())
        };

        if let Err(err) = start_result {
            unsafe {
                self.h264_encoder = None;
                let _ = MFShutdown();
                if self.com_initialized {
                    CoUninitialize();
                    self.com_initialized = false;
                }
            }
            return Err(err);
        }

        let mut init_frames = Vec::new();
        if !self.sps.is_empty() {
            init_frames.push(EncodedFrame {
                frame_type: FrameType::SPS,
                data: self.sps.clone(),
            });
        }
        if !self.pps.is_empty() {
            init_frames.push(EncodedFrame {
                frame_type: FrameType::PPS,
                data: self.pps.clone(),
            });
        }

        Ok(init_frames)
    }

    /// 编码单帧
    pub fn encode_frame_data(
        &mut self,
        bgra_data: &[u8],
    ) -> Result<Vec<EncodedFrame>, RecorderError> {
        if !self.initialized {
            return Err(RecorderError::NotRecording);
        }

        let expected_size = (self.params.width * self.params.height * 4) as usize;
        if bgra_data.len() != expected_size {
            return Err(RecorderError::FrameSizeMismatch {
                expected: expected_size,
                actual: bgra_data.len(),
            });
        }

        unsafe {
            let encoder = self
                .h264_encoder
                .as_ref()
                .ok_or(RecorderError::NotRecording)?;

            let input_sample = self.create_bgra_sample(bgra_data)?;
            encoder
                .ProcessInput(self.encoder_input_id, &input_sample, 0)
                .map_err(|e| {
                    RecorderError::MFError(format!("H264 编码器 ProcessInput 失败: {}", e))
                })?;

            let mut encoded_frames = self.process_encoder_output()?;

            if self.sps.is_empty() || self.pps.is_empty() {
                self.extract_sps_pps_from_attributes()?;
            }

            for frame in &mut encoded_frames {
                if frame.frame_type == FrameType::Unknown {
                    frame.frame_type = Self::detect_frame_type(&frame.data);
                }
            }

            self.frame_count += 1;

            if self.frame_count % 30 == 0 {
                println!(
                    "[H264Encoder] 已编码 {} 帧，当前帧产生 {} 个 NAL 单元",
                    self.frame_count,
                    encoded_frames.len()
                );
            }

            Ok(encoded_frames)
        }
    }

    /// 停止编码器
    pub fn stop_encoding(&mut self) -> Result<Vec<EncodedFrame>, RecorderError> {
        if !self.initialized {
            return Ok(Vec::new());
        }

        let mut remaining_frames = Vec::new();

        unsafe {
            if let Some(encoder) = &self.h264_encoder {
                let _ = encoder.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
                if let Ok(frames) = self.process_encoder_output() {
                    remaining_frames.extend(frames);
                }
            }

            self.h264_encoder = None;
            let _ = MFShutdown();
            if self.com_initialized {
                CoUninitialize();
                self.com_initialized = false;
            }
        }

        self.initialized = false;
        self.frame_count = 0;

        println!("[H264Encoder] 编码器已停止");
        Ok(remaining_frames)
    }

    pub fn sps(&self) -> &[u8] {
        &self.sps
    }

    pub fn pps(&self) -> &[u8] {
        &self.pps
    }

    pub fn params(&self) -> &H264EncodeParams {
        &self.params
    }

    pub fn encoded_frame_count(&self) -> u64 {
        self.frame_count
    }

    // ==================== 内部方法 ====================

    /// 创建 H264 编码 MFT
    unsafe fn create_h264_encoder(&self) -> Result<IMFTransform, RecorderError> {
        let encoder: IMFTransform =
            CoCreateInstance(&CLSID_MSH264_ENCODER_MFT, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| RecorderError::MFError(format!("创建 H264 编码 MFT 失败: {}", e)))?;

        println!("[H264Encoder] H264 编码 MFT 创建成功");
        Ok(encoder)
    }

    /// 配置 MFT 管线：RGB32 输入 -> H264 输出
    ///
    /// 和 WinRecorder 的 MFSinkWriter 一样，直接使用 RGB32 作为输入类型，
    /// H264 编码器 MFT 内部会自动处理颜色转换。
    unsafe fn configure_pipeline(&mut self) -> Result<(), RecorderError> {
        let h264_encoder = self
            .h264_encoder
            .as_ref()
            .ok_or_else(|| RecorderError::MFError("H264 编码器未创建".into()))?;

        // 获取流 ID
        self.encoder_input_id = self.get_input_stream_id(h264_encoder)?;
        self.encoder_output_id = self.get_output_stream_id(h264_encoder)?;

        // 1. 设置 RGB32 输入类型（编码器内部自动转换颜色）
        let rgb32_type = self.create_rgb32_media_type()?;
        h264_encoder
            .SetInputType(self.encoder_input_id, &rgb32_type, 0)
            .map_err(|e| RecorderError::MFError(format!("设置 RGB32 输入类型失败: {}", e)))?;

        // 2. 设置 H264 输出类型
        let h264_type = self.create_h264_media_type()?;
        h264_encoder
            .SetOutputType(self.encoder_output_id, &h264_type, 0)
            .map_err(|e| RecorderError::MFError(format!("设置 H264 输出类型失败: {}", e)))?;

        println!("[H264Encoder] 管线配置完成: RGB32 -> H264 (内置颜色转换)");
        Ok(())
    }

    /// 创建 RGB32 (BGRA) 输入媒体类型
    unsafe fn create_rgb32_media_type(&self) -> Result<IMFMediaType, RecorderError> {
        let media_type = MFCreateMediaType()
            .map_err(|e| RecorderError::MFError(format!("创建 RGB32 MediaType 失败: {}", e)))?;

        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| RecorderError::MFError(format!("设置主类型失败: {}", e)))?;

        media_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
            .map_err(|e| RecorderError::MFError(format!("设置子类型失败: {}", e)))?;

        let aligned_width = (self.params.width + 15) & !15;
        let aligned_height = (self.params.height + 15) & !15;
        media_type
            .SetUINT64(
                &MF_MT_FRAME_SIZE,
                ((aligned_width as u64) << 32) | (aligned_height as u64),
            )
            .map_err(|e| RecorderError::MFError(format!("设置帧大小失败: {}", e)))?;

        media_type
            .SetUINT64(&MF_MT_FRAME_RATE, ((self.params.fps as u64) << 32) | 1u64)
            .map_err(|e| RecorderError::MFError(format!("设置帧率失败: {}", e)))?;

        media_type
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1u64)
            .map_err(|e| RecorderError::MFError(format!("设置像素宽高比失败: {}", e)))?;

        media_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| RecorderError::MFError(format!("设置交错模式失败: {}", e)))?;

        media_type
            .SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)
            .map_err(|e| RecorderError::MFError(format!("设置样本独立属性失败: {}", e)))?;

        // BGRA stride = width * 4（正数表示从上到下）
        let stride = (aligned_width * 4) as i32 as u32;
        media_type
            .SetUINT32(&MF_MT_DEFAULT_STRIDE, stride)
            .map_err(|e| RecorderError::MFError(format!("设置 RGB32 stride 失败: {}", e)))?;

        Ok(media_type)
    }

    /// 发送流控制消息
    unsafe fn send_stream_messages(&self) -> Result<(), RecorderError> {
        let h264_encoder = self
            .h264_encoder
            .as_ref()
            .ok_or_else(|| RecorderError::MFError("H264 编码器未创建".into()))?;

        h264_encoder
            .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
            .map_err(|e| {
                RecorderError::MFError(format!("H264 编码器 START_OF_STREAM 失败: {}", e))
            })?;

        h264_encoder
            .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
            .map_err(|e| {
                RecorderError::MFError(format!("H264 编码器 BEGIN_STREAMING 失败: {}", e))
            })?;

        println!("[H264Encoder] 流控制消息已发送");
        Ok(())
    }

    /// 创建 H264 输出媒体类型
    unsafe fn create_h264_media_type(&self) -> Result<IMFMediaType, RecorderError> {
        let media_type = MFCreateMediaType()
            .map_err(|e| RecorderError::MFError(format!("创建 H264 MediaType 失败: {}", e)))?;

        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| RecorderError::MFError(format!("设置主类型失败: {}", e)))?;

        media_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
            .map_err(|e| RecorderError::MFError(format!("设置子类型失败: {}", e)))?;

        media_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| RecorderError::MFError(format!("设置交错模式失败: {}", e)))?;

        let aligned_width = (self.params.width + 15) & !15;
        let aligned_height = (self.params.height + 15) & !15;
        media_type
            .SetUINT64(
                &MF_MT_FRAME_SIZE,
                ((aligned_width as u64) << 32) | (aligned_height as u64),
            )
            .map_err(|e| RecorderError::MFError(format!("设置帧大小失败: {}", e)))?;

        media_type
            .SetUINT64(&MF_MT_FRAME_RATE, ((self.params.fps as u64) << 32) | 1u64)
            .map_err(|e| RecorderError::MFError(format!("设置帧率失败: {}", e)))?;

        media_type
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1u64)
            .map_err(|e| RecorderError::MFError(format!("设置像素宽高比失败: {}", e)))?;

        media_type
            .SetUINT32(&MF_MT_AVG_BITRATE, self.params.bitrate)
            .map_err(|e| RecorderError::MFError(format!("设置码率失败: {}", e)))?;

        media_type
            .SetUINT32(&MF_MT_MPEG2_PROFILE, self.params.profile)
            .map_err(|e| RecorderError::MFError(format!("设置 H264 Profile 失败: {}", e)))?;

        let level = if self.params.width >= 3840 {
            52
        } else if self.params.width >= 1920 {
            40
        } else {
            30
        };
        media_type
            .SetUINT32(&MF_MT_MPEG2_LEVEL, level)
            .map_err(|e| RecorderError::MFError(format!("设置 H264 Level 失败: {}", e)))?;

        Ok(media_type)
    }

    /// 获取 MFT 的输入流 ID
    unsafe fn get_input_stream_id(&self, transform: &IMFTransform) -> Result<u32, RecorderError> {
        let mut input_ids = [0u32; 1];
        let mut output_ids = [0u32; 1];
        match transform.GetStreamIDs(&mut input_ids, &mut output_ids) {
            Ok(_) => Ok(input_ids[0]),
            Err(e) if e.code().0 == 0x80004001u32 as i32 => Ok(0),
            Err(e) => Err(RecorderError::MFError(format!("获取输入流 ID 失败: {}", e))),
        }
    }

    /// 获取 MFT 的输出流 ID
    unsafe fn get_output_stream_id(&self, transform: &IMFTransform) -> Result<u32, RecorderError> {
        let mut input_ids = [0u32; 1];
        let mut output_ids = [0u32; 1];
        match transform.GetStreamIDs(&mut input_ids, &mut output_ids) {
            Ok(_) => Ok(output_ids[0]),
            Err(e) if e.code().0 == 0x80004001u32 as i32 => Ok(0),
            Err(e) => Err(RecorderError::MFError(format!("获取输出流 ID 失败: {}", e))),
        }
    }

    /// 从 BGRA 数据创建 IMFSample
    unsafe fn create_bgra_sample(&self, bgra_data: &[u8]) -> Result<IMFSample, RecorderError> {
        let sample = MFCreateSample()
            .map_err(|e| RecorderError::MFError(format!("创建 IMFSample 失败: {}", e)))?;

        let timestamp = self.frame_count as i64 * self.frame_duration;
        sample
            .SetSampleTime(timestamp)
            .map_err(|e| RecorderError::MFError(format!("设置样本时间失败: {}", e)))?;

        sample
            .SetSampleDuration(self.frame_duration)
            .map_err(|e| RecorderError::MFError(format!("设置样本持续时间失败: {}", e)))?;

        let buffer_size = bgra_data.len() as u32;
        let buffer = MFCreateMemoryBuffer(buffer_size)
            .map_err(|e| RecorderError::MFError(format!("创建内存缓冲区失败: {}", e)))?;

        let mut data_ptr: *mut u8 = ptr::null_mut();
        let mut max_length = 0u32;
        let mut current_length = 0u32;
        buffer
            .Lock(
                &mut data_ptr,
                Some(&mut max_length),
                Some(&mut current_length),
            )
            .map_err(|e| RecorderError::MFError(format!("锁定缓冲区失败: {}", e)))?;

        ptr::copy_nonoverlapping(bgra_data.as_ptr(), data_ptr, bgra_data.len());

        buffer
            .SetCurrentLength(buffer_size)
            .map_err(|e| RecorderError::MFError(format!("设置缓冲区长度失败: {}", e)))?;

        buffer
            .Unlock()
            .map_err(|e| RecorderError::MFError(format!("解锁缓冲区失败: {}", e)))?;

        sample
            .AddBuffer(&buffer)
            .map_err(|e| RecorderError::MFError(format!("添加 Buffer 到 Sample 失败: {}", e)))?;

        Ok(sample)
    }

    /// 处理 H264 编码器的输出
    unsafe fn process_encoder_output(&mut self) -> Result<Vec<EncodedFrame>, RecorderError> {
        let encoder = self
            .h264_encoder
            .as_ref()
            .ok_or(RecorderError::NotRecording)?;

        let mut encoded_frames = Vec::new();

        let max_output_size = (self.params.bitrate / self.params.fps * 2) as u32;
        let output_buffer_size = max_output_size.max(1024 * 1024);

        loop {
            let output_buffer = MFCreateMemoryBuffer(output_buffer_size)
                .map_err(|e| RecorderError::MFError(format!("创建 H264 输出缓冲区失败: {}", e)))?;

            let output_sample = MFCreateSample().map_err(|e| {
                RecorderError::MFError(format!("创建 H264 输出 Sample 失败: {}", e))
            })?;

            output_sample.AddBuffer(&output_buffer).map_err(|e| {
                RecorderError::MFError(format!("添加 H264 输出 Buffer 失败: {}", e))
            })?;

            let output_data_buffer = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: self.encoder_output_id,
                pEvents: ManuallyDrop::new(None),
                pSample: ManuallyDrop::new(Some(output_sample.clone())),
                dwStatus: 0,
            };

            let mut process_status = 0u32;
            let mut output_buffers = [output_data_buffer];

            let result = encoder.ProcessOutput(0, &mut output_buffers, &mut process_status);

            match result {
                Ok(_) => {
                    if let Some(sample) = output_buffers[0].pSample.as_ref() {
                        let frame_data = self.extract_sample_data(sample)?;
                        if !frame_data.is_empty() {
                            let nal_units = extract_nal_units(&frame_data);
                            for nal_data in nal_units {
                                let mut annex_b = vec![0x00, 0x00, 0x00, 0x01];
                                annex_b.extend_from_slice(&nal_data);

                                let frame_type = Self::detect_frame_type(&annex_b);

                                match frame_type {
                                    FrameType::SPS => {
                                        if self.sps.is_empty() {
                                            self.sps = annex_b.clone();
                                        }
                                    }
                                    FrameType::PPS => {
                                        if self.pps.is_empty() {
                                            self.pps = annex_b.clone();
                                        }
                                    }
                                    _ => {}
                                }

                                encoded_frames.push(EncodedFrame {
                                    frame_type,
                                    data: annex_b,
                                });
                            }
                        }
                    }
                    continue;
                }
                Err(e) => {
                    let hr = e.code().0 as u32;
                    if hr == 0xC00D6D72 {
                        break;
                    }
                    return Err(RecorderError::MFError(format!(
                        "H264 编码器 ProcessOutput 失败: {}",
                        e
                    )));
                }
            }
        }

        Ok(encoded_frames)
    }

    /// 从 IMFSample 提取原始数据
    unsafe fn extract_sample_data(&self, sample: &IMFSample) -> Result<Vec<u8>, RecorderError> {
        let buffer_count = sample
            .GetBufferCount()
            .map_err(|e| RecorderError::MFError(format!("获取 Buffer 数量失败: {}", e)))?;

        let mut total_data = Vec::new();

        for i in 0..buffer_count {
            let buffer = sample
                .GetBufferByIndex(i)
                .map_err(|e| RecorderError::MFError(format!("获取 Buffer[{}] 失败: {}", i, e)))?;

            let mut data_ptr: *mut u8 = ptr::null_mut();
            let mut max_length = 0u32;
            let mut current_length = 0u32;

            buffer
                .Lock(
                    &mut data_ptr,
                    Some(&mut max_length),
                    Some(&mut current_length),
                )
                .map_err(|e| RecorderError::MFError(format!("锁定输出 Buffer 失败: {}", e)))?;

            if current_length > 0 && !data_ptr.is_null() {
                let data_slice = std::slice::from_raw_parts(data_ptr, current_length as usize);
                total_data.extend_from_slice(data_slice);
            }

            buffer
                .Unlock()
                .map_err(|e| RecorderError::MFError(format!("解锁输出 Buffer 失败: {}", e)))?;
        }

        Ok(total_data)
    }

    /// 从编码器属性中提取 SPS/PPS
    unsafe fn extract_sps_pps_from_attributes(&mut self) -> Result<(), RecorderError> {
        let encoder = match self.h264_encoder.as_ref() {
            Some(e) => e,
            None => return Ok(()),
        };

        let output_type = match encoder.GetOutputAvailableType(self.encoder_output_id, 0) {
            Ok(t) => t,
            Err(_) => return Ok(()),
        };

        let result = output_type.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER);

        if let Ok(header_length) = result {
            if header_length == 0 {
                return Ok(());
            }
            let mut header_data = vec![0u8; header_length as usize];
            let mut actual_length = 0u32;
            let get_result = output_type.GetBlob(
                &MF_MT_MPEG_SEQUENCE_HEADER,
                &mut header_data,
                Some(&mut actual_length as *mut u32),
            );

            if get_result.is_ok() && actual_length > 0 {
                header_data.truncate(actual_length as usize);

                if header_data.len() > 4 {
                    self.parse_mpeg_sequence_header(&header_data);
                }
            }
        }

        Ok(())
    }

    /// 解析 MPEG Sequence Header 格式的 SPS/PPS 数据
    fn parse_mpeg_sequence_header(&mut self, data: &[u8]) {
        if data.len() < 4 {
            return;
        }

        let mut offset = 0;

        if offset + 2 <= data.len() {
            let sps_len = ((data[offset] as usize) << 8) | (data[offset + 1] as usize);
            offset += 2;

            if offset + sps_len <= data.len() && sps_len > 0 {
                let mut sps = vec![0x00, 0x00, 0x00, 0x01];
                sps.extend_from_slice(&data[offset..offset + sps_len]);

                if self.sps.is_empty() {
                    self.sps = sps;
                    println!("[H264Encoder] 从属性中提取 SPS: {} 字节", sps_len);
                }
                offset += sps_len;
            }
        }

        if offset + 2 <= data.len() {
            let pps_len = ((data[offset] as usize) << 8) | (data[offset + 1] as usize);
            offset += 2;

            if offset + pps_len <= data.len() && pps_len > 0 {
                let mut pps = vec![0x00, 0x00, 0x00, 0x01];
                pps.extend_from_slice(&data[offset..offset + pps_len]);

                if self.pps.is_empty() {
                    self.pps = pps;
                    println!("[H264Encoder] 从属性中提取 PPS: {} 字节", pps_len);
                }
            }
        }
    }

    /// 检测 NAL 单元的帧类型
    fn detect_frame_type(data: &[u8]) -> FrameType {
        if let Some(nal_type) = get_nal_type(data) {
            match nal_type {
                5 => FrameType::IDR,
                7 => FrameType::SPS,
                8 => FrameType::PPS,
                1..=4 => FrameType::PFrame,
                _ => FrameType::Unknown,
            }
        } else {
            FrameType::Unknown
        }
    }
}

/// Python 绑定方法
#[pymethods]
impl H264Encoder {
    /// 创建 H264 编码器
    #[new]
    #[pyo3(signature = (fps=10, bitrate=2000000, monitor=1, profile=66))]
    pub fn new(fps: u32, bitrate: u32, monitor: u32, profile: u32) -> Result<Self, RecorderError> {
        use crate::d3d11::D3D11TextureManager;

        let (width, height) = D3D11TextureManager::detect_monitor(monitor)?;
        println!(
            "[H264Encoder] Detected monitor {}: {}x{}",
            monitor, width, height
        );

        let params = H264EncodeParams {
            width,
            height,
            fps,
            bitrate,
            profile,
        };
        Self::from_params(params)
    }

    /// 启动编码器，返回初始化信息（SPS/PPS）
    pub fn start(&mut self) -> Result<Py<PyDict>, RecorderError> {
        let _ = self.start_encoding()?;

        Ok(pyo3::Python::with_gil(|py| {
            let dict = PyDict::new_bound(py);
            dict.set_item("width", self.params.width)?;
            dict.set_item("height", self.params.height)?;
            dict.set_item("fps", self.params.fps)?;
            dict.set_item("sps", self.sps.clone())?;
            dict.set_item("pps", self.pps.clone())?;
            Ok::<_, pyo3::PyErr>(dict.into())
        })?)
    }

    /// 编码单帧（推流专用）
    ///
    /// 返回编码后的数据，带帧类型前缀：
    /// - 0x01 = SPS/PPS
    /// - 0x02 = IDR (关键帧)
    /// - 0x03 = P (预测帧)
    pub fn encode_frame(&mut self, frame_data: &[u8]) -> Result<Option<Vec<u8>>, RecorderError> {
        let frames = self.encode_frame_data(frame_data)?;

        if frames.is_empty() {
            return Ok(None);
        }

        let mut result = Vec::new();
        for frame in frames {
            let prefix = match frame.frame_type {
                FrameType::SPS | FrameType::PPS => 0x01,
                FrameType::IDR => 0x02,
                FrameType::PFrame | FrameType::Unknown => 0x03,
            };
            result.push(prefix);
            result.extend_from_slice(&frame.data);
        }

        Ok(Some(result))
    }

    /// 停止编码器
    pub fn stop(&mut self) -> Result<(), RecorderError> {
        self.stop_encoding()?;
        Ok(())
    }

    #[getter]
    pub fn width(&self) -> u32 {
        self.params.width
    }

    #[getter]
    pub fn height(&self) -> u32 {
        self.params.height
    }

    #[getter]
    pub fn fps(&self) -> u32 {
        self.params.fps
    }

    #[getter]
    pub fn is_encoding(&self) -> bool {
        self.initialized
    }

    #[getter]
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    #[getter]
    pub fn get_sps(&self) -> Vec<u8> {
        self.sps.clone()
    }

    #[getter]
    pub fn get_pps(&self) -> Vec<u8> {
        self.pps.clone()
    }
}

impl Drop for H264Encoder {
    fn drop(&mut self) {
        if self.initialized {
            self.h264_encoder = None;
            unsafe {
                let _ = MFShutdown();
                if self.com_initialized {
                    CoUninitialize();
                    self.com_initialized = false;
                }
            }
        }
    }
}
