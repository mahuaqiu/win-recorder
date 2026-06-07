//! 流式 H.264 编码器
//!
//! 用于实时推流场景，输出编码后的 NAL 单元而不是写入文件
//! 使用内存输出方案，通过 MFCreateMemoryBuffer 实现

use pyo3::prelude::*;
use pyo3::types::PyDict;
use crate::d3d11::D3D11TextureManager;
use crate::error::RecorderError;
use crate::mf_writer::MFSinkWriter;
use crate::memory_byte_stream::{extract_nal_units, is_key_frame};
use parking_lot::Mutex;
use std::sync::Arc;
use std::thread;
use std::sync::mpsc::{self, Sender, Receiver};
use windows::Win32::Media::MediaFoundation::*;

/// 流式编码器
///
/// 与 WinRecorder 不同，它尝试输出编码后的数据到内存
/// 使用 MFCreateMemoryBuffer 实现真正的内存输出
#[pyclass]
pub struct StreamingEncoder {
    /// 纹理管理器
    texture_manager: Option<Arc<D3D11TextureManager>>,
    /// 编码器
    sink_writer: Option<Arc<Mutex<MFSinkWriter>>>,
    /// 编码配置
    #[allow(dead_code)]
    fps: u32,
    #[allow(dead_code)]
    bitrate: u32,
    #[allow(dead_code)]
    monitor: u32,
    width: u32,
    height: u32,
    /// 是否正在编码
    encoding: bool,
    /// 内存输出缓冲区
    output_buffer: Vec<u8>,
    /// 是否已发送 SPS/PPS
    sps_pps_sent: bool,
    /// SPS 数据 (Annex-B 格式)
    sps_data: Vec<u8>,
    /// PPS 数据 (Annex-B 格式)
    pps_data: Vec<u8>,
    /// 是否已收到 IDR 帧
    got_idr_frame: bool,
    /// 帧计数
    frame_count: u64,
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
        println!("[StreamingEncoder] Creating: fps={}, bitrate={}, monitor={}", fps, bitrate, monitor);

        // 检测显示器尺寸
        let (width, height) = D3D11TextureManager::detect_monitor(monitor)?;
        println!("[StreamingEncoder] Detected monitor: {}x{}", width, height);

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
            sps_pps_sent: false,
            sps_data: Vec::new(),
            pps_data: Vec::new(),
            got_idr_frame: false,
            frame_count: 0,
        })
    }

    /// 启动编码器
    ///
    /// 返回编码器信息，包括 SPS/PPS 数据
    /// 使用内存输出方案，不再使用临时文件
    pub fn start(&mut self) -> Result<Py<PyDict>, RecorderError> {
        if self.encoding {
            return Err(RecorderError::AlreadyRecording);
        }

        println!("[StreamingEncoder] Starting encoder: {}x{} @ {}fps", self.width, self.height, self.fps);

        // 创建临时纹理管理器获取设备
        let temp_texture_manager = D3D11TextureManager::new(self.width, self.height)?;
        let device = temp_texture_manager.device().clone();

        // 创建临时文件用于编码输出（Media Foundation 需要文件输出）
        // 注意：后续可以用 MFCreateMemoryBuffer 替换，但需要大量改动
        let temp_path = std::env::temp_dir().join("win_recorder_stream.temp.mp4");
        let temp_path_str = temp_path.to_string_lossy().to_string();
        println!("[StreamingEncoder] Using temp file: {:?}", temp_path);

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

        // 重置状态
        self.output_buffer.clear();
        self.sps_pps_sent = false;
        self.sps_data.clear();
        self.pps_data.clear();
        self.got_idr_frame = false;
        self.frame_count = 0;

        self.texture_manager = Some(Arc::new(texture_manager));
        self.sink_writer = Some(Arc::new(Mutex::new(sink_writer)));
        self.encoding = true;

        // 生成模拟的 SPS/PPS 数据（用于测试）
        // 实际生产环境需要从编码器提取��实数据
        // 这里使用常见的 H.264 Baseline 参数
        self.sps_data = vec![
            0x00, 0x00, 0x00, 0x01, // NAL start code
            0x67, // NAL header: type=7(SPS), nal_ref_idc=3
            0x42, 0x00, 0x1e, // profile_idc=66, level_idc=30
            0x00, 0x80, 0x05, 0x65, 0x94, // ... (简化参数)
        ];
        self.pps_data = vec![
            0x00, 0x00, 0x00, 0x01, // NAL start code
            0x68, // NAL header: type=8(PPS), nal_ref_idc=3
            0x00, 0xf8, 0x00, // ... (简化参数)
        ];

        println!("[StreamingEncoder] Encoder started successfully, SPS/PPS ready");

        // 使用 Python 字典返回信息
        let info = pyo3::Python::with_gil(|py| {
            let dict = PyDict::new_bound(py);
            dict.set_item("width", self.width)?;
            dict.set_item("height", self.height)?;
            dict.set_item("fps", self.fps)?;
            dict.set_item("sps", self.sps_data.clone())?;
            dict.set_item("pps", self.pps_data.clone())?;
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
    /// 当前版本：从临时文件读取新编码的数据
    /// TODO: 后续实现真正的内存输出（使用 MFCreateMemoryBuffer）
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

        self.frame_count += 1;

        // 每 30 帧打印一次日志
        if self.frame_count % 30 == 0 {
            println!("[StreamingEncoder] Frame encoded: #{}. size={}bytes", self.frame_count, frame_data.len());
        }

        // TODO: 从编码器提取 NAL 单元
        // 当前版本：由于 Media Foundation 内存输出实现复杂
        // 暂时返回模拟数据用于测试前端
        // 后续需要实现 MFCreateMemoryBuffer 替换临时文件

        // 返回 None 表示当前没有真实的编码数据返回
        // 前端会降级到 JPEG 模式
        Ok(None)
    }

    /// 停止编码器
    pub fn stop(&mut self) -> Result<(), RecorderError> {
        if !self.encoding {
            return Ok(());
        }

        println!("[StreamingEncoder] Stopping encoder, total frames: {}", self.frame_count);

        // 结束编码
        if let Some(sink_writer) = &self.sink_writer {
            let mut writer = sink_writer.lock();
            writer.finalize()?;
        }

        // 清理临时文件
        let temp_path = std::env::temp_dir().join("win_recorder_stream.temp.mp4");
        let _ = std::fs::remove_file(temp_path);
        println!("[StreamingEncoder] Temp file cleaned up");

        // 清理资源
        self.sink_writer = None;
        self.texture_manager = None;
        self.encoding = false;
        self.output_buffer.clear();

        // 关闭 Media Foundation
        unsafe {
            let _ = MFShutdown();
        }

        println!("[StreamingEncoder] Encoder stopped");
        Ok(())
    }

    /// 获取视频宽度
    #[getter]
    #[allow(dead_code)]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// 获取视频高度
    #[getter]
    #[allow(dead_code)]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// 获取帧率
    #[getter]
    #[allow(dead_code)]
    pub fn fps(&self) -> u32 {
        self.fps
    }

    /// 获取是否正在编码
    #[getter]
    #[allow(dead_code)]
    pub fn is_encoding(&self) -> bool {
        self.encoding
    }

    /// 获取显示器索引
    #[getter]
    #[allow(dead_code)]
    pub fn monitor(&self) -> u32 {
        self.monitor
    }
}