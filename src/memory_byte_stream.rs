//! 内存字节流实现
//!
//! 用于 Media Foundation 内存输出，将编码数据直接写入内存缓冲区
//! 替代临时文件方案

use std::sync::Arc;
use parking_lot::Mutex;

/// 内存字节流
///
/// 实现 IMFByteStream 接口，将数据写入内存缓冲区
/// 供 MFSinkWriter 使用以实现内存输出
pub struct MemoryByteStream {
    buffer: Arc<Mutex<Vec<u8>>>,
    position: u64,
    is_valid: bool,
}

impl MemoryByteStream {
    /// 创建新的内存字节流
    pub fn new() -> Self {
        Self {
            buffer: Arc::new(Mutex::new(Vec::new())),
            position: 0,
            is_valid: true,
        }
    }

    /// 获取 Arc 包装的缓冲区
    pub fn buffer(&self) -> Arc<Mutex<Vec<u8>>> {
        self.buffer.clone()
    }

    /// 获取当前缓冲区数据
    pub fn get_data(&self) -> Vec<u8> {
        let buffer = self.buffer.lock();
        if self.position >= buffer.len() as u64 {
            return Vec::new();
        }
        buffer[self.position as usize..].to_vec()
    }

    /// 清空缓冲区
    pub fn clear(&mut self) {
        let mut buffer = self.buffer.lock();
        buffer.clear();
        self.position = 0;
    }

    /// 重置位置
    pub fn reset(&mut self) {
        self.position = 0;
    }

    /// 检查是否有效
    pub fn is_valid(&self) -> bool {
        self.is_valid
    }
}

impl Default for MemoryByteStream {
    fn default() -> Self {
        Self::new()
    }
}

/// 内存字节流 COM 对象
#[repr(C)]
pub struct MFByteStream {
    // vtable 指针由 Windows COM 机制管理
    buffer: Arc<Mutex<Vec<u8>>>,
    position: u64,
    length: u64,
    is_valid: bool,
}

impl MFByteStream {
    pub fn new() -> Self {
        Self {
            buffer: Arc::new(Mutex::new(Vec::new())),
            position: 0,
            length: 0,
            is_valid: true,
        }
    }

    pub fn buffer(&self) -> Arc<Mutex<Vec<u8>>> {
        self.buffer.clone()
    }
}

impl Default for MFByteStream {
    fn default() -> Self {
        Self::new()
    }
}

// 由于 IMFByteStream 接口非常复杂，需要实现大量方法
// 这里提供一个简化方案：使用 MFCreateMemoryBuffer 替代自定义 ByteStream
// 然后从内存缓冲区读取编码数据

/// 从内存缓冲区提取 NAL 单元
#[allow(dead_code)]
pub fn extract_nal_units(data: &[u8]) -> Vec<Vec<u8>> {
    let mut nal_units = Vec::new();

    if data.len() < 5 {
        return nal_units;
    }

    // 查找 NAL 单元起始码: 0x00 0x00 0x00 0x01 或 0x00 0x00 0x01
    let mut start_idx: Option<usize> = None;

    for i in 0..data.len() - 3 {
        // 检测 4 字节起始码: 0x00 0x00 0x00 0x01
        if i + 4 <= data.len()
            && data[i] == 0x00
            && data[i+1] == 0x00
            && data[i+2] == 0x00
            && data[i+3] == 0x01 {

            if let Some(start) = start_idx {
                if start < i {
                    nal_units.push(data[start..i].to_vec());
                }
            }
            start_idx = Some(i + 4);
        }
        // 检测 3 字节起始码: 0x00 0x00 0x01
        else if data[i] == 0x00
            && data[i+1] == 0x00
            && data[i+2] == 0x01 {

            if let Some(start) = start_idx {
                if start < i {
                    nal_units.push(data[start..i].to_vec());
                }
            }
            start_idx = Some(i + 3);
        }
    }

    // 添加最后一个 NAL 单元
    if let Some(start) = start_idx {
        if start < data.len() {
            nal_units.push(data[start..].to_vec());
        }
    }

    nal_units
}

/// 检测 NAL 单元类型
#[allow(dead_code)]
pub fn get_nal_type(data: &[u8]) -> Option<u8> {
    if data.is_empty() {
        return None;
    }

    // 跳过起始码后获取 NAL 头
    let mut i = 0;
    while i < data.len() && data[i] == 0x00 {
        i += 1;
    }

    if i < data.len() && data[i] == 0x01 {
        i += 1;
    }

    if i < data.len() {
        // NAL 单元类型在低 5 位
        Some(data[i] & 0x1F)
    } else {
        None
    }
}

/// 判断是否为关键帧 (IDR)
#[allow(dead_code)]
pub fn is_key_frame(data: &[u8]) -> bool {
    if let Some(nal_type) = get_nal_type(data) {
        // IDR 帧的 NAL 类型为 5
        nal_type == 5
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_nal_units() {
        // 模拟 H.264 比特流，包含 SPS、PPS 和一个 IDR 帧
        let data = vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0x00, // SPS
            0x00, 0x00, 0x00, 0x01, 0x68, 0x00, 0xF8,             // PPS
            0x00, 0x00, 0x00, 0x01, 0x65, 0x41, 0xFF, 0xFF,       // IDR
        ];

        let nal_units = extract_nal_units(&data);
        assert!(nal_units.len() >= 2);
    }

    #[test]
    fn test_is_key_frame() {
        // IDR 帧
        let idr = vec![0x00, 0x00, 0x00, 0x01, 0x65];
        assert!(is_key_frame(&idr));

        // 非 IDR 帧
        let non_idr = vec![0x00, 0x00, 0x00, 0x01, 0x41];
        assert!(!is_key_frame(&non_idr));
    }
}