//! 基于 IMFTransform 的 H.264 编码器
//!
//! 直接使用 Media Foundation Transform (MFT) 接口进行 H264 编码，
//! 不依赖 MFSinkWriter，可以直接在内存中获取编码后的 NAL 单元数据。
//!
//! # 架构
//! - 输入: RGB32 (BGRA) 格式的帧数据
//! - 颜色转换: 通过 MFT 颜色转换器 (RGB32 -> NV12)
//! - 编码: H264 编码器 MFT (NV12 -> H264)
//! - 输出: Annex-B 格式的 H264 码流
//!
//! # 注意
//! H264 编码器 MFT 通常不接受 RGB32 输入，只接受 NV12 或 YUV420 格式。
//! 因此需要在 RGB32 和 H264 编码器之间插入颜色转换 MFT。

use crate::bgra_to_nv12::{bgra_to_nv12, bgra_to_iyuv};
use crate::error::RecorderError;
use crate::memory_byte_stream::{extract_nal_units, get_nal_type};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::*;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::mem::ManuallyDrop;
use std::ptr;
use windows::core::GUID;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;

// MFT 消息：设置 D3D 管理器（让硬件编码器可以接受 D3D11 纹理输入）
const MFT_MESSAGE_SET_D3D_MANAGER: u32 = 0x1; // 实际值需要查文档

/// CMSH264EncoderMFT 的 CLSID
///
/// 这是 Windows 内置的 H.264 编码器 Media Foundation Transform
/// GUID: {6CA50344-051A-4DED-9779-A43305165E35}
/// H264 硬件编码器 CLSID
///
/// 这是 Windows 内置的硬件 H.264 编码器，需要 D3D11 支持
/// CLSID: {6CA50344-051A-4DED-9779-A43305165E35}
const CLSID_MSH264_ENCODER_MFT: GUID = GUID::from_values(
    0x6CA50344,
    0x051A,
    0x4DED,
    [0x97, 0x79, 0xA4, 0x33, 0x05, 0x16, 0x5E, 0x35],
);

/// H264 编码参数
#[derive(Debug, Clone)]
pub struct H264EncodeParams {
    /// 视频宽度（会对齐到 16 的倍数）
    pub width: u32,
    /// 视频高度（会对齐到 16 的倍数）
    pub height: u32,
    /// 帧率
    pub fps: u32,
    /// 码率 (bps)
    pub bitrate: u32,
    /// H264 Profile (66=Baseline, 77=Main, 100=High)
    pub profile: u32,
}

impl Default for H264EncodeParams {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 10,
            bitrate: 2_000_000,
            profile: 66, // Baseline
        }
    }
}

/// 编码帧类型
#[derive(Debug, Clone, PartialEq)]
pub enum FrameType {
    /// IDR 帧（关键帧）
    IDR,
    /// P 帧
    PFrame,
    /// SPS 参数集
    SPS,
    /// PPS 参数集
    PPS,
    /// 未知类型
    Unknown,
}

/// 编码后的帧数据
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    /// 帧类型
    pub frame_type: FrameType,
    /// Annex-B 格式的编码数据（包含起始码）
    pub data: Vec<u8>,
}

/// 基于 IMFTransform 的 H264 编码器
///
/// 使用两阶段 MFT 管线：
/// 1. 颜色转换 MFT: RGB32 -> NV12
/// 2. H264 编码 MFT: NV12 -> H264
///
/// 编码后的数据直接在内存中获取，无需临时文件。
#[pyclass]
pub struct H264Encoder {
    /// H264 编码 MFT
    h264_encoder: Option<IMFTransform>,
    /// 编码参数
    params: H264EncodeParams,
    /// 是否已初始化
    initialized: bool,
    /// 当前线程的 COM 是否由本编码器初始化
    com_initialized: bool,
    /// 帧持续时间（100ns 单位）
    frame_duration: i64,
    /// 帧计数
    frame_count: u64,
    /// SPS 数据
    sps: Vec<u8>,
    /// PPS 数据
    pps: Vec<u8>,
    /// 输入流 ID（H264 编码器）
    encoder_input_id: u32,
    /// 输出流 ID（H264 编码器）
    encoder_output_id: u32,
    /// DXGI Device Manager reset token (保留字段)
    dxgi_reset_token: u32,
    /// 是否使用 CPU 模式（当 D3D11 硬件加速不可用时）
    use_cpu_mode: bool,
}

// 手动实现 Send trait，因为 IMFTransform 是 COM 对象
unsafe impl Send for H264Encoder {}

impl H264Encoder {
    /// 创建 H264 编码器
    ///
    /// # 参数
    /// - params: 编码参数
    ///
    /// # 返回
    /// 成功返回 H264Encoder 实例（尚未初始化，需调用 start()）
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

        // 分辨率对齐：宽高必须是 16 的倍数（H264 编码器要求）
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
            dxgi_reset_token: 0,
            use_cpu_mode: false,
        })
    }

    /// 初始化并启动编码器
    ///
    /// 创建 MFT 管线：RGB32 -> [颜色转换] -> NV12 -> [H264编码] -> H264
    /// 启动后可以获取 SPS/PPS 参数集
    pub fn start_encoding(&mut self) -> Result<Vec<EncodedFrame>, RecorderError> {
        if self.initialized {
            return Err(RecorderError::AlreadyRecording);
        }

        let start_result = unsafe {
            // CoCreateInstance 需要当前线程先初始化 COM。
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

            // 启动 Media Foundation
            MFStartup(MFSTARTUP_LITE, 0)
                .map_err(|e| RecorderError::MFError(format!("MFStartup 失败: {}", e)))?;

            // 创建 H264 编码 MFT，使用 NV12 输入
            let h264_encoder = self.create_h264_encoder()?;
            self.h264_encoder = Some(h264_encoder);

            // 配置管线：BGRA -> CPU 转换 NV12 -> 编码器
            self.configure_pipeline()?;

            // 发送开始流消息
            self.send_stream_messages()?;

            self.initialized = true;

            // 尝试获取 SPS/PPS
            // 有些编码器在处理第一帧后才输出 SPS/PPS
            // 我们先尝试从编码器属性中获取
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

        // 返回 SPS/PPS 作为初始化结果
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

    /// 编码单帧（内部方法）
    ///
    /// # 参数
    /// - bgra_data: BGRA 格式的帧数据（每像素 4 字节）
    ///
    /// # 返回
    /// 编码后的帧数据列表（一帧输入可能产生多个输出帧，如 SPS+PPS+IDR）
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

            // 根据模式选择格式
            if self.use_cpu_mode {
                // CPU 模式: BGRA -> IYUV (YUV420P)
                let iyuv_data = bgra_to_iyuv(bgra_data, self.params.width, self.params.height);
                let input_sample = self.create_iyuv_sample(&iyuv_data)?;
                encoder
                    .ProcessInput(self.encoder_input_id, &input_sample, 0)
                    .map_err(|e| {
                        RecorderError::MFError(format!("H264 编码器 ProcessInput 失败(CPU模式): {}", e))
                    })?;
            } else {
                // GPU 模式: BGRA -> NV12
                let nv12_data = bgra_to_nv12(bgra_data, self.params.width, self.params.height);
                let input_sample = self.create_nv12_sample(&nv12_data)?;
                encoder
                    .ProcessInput(self.encoder_input_id, &input_sample, 0)
                    .map_err(|e| {
                        RecorderError::MFError(format!("H264 编码器 ProcessInput 失败(GPU模式): {}", e))
                    })?;
            }

            // 从 H264 编码器获取编码输出
            let mut encoded_frames = self.process_encoder_output()?;

            // 如果还没获取到 SPS/PPS，尝试从属性中提取
            if self.sps.is_empty() || self.pps.is_empty() {
                self.extract_sps_pps_from_attributes()?;
            }

            // 为每个编码帧标记帧类型
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

    /// 停止编码器（内部方法）
    ///
    /// 发送 Drain 消息，获取所有剩余输出，然后释放资源
    pub fn stop_encoding(&mut self) -> Result<Vec<EncodedFrame>, RecorderError> {
        if !self.initialized {
            return Ok(Vec::new());
        }

        let mut remaining_frames = Vec::new();

        unsafe {
            // 1. 发送 Drain 消息给 H264 编码器
            if let Some(encoder) = &self.h264_encoder {
                let _ = encoder.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);

                // 获取所有剩余输出
                if let Ok(frames) = self.process_encoder_output() {
                    remaining_frames.extend(frames);
                }
            }

            // 3. 清理资源
            self.h264_encoder = None;

            // 关闭 Media Foundation
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

    /// 获取 SPS 数据
    pub fn sps(&self) -> &[u8] {
        &self.sps
    }

    /// 获取 PPS 数据
    pub fn pps(&self) -> &[u8] {
        &self.pps
    }

    /// 获取编码参数
    pub fn params(&self) -> &H264EncodeParams {
        &self.params
    }

    /// 获取帧计数
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

    /// 配置 MFT 管线
    ///
    /// 管线: BGRA -> CPU 转换 NV12 -> H264 编码器 -> H264
    unsafe fn configure_pipeline(&mut self) -> Result<(), RecorderError> {
        let h264_encoder = self
            .h264_encoder
            .as_ref()
            .ok_or_else(|| RecorderError::MFError("H264 编码器未创建".into()))?;

        // 获取流 ID
        self.encoder_input_id = self.get_input_stream_id(h264_encoder)?;
        self.encoder_output_id = self.get_output_stream_id(h264_encoder)?;

        // 4. 配置 H264 编码器的输入类型 (NV12) - 使用硬件加速
        // 如果前面成功注册了 D3D11 设备，这里应该可以接受 NV12
        let nv12_type = self.create_nv12_media_type()?;
        h264_encoder
            .SetInputType(self.encoder_input_id, &nv12_type, 0)
            .map_err(|e| {
                // 如果失败，回退到 IYUV 格式（CPU 模式）
                println!("[H264Encoder] NV12 输入类型失败: {}，回退到 IYUV 模式", e);
                
                let iyuv_type = self.create_iyuv_media_type()?;
                h264_encoder
                    .SetInputType(self.encoder_input_id, &iyuv_type, 0)
                    .map_err(|e2| RecorderError::MFError(format!("设置 H264 编码器输入类型(IYUV)失败: {}", e2)))?;
                
                // 标记为使用 CPU 模式
                self.use_cpu_mode = true;
                return Ok(());
            })?;

        // 如果 NV12 成功，设置输出类型
        let h264_type = self.create_h264_media_type()?;
        h264_encoder
            .SetOutputType(self.encoder_output_id, &h264_type, 0)
            .map_err(|e| RecorderError::MFError(format!("设置 H264 编码器输出类型失败: {}", e)))?;

        println!("[H264Encoder] MFT 管线配置完成: NV12 -> H264 (D3D11 加速)");
        Ok(())
    }

    /// 发送流控制消息
    unsafe fn send_stream_messages(&self) -> Result<(), RecorderError> {
        let h264_encoder = self
            .h264_encoder
            .as_ref()
            .ok_or_else(|| RecorderError::MFError("H264 编码器未创建".into()))?;

        // 通知 H264 编码器开始流
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

    /// 创建 RGB32 输入媒体类型
    unsafe fn create_rgb32_media_type(&self) -> Result<IMFMediaType, RecorderError> {
        let media_type = MFCreateMediaType()
            .map_err(|e| RecorderError::MFError(format!("创建 RGB32 MediaType 失败: {}", e)))?;

        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| RecorderError::MFError(format!("设置主类型失败: {}", e)))?;

        media_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
            .map_err(|e| RecorderError::MFError(format!("设置子类型失败: {}", e)))?;

        media_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| RecorderError::MFError(format!("设置交错模式失败: {}", e)))?;

        // 使用对齐后的分辨率
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
            .SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)
            .map_err(|e| RecorderError::MFError(format!("设置样本独立属性失败: {}", e)))?;

        // 设置默认 stride (BGRA = width * 4)
        let stride = (self.params.width * 4) as i32 as u32;
        media_type
            .SetUINT32(&MF_MT_DEFAULT_STRIDE, stride)
            .map_err(|e| RecorderError::MFError(format!("设置 stride 失败: {}", e)))?;

        Ok(media_type)
    }

    /// 创建 NV12 媒体类型（颜色转换器输出 / H264 编码器输入）
    unsafe fn create_nv12_media_type(&self) -> Result<IMFMediaType, RecorderError> {
        let media_type = MFCreateMediaType()
            .map_err(|e| RecorderError::MFError(format!("创建 NV12 MediaType 失败: {}", e)))?;

        // 主类型
        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| RecorderError::MFError(format!("设置主类型失败: {}", e)))?;

        // 子类型 NV12
        media_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
            .map_err(|e| RecorderError::MFError(format!("设置子类型失败: {}", e)))?;

        // 帧大小（对齐到 16）
        let aligned_width = (self.params.width + 15) & !15;
        let aligned_height = (self.params.height + 15) & !15;
        media_type
            .SetUINT64(
                &MF_MT_FRAME_SIZE,
                ((aligned_width as u64) << 32) | (aligned_height as u64),
            )
            .map_err(|e| RecorderError::MFError(format!("设置帧大小失败: {}", e)))?;

        // 帧率
        media_type
            .SetUINT64(&MF_MT_FRAME_RATE, ((self.params.fps as u64) << 32) | 1u64)
            .map_err(|e| RecorderError::MFError(format!("设置帧率失败: {}", e)))?;

        // 像素宽高比 1:1
        media_type
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1u64)
            .map_err(|e| RecorderError::MFError(format!("设置像素宽高比失败: {}", e)))?;

        // 交错模式：逐行
        media_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| RecorderError::MFError(format!("设置交错模式失败: {}", e)))?;

        // 所有样本独立
        media_type
            .SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)
            .map_err(|e| RecorderError::MFError(format!("设置样本独立属性失败: {}", e)))?;

        // NV12 的 stride：Y 平面 stride = 对齐后的宽度
        let stride = aligned_width;
        media_type
            .SetUINT32(&MF_MT_DEFAULT_STRIDE, stride)
            .map_err(|e| RecorderError::MFError(format!("设置 NV12 stride 失败: {}", e)))?;

        Ok(media_type)
    }

    /// 创建 IYUV (YUV420P) 媒体类型 - CPU 回退模式
    unsafe fn create_iyuv_media_type(&self) -> Result<IMFMediaType, RecorderError> {
        let media_type = MFCreateMediaType()
            .map_err(|e| RecorderError::MFError(format!("创建 IYUV MediaType 失败: {}", e)))?;

        // 主类型
        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| RecorderError::MFError(format!("设置主类型失败: {}", e)))?;

        // 子类型 IYUV (YUV420 Planar)
        media_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_IYUV)
            .map_err(|e| RecorderError::MFError(format!("设置子类型失败: {}", e)))?;

        // 帧大小（对齐到 16）
        let aligned_width = (self.params.width + 15) & !15;
        let aligned_height = (self.params.height + 15) & !15;
        media_type
            .SetUINT64(
                &MF_MT_FRAME_SIZE,
                ((aligned_width as u64) << 32) | (aligned_height as u64),
            )
            .map_err(|e| RecorderError::MFError(format!("设置帧大小失败: {}", e)))?;

        // 帧率
        media_type
            .SetUINT64(&MF_MT_FRAME_RATE, ((self.params.fps as u64) << 32) | 1u64)
            .map_err(|e| RecorderError::MFError(format!("设置帧率失败: {}", e)))?;

        // 像素宽高比 1:1
        media_type
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1u64)
            .map_err(|e| RecorderError::MFError(format!("设置像素宽高比失败: {}", e)))?;

        // 交错模式：逐行
        media_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| RecorderError::MFError(format!("设置交错模式失败: {}", e)))?;

        // 所有样本独立
        media_type
            .SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)
            .map_err(|e| RecorderError::MFError(format!("设置样本独立属性失败: {}", e)))?;

        // IYUV 的 stride
        let stride = aligned_width;
        media_type
            .SetUINT32(&MF_MT_DEFAULT_STRIDE, stride)
            .map_err(|e| RecorderError::MFError(format!("设置 IYUV stride 失败: {}", e)))?;

        Ok(media_type)
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

        // 使用对齐后的分辨率
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

        // 设置码率
        media_type
            .SetUINT32(&MF_MT_AVG_BITRATE, self.params.bitrate)
            .map_err(|e| RecorderError::MFError(format!("设置码率失败: {}", e)))?;

        // 设置 H264 Profile
        media_type
            .SetUINT32(&MF_MT_MPEG2_PROFILE, self.params.profile)
            .map_err(|e| RecorderError::MFError(format!("设置 H264 Profile 失败: {}", e)))?;

        // 设置 Level（根据分辨率自动选择）
        let level = if self.params.width >= 3840 {
            52 // Level 5.2 (4K)
        } else if self.params.width >= 1920 {
            40 // Level 4.0 (1080P)
        } else {
            30 // Level 3.0 (720P 及以下)
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

    /// 创建 NV12 格式的 IMFSample
    unsafe fn create_nv12_sample(&self, nv12_data: &[u8]) -> Result<IMFSample, RecorderError> {
        let sample = MFCreateSample()
            .map_err(|e| RecorderError::MFError(format!("创建 IMFSample 失败: {}", e)))?;

        // 设置时间戳
        let timestamp = self.frame_count as i64 * self.frame_duration;
        sample
            .SetSampleTime(timestamp)
            .map_err(|e| RecorderError::MFError(format!("设置样本时间失败: {}", e)))?;

        sample
            .SetSampleDuration(self.frame_duration)
            .map_err(|e| RecorderError::MFError(format!("设置样本持续时间失败: {}", e)))?;

        // 创建内存缓冲区
        let buffer_size = nv12_data.len() as u32;
        let buffer = MFCreateMemoryBuffer(buffer_size)
            .map_err(|e| RecorderError::MFError(format!("创建内存缓冲区失败: {}", e)))?;

        // 锁定缓冲区并拷贝数据
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

        // 设置 NV12 格式的 stride
        // NV12: Y 平面 (width * height) + UV 平面 (width * height / 2)
        // Y stride = width, UV stride = width
        let width = self.params.width;
        sample
            .SetUINT32(&MF_MT_DEFAULT_STRIDE, width)
            .ok(); // stride 设置失败不是致命错误

        Ok(sample)
    }

    /// 创建 IYUV 格式的 IMFSample（CPU 回退模式）
    unsafe fn create_iyuv_sample(&self, iyuv_data: &[u8]) -> Result<IMFSample, RecorderError> {
        let sample = MFCreateSample()
            .map_err(|e| RecorderError::MFError(format!("创建 IMFSample 失败: {}", e)))?;

        // 设置时间戳
        let timestamp = self.frame_count as i64 * self.frame_duration;
        sample
            .SetSampleTime(timestamp)
            .map_err(|e| RecorderError::MFError(format!("设置样本时间失败: {}", e)))?;

        sample
            .SetSampleDuration(self.frame_duration)
            .map_err(|e| RecorderError::MFError(format!("设置样本持续时间失败: {}", e)))?;

        // 创建内存缓冲区
        let buffer_size = iyuv_data.len() as u32;
        let buffer = MFCreateMemoryBuffer(buffer_size)
            .map_err(|e| RecorderError::MFError(format!("创建内存缓冲区失败: {}", e)))?;

        // 锁定缓冲区并拷贝数据
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

        ptr::copy_nonoverlapping(iyuv_data.as_ptr(), data_ptr, iyuv_data.len());

        buffer
            .SetCurrentLength(buffer_size)
            .map_err(|e| RecorderError::MFError(format!("设置缓冲区长度失败: {}", e)))?;

        buffer
            .Unlock()
            .map_err(|e| RecorderError::MFError(format!("解锁缓冲区失败: {}", e)))?;

        sample
            .AddBuffer(&buffer)
            .map_err(|e| RecorderError::MFError(format!("添加 Buffer 到 Sample 失败: {}", e)))?;

        // 设置 IYUV 格式的 stride
        let width = self.params.width;
        sample
            .SetUINT32(&MF_MT_DEFAULT_STRIDE, width)
            .ok();

        Ok(sample)
    }

    /// 处理 H264 编码器的输出
    ///
    /// 从 H264 编码器获取编码后的 NAL 单元
    unsafe fn process_encoder_output(&mut self) -> Result<Vec<EncodedFrame>, RecorderError> {
        let encoder = self
            .h264_encoder
            .as_ref()
            .ok_or(RecorderError::NotRecording)?;

        let mut encoded_frames = Vec::new();

        // H264 编码输出缓冲区大小（根据码率估算）
        let max_output_size = (self.params.bitrate / self.params.fps * 2) as u32;
        let output_buffer_size = max_output_size.max(1024 * 1024); // 至少 1MB

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
                        // 从 Sample 提取编码数据
                        let frame_data = self.extract_sample_data(sample)?;
                        if !frame_data.is_empty() {
                            // 将编码数据拆分为 NAL 单元
                            let nal_units = extract_nal_units(&frame_data);
                            for nal_data in nal_units {
                                // 重建带起始码的 NAL 单元（Annex-B 格式）
                                let mut annex_b = vec![0x00, 0x00, 0x00, 0x01];
                                annex_b.extend_from_slice(&nal_data);

                                let frame_type = Self::detect_frame_type(&annex_b);

                                // 缓存 SPS/PPS
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
                    // 继续尝试获取更多输出
                    continue;
                }
                Err(e) => {
                    let hr = e.code().0 as u32;
                    if hr == 0xC00D6D72 {
                        // MF_E_TRANSFORM_NEED_MORE_INPUT
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
    ///
    /// 某些 H264 编码器在编码开始后会将 SPS/PPS 存储在输出媒体类型的属性中
    unsafe fn extract_sps_pps_from_attributes(&mut self) -> Result<(), RecorderError> {
        let encoder = match self.h264_encoder.as_ref() {
            Some(e) => e,
            None => return Ok(()),
        };

        // 获取输出媒体类型
        let output_type = match encoder.GetOutputAvailableType(self.encoder_output_id, 0) {
            Ok(t) => t,
            Err(_) => return Ok(()), // 获取失败不影响主流程
        };

        // 尝试从输出类型获取 SPS
        // 使用 MF_MT_MPEG_SEQUENCE_HEADER 属性，它包含 SPS+PPS
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

                // 解析 SPS+PPS（MPEG Sequence Header 格式）
                // 格式: [2字节 SPS 长度][SPS 数据][2字节 PPS 长度][PPS 数据]
                if header_data.len() > 4 {
                    self.parse_mpeg_sequence_header(&header_data);
                }
            }
        }

        Ok(())
    }

    /// 解析 MPEG Sequence Header 格式的 SPS/PPS 数据
    ///
    /// 格式: [2字节 SPS 长度][SPS 数据][2字节 PPS 长度][PPS 数据]
    /// 其中长度是大端序
    fn parse_mpeg_sequence_header(&mut self, data: &[u8]) {
        if data.len() < 4 {
            return;
        }

        let mut offset = 0;

        // 解析 SPS
        if offset + 2 <= data.len() {
            let sps_len = ((data[offset] as usize) << 8) | (data[offset + 1] as usize);
            offset += 2;

            if offset + sps_len <= data.len() && sps_len > 0 {
                // 添加 Annex-B 起始码
                let mut sps = vec![0x00, 0x00, 0x00, 0x01];
                sps.extend_from_slice(&data[offset..offset + sps_len]);

                if self.sps.is_empty() {
                    self.sps = sps;
                    println!("[H264Encoder] 从属性中提取 SPS: {} 字节", sps_len);
                }
                offset += sps_len;
            }
        }

        // 解析 PPS
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
    /// 创建 H264 编码器（使用显示器编号）
    ///
    /// # 参数
    /// - fps: 帧率（默认 10）
    /// - bitrate: 码率（默认 2000000 bps）
    /// - monitor: 显示器编号（1=主显示器，2=副显示器）
    /// - profile: H264 Profile（66=Baseline, 77=Main, 100=High，默认 66）
    ///
    /// 此构造函数自动检测显示器分辨率
    #[new]
    #[pyo3(signature = (fps=10, bitrate=2000000, monitor=1, profile=66))]
    pub fn new(fps: u32, bitrate: u32, monitor: u32, profile: u32) -> Result<Self, RecorderError> {
        use crate::d3d11::D3D11TextureManager;

        // 检测显示器尺寸
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

    /// 启动编码器
    ///
    /// # 返回
    /// 返回 Python 字典，包含初始化信息（SPS/PPS）
    pub fn start(&mut self) -> Result<Py<PyDict>, RecorderError> {
        // 调用现有的 start_encoding 方法
        let _ = self.start_encoding()?;

        // 返回 Python 字典
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

    /// 编码单帧（推流专用版本）
    ///
    /// # 参数
    /// - frame_data: BGRA 格式的帧数据
    ///
    /// # 返回
    /// 返回编码后的数据，带帧类型前缀：
    /// - 0x01 = SPS/PPS
    /// - 0x02 = IDR (关键帧)
    /// - 0x03 = P (预测帧)
    /// 格式: [1字节帧类型][Annex-B NAL 数据]
    pub fn encode_frame(&mut self, frame_data: &[u8]) -> Result<Option<Vec<u8>>, RecorderError> {
        let frames = self.encode_frame_data(frame_data)?;

        if frames.is_empty() {
            return Ok(None);
        }

        let mut result = Vec::new();
        for frame in frames {
            // 根据帧类型添加前缀
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

    /// 获取视频宽度
    #[getter]
    pub fn width(&self) -> u32 {
        self.params.width
    }

    /// 获取视频高度
    #[getter]
    pub fn height(&self) -> u32 {
        self.params.height
    }

    /// 获取帧率
    #[getter]
    pub fn fps(&self) -> u32 {
        self.params.fps
    }

    /// 获取是否已启动
    #[getter]
    pub fn is_encoding(&self) -> bool {
        self.initialized
    }

    /// 获取已编码帧数
    #[getter]
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// 获取 SPS 数据
    #[getter]
    pub fn get_sps(&self) -> Vec<u8> {
        self.sps.clone()
    }

    /// 获取 PPS 数据
    #[getter]
    pub fn get_pps(&self) -> Vec<u8> {
        self.pps.clone()
    }
}

impl Drop for H264Encoder {
    fn drop(&mut self) {
        if self.initialized {
            // 确保资源被正确清理
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
