use crate::d3d11::D3D11TextureManager;
use crate::error::RecorderError;
use crate::mf_writer::MFSinkWriter;
use parking_lot::Mutex;
use pyo3::prelude::*;
use pyo3::types::PyByteArray;
use std::sync::Arc;
use windows::Win32::Media::MediaFoundation::*;

/// Windows 录屏器
///
/// 使用 D3D11 纹理和 Media Foundation SinkWriter 进行硬件编码
#[pyclass]
pub struct WinRecorder {
    texture_manager: Option<Arc<D3D11TextureManager>>,
    sink_writer: Option<Arc<Mutex<MFSinkWriter>>>,
    output_path: String,
    fps: u32,
    audio: bool,
    monitor: u32,
    width: u32,
    height: u32,
    recording: bool,
}

#[pymethods]
impl WinRecorder {
    /// 创建录屏器
    ///
    /// # 参数
    /// - output_path: 输出 MP4 文件路径
    /// - fps: 帧率（默认 30）
    /// - audio: 是否录制音频（默认 false，当前版本不支持音频）
    /// - monitor: 显示器选择（1=主屏幕 left=0，2=副屏幕，默认 1）
    #[new]
    #[pyo3(signature = (output_path, fps=30, audio=false, monitor=1))]
    pub fn new(output_path: String, fps: u32, audio: bool, monitor: u32) -> Result<Self, RecorderError> {
        // 检测显示器尺寸
        let (width, height) = D3D11TextureManager::detect_monitor(monitor)?;

        Ok(Self {
            texture_manager: None,
            sink_writer: None,
            output_path,
            fps,
            audio,
            monitor,
            width,
            height,
            recording: false,
        })
    }

    /// 开始录制
    ///
    /// # 说明
    /// 初始化 D3D11 设备和 Media Foundation SinkWriter
    /// 分辨率会自动对齐到 16 倍数（H264 编码器要求）
    pub fn start(&mut self) -> Result<(), RecorderError> {
        if self.recording {
            return Err(RecorderError::AlreadyRecording);
        }

        // 先创建 SinkWriter（内部会对齐分辨率）
        // 使用临时设备
        let temp_texture_manager = D3D11TextureManager::new(self.width, self.height)?;
        let device = temp_texture_manager.device().clone();

        let mut sink_writer = MFSinkWriter::new(
            &self.output_path,
            &device,
            self.width,
            self.height,
            self.fps,
            self.audio,
        )?;

        // 获取对齐后的分辨率
        let aligned_width = sink_writer.width();
        let aligned_height = sink_writer.height();

        // 使用对齐后的分辨率创建纹理管理器
        let texture_manager = D3D11TextureManager::new(aligned_width, aligned_height)?;

        sink_writer.begin_writing()?;

        // 更新内部尺寸为对齐后的尺寸
        self.width = aligned_width;
        self.height = aligned_height;

        self.texture_manager = Some(Arc::new(texture_manager));
        self.sink_writer = Some(Arc::new(Mutex::new(sink_writer)));
        self.recording = true;

        Ok(())
    }

    /// 写入一帧
    ///
    /// # 参数
    /// - frame_data: BGRA 格式的帧数据（字节数组）
    ///
    /// # 说明
    /// 数据将被上传到 D3D11 纹理并编码
    pub fn write_frame(&mut self, frame_data: &Bound<'_, PyByteArray>) -> Result<(), RecorderError> {
        if !self.recording {
            return Err(RecorderError::NotRecording);
        }

        let texture_manager = self
            .texture_manager
            .as_ref()
            .ok_or(RecorderError::NotRecording)?;

        let sink_writer = self
            .sink_writer
            .as_ref()
            .ok_or(RecorderError::NotRecording)?;

        // 获取帧数据（零拷贝）
        let frame_bytes = unsafe { frame_data.as_bytes() };

        // 上传到纹理
        texture_manager.upload_bgra(frame_bytes)?;

        // 创建 MF Sample
        let sample = texture_manager.create_mf_sample()?;

        // 写入 SinkWriter
        let mut writer = sink_writer.lock();
        writer.write_sample(&sample)?;

        Ok(())
    }

    /// 结束录制
    ///
    /// # 说明
    /// 完成 MP4 文件并释放资源
    pub fn stop(&mut self) -> Result<(), RecorderError> {
        if !self.recording {
            return Err(RecorderError::NotRecording);
        }

        // 结束编码
        if let Some(sink_writer) = &self.sink_writer {
            let mut writer = sink_writer.lock();
            writer.finalize()?;
        }

        // 清理资源
        self.sink_writer = None;
        self.texture_manager = None;
        self.recording = false;

        // 关闭 Media Foundation
        unsafe {
            let _ = MFShutdown();
        }

        Ok(())
    }

    /// 获取视频宽度
    #[getter]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// 获取视频高度
    #[getter]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// 获取帧率
    #[getter]
    pub fn fps(&self) -> u32 {
        self.fps
    }

    /// 获取是否正在录制
    #[getter]
    pub fn is_recording(&self) -> bool {
        self.recording
    }

    /// 获取已编码帧数
    pub fn frame_count(&self) -> Result<u64, RecorderError> {
        let sink_writer = self
            .sink_writer
            .as_ref()
            .ok_or(RecorderError::NotRecording)?;

        let writer = sink_writer.lock();
        Ok(writer.frame_count())
    }

    /// 获取显示器尺寸（静态方法）
    ///
    /// # 参数
    /// - monitor: 显示器选择（1=主屏幕 left=0，2=副屏幕）
    ///
    /// # 返回
    /// 返回元组 (width, height)
    #[staticmethod]
    pub fn get_monitor_size(monitor: u32) -> Result<(u32, u32), RecorderError> {
        D3D11TextureManager::detect_monitor(monitor)
    }
}