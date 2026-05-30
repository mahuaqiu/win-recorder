use crate::error::RecorderError;
use windows::core::PCWSTR;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Media::MediaFoundation::*;

/// Media Foundation SinkWriter 封装
///
/// 负责将帧数据编码为 MP4 视频
pub struct MFSinkWriter {
    sink_writer: IMFSinkWriter,
    stream_index: u32,
    frame_duration: i64,
    frame_count: u64,
    width: u32,
    height: u32,
}

// 手动实现 Send trait
unsafe impl Send for MFSinkWriter {}

impl MFSinkWriter {
    /// 创建 SinkWriter
    ///
    /// # 参数
    /// - output_path: 输出文件路径
    /// - device: D3D11 设备（当前版本不使用，但保留接口）
    /// - width: 视频宽度
    /// - height: 视频高度
    /// - fps: 帧率
    /// - audio: 是否包含音频（当前版本不支持）
    ///
    /// # 说明
    /// 输入类型为 MFVideoFormat_RGB32 (BGRA)，输出类型为 MFVideoFormat_H264
    pub fn new(
        output_path: &str,
        _device: &ID3D11Device,
        width: u32,
        height: u32,
        fps: u32,
        audio: bool,
    ) -> Result<Self, RecorderError> {
        if audio {
            return Err(RecorderError::InvalidParam(
                "Audio encoding is not supported in this version".into(),
            ));
        }

        unsafe {
            // 启动 Media Foundation
            MFStartup(MFSTARTUP_LITE, 0)
                .map_err(|e| RecorderError::MFError(format!("MFStartup 失败: {}", e)))?;

            // 设置输出文件路径
            let path_wide: Vec<u16> = output_path.encode_utf16().chain(Some(0)).collect();

            // 使用 MFCreateSinkWriterFromURL 自动创建 MP4 Sink
            // 直接传入 None 作为 attributes，让系统自动处理
            let sink_writer = MFCreateSinkWriterFromURL(
                PCWSTR(path_wide.as_ptr()),
                None,
                None::<&IMFAttributes>,
            )
            .map_err(|e| RecorderError::MFError(format!("创建 SinkWriter 失败: {}", e)))?;

            // 创建输出媒体类型（H264）
            let output_type = MFCreateMediaType()
                .map_err(|e| RecorderError::MFError(format!("创建 Output MediaType 失败: {}", e)))?;

            output_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| RecorderError::MFError(format!("设置输出主类型失败: {}", e)))?;

            output_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
                .map_err(|e| RecorderError::MFError(format!("设置输出子类型失败: {}", e)))?;

            output_type
                .SetUINT64(&MF_MT_FRAME_SIZE, (width as u64) << 32 | height as u64)
                .map_err(|e| RecorderError::MFError(format!("设置输出帧大小失败: {}", e)))?;

            output_type
                .SetUINT64(&MF_MT_FRAME_RATE, (fps as u64) << 32 | 1u64)
                .map_err(|e| RecorderError::MFError(format!("设置输出帧率失败: {}", e)))?;

            // 添加流（使用输出类型）
            let stream_index = sink_writer
                .AddStream(&output_type)
                .map_err(|e| RecorderError::MFError(format!("添加流失败: {}", e)))?;

            // 设置输入类型（RGB32）
            let input_type = MFCreateMediaType()
                .map_err(|e| RecorderError::MFError(format!("创建 Input MediaType 失败: {}", e)))?;

            input_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| RecorderError::MFError(format!("设置输入主类型失败: {}", e)))?;

            input_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
                .map_err(|e| RecorderError::MFError(format!("设置输入子类型失败: {}", e)))?;

            input_type
                .SetUINT64(&MF_MT_FRAME_SIZE, (width as u64) << 32 | height as u64)
                .map_err(|e| RecorderError::MFError(format!("设置输入帧大小失败: {}", e)))?;

            input_type
                .SetUINT64(&MF_MT_FRAME_RATE, (fps as u64) << 32 | 1u64)
                .map_err(|e| RecorderError::MFError(format!("设置输入帧率失败: {}", e)))?;

            // 设置输入类型
            sink_writer
                .SetInputMediaType(stream_index, &input_type, None)
                .map_err(|e| RecorderError::MFError(format!("设置输入类型失败: {}", e)))?;

            let frame_duration = 10_000_000_i64 / fps as i64;

            Ok(Self {
                sink_writer,
                stream_index,
                frame_duration,
                frame_count: 0,
                width,
                height,
            })
        }
    }

    /// 开始录制
    pub fn begin_writing(&mut self) -> Result<(), RecorderError> {
        unsafe {
            self.sink_writer
                .BeginWriting()
                .map_err(|e| RecorderError::MFError(format!("BeginWriting 失败: {}", e)))?;
        }
        Ok(())
    }

    /// 写入一帧
    ///
    /// # 参数
    /// - sample: 包含 D3D11 纹理的 IMFSample
    ///
    /// # 说明
    /// 自动计算时间戳并写入
    pub fn write_sample(&mut self, sample: &IMFSample) -> Result<(), RecorderError> {
        unsafe {
            // 设置样本时间
            let timestamp = (self.frame_count as i64) * self.frame_duration;
            sample
                .SetSampleTime(timestamp)
                .map_err(|e| RecorderError::MFError(format!("设置样本时间失败: {}", e)))?;

            // 设置样本持续时间
            sample
                .SetSampleDuration(self.frame_duration)
                .map_err(|e| RecorderError::MFError(format!("设置样本持续时间失败: {}", e)))?;

            // 写入样本
            self.sink_writer
                .WriteSample(self.stream_index, sample)
                .map_err(|e| RecorderError::MFError(format!("写入样本失败: {}", e)))?;

            self.frame_count += 1;
        }
        Ok(())
    }

    /// 结束录制
    pub fn finalize(&mut self) -> Result<(), RecorderError> {
        unsafe {
            self.sink_writer
                .Finalize()
                .map_err(|e| RecorderError::MFError(format!("Finalize 失败: {}", e)))?;
        }
        Ok(())
    }

    /// 计算合适的比特率
    fn calculate_bitrate(width: u32, height: u32, fps: u32) -> u32 {
        // 根据分辨率和帧率估算合适的比特率
        // 公式：像素数 * 帧率 * 比特每像素
        let pixels = (width * height) as u64;
        let bpp = 0.1; // 比特每像素，可根据质量要求调整
        ((pixels * fps as u64 * bpp as u64 * 1000) / 1000) as u32
    }

    /// 获取视频宽度
    pub fn width(&self) -> u32 {
        self.width
    }

    /// 获取视频高度
    pub fn height(&self) -> u32 {
        self.height
    }

    /// 获取已编码帧数
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }
}

impl Drop for MFSinkWriter {
    fn drop(&mut self) {
        unsafe {
            let _ = self.sink_writer.Flush(self.stream_index);
        }
        // MFShutdown 在 WinRecorder 中调用
    }
}