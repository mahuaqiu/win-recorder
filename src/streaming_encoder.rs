//! 流式 H.264 编码器
//!
//! 用于实时推流场景，输出编码后的 NAL 单元而不是写入文件

use pyo3::prelude::*;
use crate::d3d11::D3D11TextureManager;
use crate::error::RecorderError;
use crate::mf_writer::MFSinkWriter;
use std::sync::Arc;
use windows::Win32::Media::MediaFoundation::*;

/// 流式编码器
///
/// 与 WinRecorder 不同，它不写入文件，而是将编码后的数据输出到内存缓冲区
#[pyclass]
pub struct StreamingEncoder {
    /// 纹理管理器
    texture_manager: Option<Arc<D3D11TextureManager>>,
    /// 编码器
    sink_writer: Option<Arc<Mutex<MFSinkWriter>>>,
    /// 编码器配置
    fps: u32,
    bitrate: u32,
    monitor: u32,
    width: u32,
    height: u32,
    /// 是否正在编码
    encoding: bool,
    /// 编码后的数据缓冲区
    output_buffer: Vec<u8>,
}

#[pymethods]
impl StreamingEncoder {
    /// 创建流式编码器
    ///
    /// # 参数
    /// - fps: 帧率
    /// - bitrate: 码率（默认 2Mbps）
    /// - monitor: 显示器索引
    #[new]
    #[pyo3(signature = (fps=10, bitrate=2000000, monitor=1))]
    pub fn new(fps: u32, bitrate: u32, monitor: u32) -> Result<Self, RecorderError> {
        // 检测显示器尺寸
        let (width, height) = D3D11TextureManager::detect_monitor(monitor)?;

        Ok(Self {
            texture_manager: None,
            sink_writer: None,
            fps,
            bitrate,
            monitor,
            width,
            height,
            encoding: false,
            output_buffer: Vec::new(),
        })
    }

    /// 启动编码器
    ///
    /// 返回编码器信息，包括 SPS/PPS 数据
    pub fn start(&mut self) -> Result<Py<PyDict>, RecorderError> {
        if self.encoding {
            return Err(RecorderError::AlreadyRecording);
        }

        // 创建临时纹理管理器获取设备
        let temp_texture_manager = D3D11TextureManager::new(self.width, self.height)?;
        let device = temp_texture_manager.device().clone();

        // 创建内存输出的 SinkWriter
        // 注意：当前版本仍然需要临时文件路径，我们使用临时方案
        // 完整实现需要自定义 IMFByteStream
        let temp_path = std::env::temp_dir().join("win_recorder_stream.temp.mp4");
        let temp_path_str = temp_path.to_string_lossy().to_string();

        let mut sink_writer = MFSinkWriter::new(
            &temp_path_str,
            &device,
            self.width,
            self.height,
            self.fps,
            false, // 不含音频
        )?;

        // 获取对齐后的分辨率
        let aligned_width = sink_writer.width();
        let aligned_height = sink_writer.height();

        // 使用对齐后的分辨率创建纹理管理器
        let texture_manager = D3D11TextureManager::new(aligned_width, aligned_height)?;

        sink_writer.begin_writing()?;

        // 更新内部尺寸
        self.width = aligned_width;
        self.height = aligned_height;

        self.texture_manager = Some(Arc::new(texture_manager));
        self.sink_writer = Some(Arc::new(Mutex::new(sink_writer)));
        self.encoding = true;
        self.output_buffer.clear();

        // TODO: 提取 SPS/PPS 数据
        // 当前返回空数据，需要实现内存输出后才能获取
        let sps = String::new();
        let pps = String::new();

        // 使用 Python 字典返回信息
        let info = pyo3::Python::with_gil(|py| {
            let dict = pyo3::types::PyDict::new(py);
            dict.set_item("width", self.width)?;
            dict.set_item("height", self.height)?;
            dict.set_item("fps", self.fps)?;
            dict.set_item("sps", sps)?;
            dict.set_item("pps", pps)?;
            Ok::<_, pyo3::PyErr>(dict.into())
        })?;

        Ok(info)
    }

    /// 编码单帧
    ///
    /// # 参数
    /// - frame_data: BGRA 格式的帧数据
    ///
    /// # 返回
    /// 编码后的数据，包含帧类型前缀
    pub fn encode_frame(&mut self, frame_data: &[u8]) -> Result<Option<Vec<u8>>, RecorderError> {
        if !self.encoding {
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

        // 上传到纹理
        texture_manager.upload_bgra(frame_data)?;

        // 创建 MF Sample
        let sample = texture_manager.create_mf_sample()?;

        // 写入编码器
        let mut writer = sink_writer.lock();
        writer.write_sample(&sample)?;

        // TODO: 从编码器提取 NAL 单元
        // 当前版本不返回实际编码数据，因为 Media Foundation
        // 的内存输出需要复杂的 IMFByteStream 实现
        // 简化方案：返回空数据，由调用方处理

        Ok(None)
    }

    /// 停止编码器
    pub fn stop(&mut self) -> Result<(), RecorderError> {
        if !self.encoding {
            return Ok(());
        }

        // 结束编码
        if let Some(sink_writer) = &self.sink_writer {
            let mut writer = sink_writer.lock();
            writer.finalize()?;
        }

        // 清理临时文件
        let temp_path = std::env::temp_dir().join("win_recorder_stream.temp.mp4");
        let _ = std::fs::remove_file(temp_path);

        // 清理资源
        self.sink_writer = None;
        self.texture_manager = None;
        self.encoding = false;
        self.output_buffer.clear();

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

    /// 获取是否正在编码
    #[getter]
    pub fn is_encoding(&self) -> bool {
        self.encoding
    }
}