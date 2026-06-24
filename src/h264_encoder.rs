//! 基于 IMFTransform 的 H.264 编码器
//!
//! 直接使用 Media Foundation Transform (MFT) 接口进行 H264 编码，
//! 不依赖 MFSinkWriter，可以直接在内存中获取编码后的 NAL 单元数据。
//!
//! # 架构
//! - 输入: NV12 格式的帧数据（由调用方传入的 BGRA 经 CPU 转换得到）
//! - 编码: H264 编码器 MFT
//! - 输出: Annex-B 格式的 H264 码流
//!
//! # 说明
//! 裸的 H264 MFT 不接受 RGB32 直连（SetInputType 会返回 MF_E_INVALIDMEDIATYPE
//! / 0xC00D36B4，部分系统会返回 0xC00D6D60），因此在喂帧前先用
//! `bgra_to_nv12` 做 CPU 端的颜色空间转换，再把 NV12 数据送入编码器。
//! 这与 SinkWriter 内部自动插入颜色转换器的效果一致，但更通用。

use crate::bgra_to_nv12::bgra_to_nv12;
use crate::error::RecorderError;
use crate::memory_byte_stream::{extract_nal_units, get_nal_type};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
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

/// 基于 IMFTransform 的 H.264 编码器
///
/// 输入为 NV12（由调用方传入的 BGRA 经 CPU 转换得到）。详见模块顶部说明。
#[pyclass]
pub struct H264Encoder {
    h264_encoder: Option<IMFTransform>,
    params: H264EncodeParams,
    /// 调用方传入帧的实际宽高（未对齐，例如 1920x1080）
    input_width: u32,
    input_height: u32,
    initialized: bool,
    com_initialized: bool,
    frame_duration: i64,
    frame_count: u64,
    sps: Vec<u8>,
    pps: Vec<u8>,
    encoder_input_id: u32,
    encoder_output_id: u32,
    /// IDR 关键帧间隔（帧数），0 表示不强制 IDR
    idr_interval: u32,
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

        // 保存实际输入尺寸（用于校验 BGRA 数据大小 / 做行对齐填充）
        let input_width = params.width;
        let input_height = params.height;

        // 编码器要求分辨率对齐到 16 的倍数
        let aligned_width = (params.width + 15) & !15;
        let aligned_height = (params.height + 15) & !15;
        let frame_duration = 10_000_000_i64 / params.fps as i64;

        let mut params = params;
        params.width = aligned_width;
        params.height = aligned_height;

        Ok(Self {
            h264_encoder: None,
            params,
            input_width,
            input_height,
            initialized: false,
            com_initialized: false,
            frame_duration,
            frame_count: 0,
            sps: Vec::new(),
            pps: Vec::new(),
            encoder_input_id: 0,
            encoder_output_id: 0,
            idr_interval: 30, // 默认每 30 帧产生一个 IDR 关键帧
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

            // 配置管线：NV12 输入 -> H264 编码器
            self.configure_pipeline()?;

            self.send_stream_messages()?;

            self.initialized = true;

            // 送入一帧黑帧，触发编码器生成 SPS/PPS（裸 MFT 在 ProcessInput
            // 之后才会在输出属性中暴露 MPEG_SEQUENCE_HEADER）。
            // 这帧的 IDR 也会被丢弃，因为 start() 之前还没人接收初始码流，
            // 但 SPS/PPS 会保留下来，供后续 encode_frame 使用。
            self.prime_encoder_with_black_frame()?;

            // 现在可以从输出属性中提取 SPS/PPS
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
    ///
    /// 入参可以是：
    /// - 实际显示器尺寸（`input_width` x `input_height`）的 BGRA 数据，内部会自动补齐到对齐尺寸
    /// - 或者直接对齐后的尺寸（`params.width` x `params.height`），省去补齐步骤
    pub fn encode_frame_data(
        &mut self,
        bgra_data: &[u8],
    ) -> Result<Vec<EncodedFrame>, RecorderError> {
        if !self.initialized {
            return Err(RecorderError::NotRecording);
        }

        // 校验输入帧尺寸：支持对齐或非对齐两种尺寸
        let aligned_size = (self.params.width * self.params.height * 4) as usize;
        let non_aligned_size = (self.input_width * self.input_height * 4) as usize;

        if bgra_data.len() != aligned_size && bgra_data.len() != non_aligned_size {
            return Err(RecorderError::FrameSizeMismatch {
                expected: aligned_size,
                actual: bgra_data.len(),
            });
        }

        // 将 BGRA 补齐到对齐后的尺寸（行优先复制，补齐的像素置黑）
        // 如果已经是对齐尺寸，直接使用，无需填充
        let aligned_bgra = if bgra_data.len() == aligned_size {
            // 已经是对齐尺寸，直接使用
            Vec::from(bgra_data)
        } else {
            // 非对齐尺寸，需要填充到对齐尺寸
            self.pad_bgra_to_aligned(bgra_data)
        };

        // BGRA -> NV12（CPU 转换）
        let nv12 = bgra_to_nv12(&aligned_bgra, self.params.width, self.params.height);

        unsafe {
            let encoder = self
                .h264_encoder
                .as_ref()
                .ok_or(RecorderError::NotRecording)?;

            // 直接送入帧，IDR 由编码器根据 GOP 大小自动生成
            //（通过 ICodecAPI::CODECAPI_AVEncMPVGOPSize 在 configure_pipeline 中设置）
            let input_sample = self.create_nv12_sample(&nv12)?;
            encoder
                .ProcessInput(self.encoder_input_id, &input_sample, 0)
                .map_err(|e| {
                    RecorderError::MFError(format!("H264 编码器 ProcessInput 失败: {}", e))
                })?;

            // H.264 MFT 有流水线延迟，可能需要多帧输入才有���出
            // 这里尝试最多 3 次取输出，每次 ProcessOutput 失败就继续送入下一帧
            let mut encoded_frames = Vec::new();
            for _ in 0..3 {
                match self.process_encoder_output() {
                    Ok(frames) => {
                        if !frames.is_empty() {
                            encoded_frames = frames;
                            break;
                        }
                    }
                    Err(e) => {
                        // 如果是 NEED_MORE_INPUT，继续尝试
                        let hr = e.to_string();
                        if !hr.contains("0xC00D6D72") {
                            return Err(e);
                        }
                    }
                }
            }

            if self.sps.is_empty() || self.pps.is_empty() {
                self.extract_sps_pps_from_attributes()?;
            }

            for frame in &mut encoded_frames {
                if frame.frame_type == FrameType::Unknown {
                    frame.frame_type = Self::detect_frame_type(&frame.data);
                }
            }

            self.frame_count += 1;

            // 调试输出：每 30 帧打印编码统计
            if self.frame_count % 30 == 0 {
                println!(
                    "[H264Encoder] 已编码 {} 帧，当前帧产生 {} 个 NAL 单元",
                    self.frame_count,
                    encoded_frames.len()
                );
                for (i, f) in encoded_frames.iter().enumerate() {
                    println!("[H264Encoder]   NAL[{}]: type={:?}, size={}", i, f.frame_type, f.data.len());
                }
            }

            Ok(encoded_frames)
        }
    }

    /// 送入一帧黑帧以触发编码器生成 SPS/PPS。
    ///
    /// 裸 H.264 MFT 在收到第一帧之前不会在输出属性中暴露 SPS/PPS。
    /// 本方法生成一帧全黑 NV12，喂给编码器，然后丢弃输出的 IDR 帧。
    /// 调用后，sps/pps 字段将被填充，供后续帧使用。
    unsafe fn prime_encoder_with_black_frame(&mut self) -> Result<(), RecorderError> {
        let w = self.params.width as usize;
        let h = self.params.height as usize;
        // NV12: Y 平面 (w*h) + UV 平面 (w*h/2)
        let y_size = w * h;
        let uv_size = w * h / 2;
        let mut black_nv12 = vec![0u8; y_size + uv_size];
        // Y 平面全 0 = 黑色；UV 平面填 128（NV12 的中性 UV 值）
        for i in y_size..y_size + uv_size {
            black_nv12[i] = 128;
        }

        let encoder = self
            .h264_encoder
            .as_ref()
            .ok_or(RecorderError::NotRecording)?;

        let input_sample = self.create_nv12_sample(&black_nv12)?;
        encoder
            .ProcessInput(self.encoder_input_id, &input_sample, 0)
            .map_err(|e| RecorderError::MFError(format!("启动帧 ProcessInput 失败: {}", e)))?;

        // 取出输出（IDR 帧），不需要保留
        let _ = self.process_encoder_output();

        println!("[H264Encoder] 启动帧已处理，SPS/PPS 已提取");
        Ok(())
    }

    /// 将实际尺寸的 BGRA 数据补齐到对齐尺寸（不足的行/列填黑色）。
    fn pad_bgra_to_aligned(&self, bgra_data: &[u8]) -> Vec<u8> {
        let in_w = self.input_width as usize;
        let in_h = self.input_height as usize;
        let out_w = self.params.width as usize;
        let out_h = self.params.height as usize;

        let mut padded = vec![0u8; out_w * out_h * 4];
        for y in 0..in_h {
            let src_start = y * in_w * 4;
            let dst_start = y * out_w * 4;
            let copy_len = in_w * 4;
            padded[dst_start..dst_start + copy_len]
                .copy_from_slice(&bgra_data[src_start..src_start + copy_len]);
        }
        padded
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

    /// 配置 MFT 管线：NV12 输入 -> H264 输出
    ///
    /// 裸 H264 MFT 不接受 RGB32 直连，因此统一使用 NV12 作为输入类型。
    /// BGRA->NV12 的颜色转换在 `encode_frame_data` 中由 CPU 完成。
    unsafe fn configure_pipeline(&mut self) -> Result<(), RecorderError> {
        let h264_encoder = self
            .h264_encoder
            .as_ref()
            .ok_or_else(|| RecorderError::MFError("H264 编码器未创建".into()))?;

        // 获取流 ID
        self.encoder_input_id = self.get_input_stream_id(h264_encoder)?;
        self.encoder_output_id = self.get_output_stream_id(h264_encoder)?;

        // 对裸 IMFTransform，必须先确定输出类型（H264），编码器才能协商出可接受的输入格式。
        // 1. 先设置 H264 输出类型
        let h264_type = self.create_h264_media_type()?;
        h264_encoder
            .SetOutputType(self.encoder_output_id, &h264_type, 0)
            .map_err(|e| RecorderError::MFError(format!("设置 H264 输出类型失败: {}", e)))?;

        // 2. 再设置 NV12 输入类型
        //    裸 H264 MFT 不接受 RGB32 直连，统一使用 NV12 输入。
        let nv12_type = self.create_nv12_media_type()?;
        h264_encoder
            .SetInputType(self.encoder_input_id, &nv12_type, 0)
            .map_err(|e| RecorderError::MFError(format!("设置 NV12 输入类型失败: {}", e)))?;

        // 3. 设置 GOP 大小（IDR 间隔），通过 ICodecAPI 接口
        self.set_gop_size(h264_encoder)?;

        Ok(())
    }

    /// 通过 ICodecAPI 设置 GOP 大小，控制 IDR 关键帧间隔
    #[allow(unused_variables)]
    unsafe fn set_gop_size(&self, encoder: &IMFTransform) -> Result<(), RecorderError> {
        use windows::core::Interface;
        use windows::Win32::System::Variant::InitVariantFromUInt32Array;

        // 查询 ICodecAPI 接口
        let codec_api: ICodecAPI = match encoder.cast() {
            Ok(api) => api,
            Err(e) => {
                println!("[H264Encoder] 查询 ICodecAPI 失败: {}", e);
                return Ok(()); // 跳过，不影响编码器启动
            }
        };

        // CODECAPI_AVEncMPVGOPSize 的 GUID：设置 GOP 大小（I 帧间隔）
        let gop_guid = GUID::from_u128(0x95f31b26_95a4_41aa_9303_246a7fc6eef1);

        // 检查是否支持此参数
        if codec_api.IsSupported(&gop_guid).is_err() {
            // 不支持 GOP 大小设置，跳过（某些编码器可能不支持）
            println!("[H264Encoder] 编码器不支持 CODECAPI_AVEncMPVGOPSize");
            return Ok(());
        }

        // 设置 GOP 大小：id 为 0 表示禁用手动 IDR，使用编码器默认值
        // idr_interval 为 0 时使用默认值 30
        let gop_size = if self.idr_interval > 0 { self.idr_interval } else { 30 };

        // 使用 InitVariantFromUInt32Array 创建 VT_UI4 类型的 VARIANT
        let variant = match InitVariantFromUInt32Array(&[gop_size]) {
            Ok(v) => v,
            Err(e) => {
                println!("[H264Encoder] 创建 VARIANT 失败: {}", e);
                return Ok(()); // 跳过，不影响编码器启动
            }
        };

        // 尝试设置 GOP 大小，失败时只打印警告不影响启动
        if let Err(e) = codec_api.SetValue(&gop_guid, &variant) {
            println!("[H264Encoder] 设置 GOP 大小失败（忽略）: {}", e);
            return Ok(());
        }

        println!("[H264Encoder] GOP 大小已设置为 {}", gop_size);
        Ok(())
    }

    /// 创建 NV12 输入媒体类型
    unsafe fn create_nv12_media_type(&self) -> Result<IMFMediaType, RecorderError> {
        let media_type = MFCreateMediaType()
            .map_err(|e| RecorderError::MFError(format!("创建 NV12 MediaType 失败: {}", e)))?;

        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| RecorderError::MFError(format!("设置主类型失败: {}", e)))?;

        media_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
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

        // NV12 的默认 stride = width（Y 平面每行字节数）。
        // 这里按对齐后的宽度设置，以与 bgra_to_nv12 输出的紧凑布局一致。
        let stride = aligned_width as u32;
        media_type
            .SetUINT32(&MF_MT_DEFAULT_STRIDE, stride)
            .map_err(|e| RecorderError::MFError(format!("设置 NV12 stride 失败: {}", e)))?;

        // 注意：H.264 编码器 MFT 的输出只支持 limited range (16-235)，
        // 设置 full range 会导致 "Input and output nominal range mismatch" 错误。
        // 因此这里不再设置 MF_MT_VIDEO_NOMINAL_RANGE，让编码器使用默认的 limited range。
        // 浏览器按 limited range 解码，画面会略显发灰，但至少能正常工作。

        // BT.601 矩阵：保持 YUV 转换色彩一致性
        media_type
            .SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT601.0 as u32)
            .map_err(|e| RecorderError::MFError(format!("设置 YUV 矩阵失败: {}", e)))?;

        // 色彩原色 / 传输函数：SDR sRGB 近似，对应 BT.709 原色 + BT.601 矩阵的常见组合。
        // 这三项确保浏览器正确还原色彩空间。
        media_type
            .SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)
            .map_err(|e| RecorderError::MFError(format!("设置色彩原色失败: {}", e)))?;

        media_type
            .SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)
            .map_err(|e| RecorderError::MFError(format!("设置传输函数失败: {}", e)))?;

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

    /// 从 NV12 数据创建 IMFSample
    unsafe fn create_nv12_sample(&self, nv12_data: &[u8]) -> Result<IMFSample, RecorderError> {
        let sample = MFCreateSample()
            .map_err(|e| RecorderError::MFError(format!("创建 IMFSample 失败: {}", e)))?;

        let timestamp = self.frame_count as i64 * self.frame_duration;
        sample
            .SetSampleTime(timestamp)
            .map_err(|e| RecorderError::MFError(format!("设置样本时间失败: {}", e)))?;

        sample
            .SetSampleDuration(self.frame_duration)
            .map_err(|e| RecorderError::MFError(format!("设置样本持续时间失败: {}", e)))?;

        // 注意：Windows H.264 MFT 不支持通过 MFSAMPLE_EXTENSION_CLEAN_POINT 强制 IDR
        // IDR 间隔通过 ICodecAPI::CODECAPI_AVEncMPVGOPSize 在 configure_pipeline 中设置

        let buffer_size = nv12_data.len() as u32;
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

        ptr::copy_nonoverlapping(nv12_data.as_ptr(), data_ptr, nv12_data.len());

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
    ///
    /// 取走编码器当前可用的所有输出帧。
    /// 返回空 Vec 表示编码器暂时没有输出（流水线延迟，属正常）。
    unsafe fn process_encoder_output(&mut self) -> Result<Vec<EncodedFrame>, RecorderError> {
        let encoder = self
            .h264_encoder
            .as_ref()
            .ok_or(RecorderError::NotRecording)?;

        let mut encoded_frames = Vec::new();

        // 查询输出流属性，判断编码器是否自己提供 sample。
        // H.264 MFT 默认设置 MFT_OUTPUT_STREAM_PROVIDES_SAMPLES，
        // 此时调用方必须把 pSample 置为 NULL，由编码器分配输出 sample；
        // 否则编码器不会写入任何数据，表现为 ProcessOutput 返回 NEED_MORE_INPUT。
        let stream_info = encoder.GetOutputStreamInfo(self.encoder_output_id).map_err(|e| {
            RecorderError::MFError(format!("GetOutputStreamInfo 失败: {}", e))
        })?;
        let provides_samples =
            (stream_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;

        // 仅在不提供 sample 的模式下才需要预分配缓冲区
        let alloc_sample = !provides_samples;
        let uncompressed_size = (self.params.width * self.params.height * 4) as u32;
        let output_buffer_size = uncompressed_size.max(1024 * 1024);

        loop {
            // 构建 MFT_OUTPUT_DATA_BUFFER
            // provides_samples=true 时 pSample 必须为 None
            let preallocated_sample: Option<IMFSample> = if alloc_sample {
                let output_buffer = MFCreateMemoryBuffer(output_buffer_size).map_err(|e| {
                    RecorderError::MFError(format!("创建 H264 输出缓冲区失败: {}", e))
                })?;
                let output_sample = MFCreateSample().map_err(|e| {
                    RecorderError::MFError(format!("创建 H264 输出 Sample 失败: {}", e))
                })?;
                output_sample.AddBuffer(&output_buffer).map_err(|e| {
                    RecorderError::MFError(format!("添加 H264 输出 Buffer 失败: {}", e))
                })?;
                Some(output_sample)
            } else {
                None
            };

            let output_data_buffer = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: self.encoder_output_id,
                pEvents: ManuallyDrop::new(None),
                pSample: ManuallyDrop::new(preallocated_sample),
                dwStatus: 0,
            };

            let mut process_status = 0u32;
            let mut output_buffers = [output_data_buffer];

            let result = encoder.ProcessOutput(0, &mut output_buffers, &mut process_status);

            // 不论结果如何，都要释放 pSample（编码器可能把它分配的 sample 放进来）。
            let p_sample = std::mem::take(&mut output_buffers[0].pSample);

            match result {
                Ok(_) => {
                    // 输出 sample 可能在 pSample 中（无论是否预分配）
                    let out_sample = p_sample.as_ref();
                    if let Some(sample) = out_sample {
                        let frame_data = self.extract_sample_data(sample)?;
                        if !frame_data.is_empty() {
                            for nal_data in extract_nal_units(&frame_data) {
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
                    // 释放 sample
                    if let Some(sample) = ManuallyDrop::into_inner(p_sample) {
                        let _ = sample;
                    }
                    // 如果没有更多输出，退出循环
                    if encoded_frames.is_empty() {
                        break;
                    }
                    continue;
                }
                Err(e) => {
                    // 释放可能残留的 sample
                    if let Some(sample) = ManuallyDrop::into_inner(p_sample) {
                        let _ = sample;
                    }
                    let hr = e.code().0 as u32;
                    if hr == 0xC00D6D72 {
                        // MF_E_TRANSFORM_NEED_MORE_INPUT - 暂无新输出，退出循环
                        // 但如果有已提取的帧（SPS/PPS），仍然返回
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

    /// 从编码器当前输出类型中提取 SPS/PPS
    ///
    /// 注意：必须使用 GetOutputCurrentType 获取当前已设置的输出类型，
    /// 而不是 GetOutputAvailableType（它只返回支持的类型列表）。
    /// MF_MT_MPEG_SEQUENCE_HEADER 属性仅在当前输出类型上可用。
    ///
    /// 如果属性中没有完整的 SPS/PPS，会返回空，调用方应该依赖
    /// process_encoder_output 中的提取逻辑（基于实际 NAL 单元）。
    unsafe fn extract_sps_pps_from_attributes(&mut self) -> Result<(), RecorderError> {
        let encoder = match self.h264_encoder.as_ref() {
            Some(e) => e,
            None => return Ok(()),
        };

        // 如果已经在 process_encoder_output 中提取到了 SPS/PPS，跳过
        if !self.sps.is_empty() && !self.pps.is_empty() {
            return Ok(());
        }

        // 获取当前已设置的输出类型（不是可用类型列表）
        let output_type = match encoder.GetOutputCurrentType(self.encoder_output_id) {
            Ok(t) => t,
            Err(_) => {
                // 当前类型未设置，尝试从可用类型中获取第一个
                match encoder.GetOutputAvailableType(self.encoder_output_id, 0) {
                    Ok(t) => t,
                    Err(_) => return Ok(()),
                }
            }
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

                // 调试输出：打印原始数据
                if header_data.len() >= 4 {
                    println!(
                        "[H264Encoder] MPEG_SEQUENCE_HEADER 原始数据: {:?}",
                        &header_data[..header_data.len().min(20)]
                    );
                }

                if header_data.len() > 4 {
                    self.parse_mpeg_sequence_header(&header_data);
                }
            }
        }

        // 如果属性中没有提取到 SPS/PPS，打印调试信息
        if self.sps.is_empty() {
            println!("[H264Encoder] 属性中未找到 SPS");
        }
        if self.pps.is_empty() {
            println!("[H264Encoder] 属性中未找到 PPS");
        }

        Ok(())
    }

    /// 解析 MPEG Sequence Header 格式的 SPS/PPS 数据
    ///
    /// MF_MT_MPEG_SEQUENCE_HEADER 可能包含两种格式：
    /// 1. 长度前缀格式: [2字节 SPS 长度][N字节 SPS 数据][2字节 PPS 长度][M字节 PPS 数据]
    /// 2. ANNEX-B 格式: [起始码 0x00000001][SPS NAL][起始码][PPS NAL]
    ///
    /// 注意：Microsoft H.264 编码器通常返回 ANNEX-B 格式。
    fn parse_mpeg_sequence_header(&mut self, data: &[u8]) {
        if data.len() < 4 {
            println!(
                "[H264Encoder] MPEG_SEQUENCE_HEADER 数据太短: {} 字节",
                data.len()
            );
            return;
        }

        // 检查是否是 ANNEX-B 格式（以 0x00000001 或 0x000001 开头）
        let is_annex_b = (data.len() >= 4 && data[0] == 0 && data[1] == 0 && data[2] == 0 && data[3] == 1)
            || (data.len() >= 3 && data[0] == 0 && data[1] == 0 && data[2] == 1);

        if is_annex_b {
            println!("[H264Encoder] 检测到 ANNEX-B 格式");
            self.parse_annex_b_header(data);
        } else {
            println!("[H264Encoder] 检测到长度前缀格式");
            self.parse_length_prefixed_header(data);
        }
    }

    /// 解析 ANNEX-B 格式的 SPS/PPS
    fn parse_annex_b_header(&mut self, data: &[u8]) {
        // 提取所有 NAL 单元
        let mut offset = 0;
        while offset + 4 <= data.len() {
            // 跳过起始码
            let mut skip = 0;
            while offset + skip < data.len() && data[offset + skip] == 0x00 {
                skip += 1;
            }
            if offset + skip < data.len() && data[offset + skip] == 0x01 {
                skip += 1;
                offset += skip;
            } else {
                break;
            }

            if offset >= data.len() {
                break;
            }

            // 获取 NAL type
            let nal_type = data[offset] & 0x1F;
            println!("[H264Encoder] ANNEX-B NAL type: {}", nal_type);

            // 找到 NAL 单元的结束位置（下一个起始码或数据末尾）
            let mut end = offset + 1;
            while end + 2 <= data.len() {
                // 检查是否是起始码模式 (0x000001 或 0x00000001)
                if data[end] == 0x00 && data[end + 1] == 0x00 {
                    if (data[end + 2] == 0x01) || (end + 3 <= data.len() && data[end + 2] == 0x00 && data[end + 3] == 0x01) {
                        break;
                    }
                }
                end += 1;
            }

            let nal_data = &data[offset..end];

            match nal_type {
                7 => { // SPS
                    if self.sps.is_empty() {
                        let mut sps = vec![0x00, 0x00, 0x00, 0x01];
                        sps.extend_from_slice(nal_data);
                        self.sps = sps;
                        println!("[H264Encoder] 从 ANNEX-B 提取 SPS: {} 字节", nal_data.len());
                    }
                }
                8 => { // PPS
                    if self.pps.is_empty() {
                        let mut pps = vec![0x00, 0x00, 0x00, 0x01];
                        pps.extend_from_slice(nal_data);
                        self.pps = pps;
                        println!("[H264Encoder] 从 ANNEX-B 提取 PPS: {} 字节", nal_data.len());
                    }
                }
                _ => {}
            }

            offset = end;
        }

        // 调试输出
        if !self.sps.is_empty() {
            println!("[H264Encoder] 最终 SPS: {:?}",
                &self.sps[..self.sps.len().min(10)]);
        }
        if !self.pps.is_empty() {
            println!("[H264Encoder] 最终 PPS: {:?}",
                &self.pps[..self.pps.len().min(10)]);
        }
    }

    /// 解析长度前缀格式的 SPS/PPS
    fn parse_length_prefixed_header(&mut self, data: &[u8]) {
        let mut offset = 0;

        // 读取 SPS 长度
        if offset + 2 <= data.len() {
            let sps_len = ((data[offset] as usize) << 8) | (data[offset + 1] as usize);
            println!("[H264Encoder] 解析到 SPS 长度: {}", sps_len);
            offset += 2;

            if sps_len > 0 && offset + sps_len <= data.len() {
                let sps_nal = &data[offset..offset + sps_len];
                if !sps_nal.is_empty() {
                    let nal_type = sps_nal[0] & 0x1F;
                    println!("[H264Encoder] SPS NAL type: {}", nal_type);

                    if nal_type == 7 && self.sps.is_empty() {
                        let mut sps = vec![0x00, 0x00, 0x00, 0x01];
                        sps.extend_from_slice(sps_nal);
                        self.sps = sps;
                        println!("[H264Encoder] 从长度前缀格式提取 SPS: {} 字节", sps_len);
                    }
                }
                offset += sps_len;
            }
        }

        // 读取 PPS
        if offset + 2 <= data.len() {
            let pps_len = ((data[offset] as usize) << 8) | (data[offset + 1] as usize);
            println!("[H264Encoder] 解析到 PPS 长度: {}", pps_len);
            offset += 2;

            if pps_len > 0 && offset + pps_len <= data.len() {
                let pps_nal = &data[offset..offset + pps_len];
                if !pps_nal.is_empty() {
                    let nal_type = pps_nal[0] & 0x1F;
                    println!("[H264Encoder] PPS NAL type: {}", nal_type);

                    if nal_type == 8 && self.pps.is_empty() {
                        let mut pps = vec![0x00, 0x00, 0x00, 0x01];
                        pps.extend_from_slice(pps_nal);
                        self.pps = pps;
                        println!("[H264Encoder] 从长度前缀格式提取 PPS: {} 字节", pps_len);
                    }
                }
            }
        }
    }

    /// 检测 NAL 单元的帧类型
    fn detect_frame_type(data: &[u8]) -> FrameType {
        if let Some(nal_type) = get_nal_type(data) {
            match nal_type {
                5 => FrameType::IDR,       // IDR 关键帧
                7 => FrameType::SPS,       // Sequence Parameter Set
                8 => FrameType::PPS,       // Picture Parameter Set
                6 => FrameType::PFrame,   // SEI (Supplemental Enhancement Information)
                9 => FrameType::PFrame,   // AUD (Access Unit Delimiter) - 帧分隔符，当作 P 帧处理
                1..=4 => FrameType::PFrame, // P/B 帧
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

        Ok(Python::with_gil(|py| {
            let dict = PyDict::new_bound(py);
            dict.set_item("width", self.params.width)?;
            dict.set_item("height", self.params.height)?;
            dict.set_item("fps", self.params.fps)?;
            // 使用 PyBytes 确保 Python 侧得到 bytes 而非 list[int]
            dict.set_item("sps", PyBytes::new_bound(py, &self.sps))?;
            dict.set_item("pps", PyBytes::new_bound(py, &self.pps))?;
            Ok::<_, pyo3::PyErr>(dict.into())
        })?)
    }

    /// 编码单帧（推流专用）
    ///
    /// 返回编码后的数据，使用 ANNEX-B 格式（与 SPS/PPS 一致）：
    /// - 0x01 = SPS/PPS
    /// - 0x02 = IDR (关键帧)
    /// - 0x03 = P (预测帧)
    ///
    /// 返回格式：[1字节前缀][4字节起始码 0x00000001][NAL数据]
    pub fn encode_frame<'py>(&mut self, py: Python<'py>, frame_data: &[u8]) -> Result<Option<Bound<'py, PyBytes>>, RecorderError> {
        let frames = self.encode_frame_data(frame_data)?;

        if frames.is_empty() {
            // H.264 MFT 有流水线延迟，部分帧无输出属正常，下一轮继续取。
            return Ok(None);
        }

        // 方案一：动态提升 Prefix 级别
        // 1. 先探测当前这组 frames 里有没有 IDR 关键帧
        let has_idr = frames.iter().any(|f| matches!(f.frame_type, FrameType::IDR));

        // 2. 根据探测结果决定整包的最高优先级前缀
        let final_prefix = if has_idr {
            0x02 // 只要有 IDR，一律视为关键帧包
        } else {
            // 检查是否有 SPS/PPS，否则为普通 P 帧
            let has_sps_pps = frames.iter().any(|f| matches!(f.frame_type, FrameType::SPS | FrameType::PPS));
            if has_sps_pps { 0x01 } else { 0x03 }
        };

        // 3. 只写 1 个字节的前缀
        let mut result = vec![final_prefix];

        // 4. 写入实际的 H264 数据（直接拼接所有 NAL 原始数据）
        // 过滤掉 AUD (Access Unit Delimiter, nal_type=9) - 对 WebCodecs 解码无用
        for frame in &frames {
            // 检查是否是 AUD (nal_type = 9)
            if frame.data.len() > 4 {
                let nal_type = frame.data[4] & 0x1F;
                if nal_type == 9 {
                    continue; // 跳过 AUD
                }
            }
            result.extend_from_slice(&frame.data);
        }

        // 如果只有前缀没有数据，返回 None
        if result.len() <= 1 {
            return Ok(None);
        }

        Ok(Some(PyBytes::new_bound(py, &result)))
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
    pub fn get_sps(&self) -> Py<PyBytes> {
        Python::with_gil(|py| PyBytes::new_bound(py, &self.sps).into())
    }

    #[getter]
    pub fn get_pps(&self) -> Py<PyBytes> {
        Python::with_gil(|py| PyBytes::new_bound(py, &self.pps).into())
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
