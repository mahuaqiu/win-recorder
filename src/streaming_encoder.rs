//! 流式 H.264 编码器
//!
//! 用于实时推流场景，输出编码后的 NAL 单元而不是写入文件
//! 使用内存 ByteStream 方案，通过 IMFByteStream 实现真正的内存输出

use pyo3::prelude::*;
use pyo3::types::PyDict;
use crate::d3d11::D3D11TextureManager;
use crate::error::RecorderError;
use crate::memory_byte_stream::{MemoryByteStream, extract_nal_units, get_nal_type};
use crate::mf_writer::MFSinkWriter;
use parking_lot::Mutex;
use std::sync::Arc;
use windows::Win32::Media::MediaFoundation::*;

/// 流式编码器
///
/// 与 WinRecorder 不同，它输出编码后的数据到内存
/// 使用 IMFByteStream 实现真正的内存输出
#[pyclass]
pub struct StreamingEncoder {
    /// 纹理管理器
    texture_manager: Option<Arc<D3D11TextureManager>>,
    /// 编码器
    sink_writer: Option<Arc<Mutex<MFSinkWriter>>>,
    /// 内存 ByteStream
    byte_stream: Option<MemoryByteStream>,
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
    /// 上次读取 ByteStream 的位置
    last_read_position: usize,
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
            byte_stream: None,
            fps,
            bitrate,
            monitor,
            width,
            height,
            encoding: false,
            sps_pps_sent: false,
            sps_data: Vec::new(),
            pps_data: Vec::new(),
            got_idr_frame: false,
            frame_count: 0,
            last_read_position: 0,
        })
    }

    /// 启动编码器
    ///
    /// 返回编码器信息，包括 SPS/PPS 数据
    /// 使用内存 ByteStream 方案，不再使用临时文件
    pub fn start(&mut self) -> Result<Py<PyDict>, RecorderError> {
        if self.encoding {
            return Err(RecorderError::AlreadyRecording);
        }

        println!("[StreamingEncoder] Starting encoder: {}x{} @ {}fps", self.width, self.height, self.fps);

        // 创建临时纹理管理器获取设备
        let temp_texture_manager = D3D11TextureManager::new(self.width, self.height)?;
        let device = temp_texture_manager.device().clone();

        // 创建内存 ByteStream
        let byte_stream = MemoryByteStream::new();

        // 使用 ByteStream 创建 SinkWriter
        let mut sink_writer = MFSinkWriter::from_byte_stream(
            byte_stream.as_raw(),
            &device,
            self.width,
            self.height,
            self.fps,
            false,
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
        self.sps_pps_sent = false;
        self.sps_data.clear();
        self.pps_data.clear();
        self.got_idr_frame = false;
        self.frame_count = 0;
        self.last_read_position = 0;

        self.texture_manager = Some(Arc::new(texture_manager));
        self.sink_writer = Some(Arc::new(Mutex::new(sink_writer)));
        self.byte_stream = Some(byte_stream);
        self.encoding = true;

        // 生成模拟的 SPS/PPS 数据
        // NOTE: 此处使用硬编码的 SPS/PPS 作为首次返回值，原因：
        // 1. IMFTransform 编码器在第一帧编码前无法获取真实的 SPS/PPS
        // 2. 客户端通常需要在编码开始时就获得 SPS/PPS 以初始化解码器
        // 3. 真实的 SPS/PPS 会在第一帧 IDR 帧编码后从编码器输出中获取
        // TODO: 后续应从编码器输出的第一帧 IDR 数据中解析真实 SPS/PPS 并缓存，
        //       替换此处的硬编码值，以确保与实际编码参数一致
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
    /// 编码后的数据，包含帧类型前缀 (0x01=SPS/PPS, 0x02=IDR, 0x03=P)
    /// 从内存 ByteStream 获取编码数据，提取 NAL 单元后返回
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

        // 从内存 ByteStream 获取编码数据
        if let Some(byte_stream) = &self.byte_stream {
            let all_data = byte_stream.get_new_data();

            // 从上次读取位置之后获取新数据
            if all_data.len() > self.last_read_position {
                let new_data = &all_data[self.last_read_position..];

                if !new_data.is_empty() {
                    // 提取 NAL 单元
                    let nal_units = extract_nal_units(new_data);

                    if !nal_units.is_empty() {
                        // 更新读取位置
                        self.last_read_position = all_data.len();

                        // 编码并返回带有帧类型前缀的数据
                        let encoded = self.encode_with_prefix(nal_units);
                        return Ok(Some(encoded));
                    }
                }
            }
        }

        Ok(None)
    }

    /// 编码 NAL 单元，添加帧类型前缀
    ///
    /// # 参数
    /// - nal_units: NAL 单元列表
    ///
    /// # 返回
    /// 带有帧类型前缀的数据：
    /// - 0x01 = SPS/PPS
    /// - 0x02 = IDR (关键帧)
    /// - 0x03 = P (预测帧)
    fn encode_with_prefix(&self, nal_units: Vec<Vec<u8>>) -> Vec<u8> {
        let mut result = Vec::new();
        for nal in nal_units {
            if let Some(nal_type) = get_nal_type(&nal) {
                match nal_type {
                    7 | 8 => {
                        // SPS/PPS
                        result.push(0x01);
                        // Annex-B 起始码：每个 NAL 单元前必须添加 0x00 0x00 0x00 0x01
                        result.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                        result.extend_from_slice(&nal);
                    }
                    5 => {
                        // IDR (关键帧)
                        result.push(0x02);
                        result.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                        result.extend_from_slice(&nal);
                    }
                    1 => {
                        // P (预测帧)
                        result.push(0x03);
                        result.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                        result.extend_from_slice(&nal);
                    }
                    _ => {
                        // 其他类型也添加，使用 0x03 前缀
                        result.push(0x03);
                        result.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                        result.extend_from_slice(&nal);
                    }
                }
            }
        }
        result
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

        // 清理资源
        self.sink_writer = None;
        self.texture_manager = None;
        self.byte_stream = None;
        self.encoding = false;

        // 注意：不在此处调用 MFShutdown()
        // MFStartup/MFShutdown 是引用计数的，MFSinkWriter 在 drop 时会清理资源
        // 如果 from_byte_stream() 失败，MFStartup 已执行但此处不会调用 MFShutdown
        // 因此由 MFSinkWriter 的 Drop 实现负责清理

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