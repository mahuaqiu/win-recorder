//! 内存字节流实现
//!
//! 用于 Media Foundation 内存输出，将编码数据直接写入内存缓冲区
//! 替代临时文件方案

use parking_lot::Mutex;
use std::sync::Arc;

/// 内存字节流内部状态
///
/// 将所有状态合并到单个结构体中，避免多个 Mutex 导致的过度拆锁和潜在死锁
struct Inner {
    buffer: Vec<u8>,
    position: u64,
    is_valid: bool,
}

/// 内存字节流
///
/// 实现 IMFByteStream 接口，将数据写入内存缓冲区
/// 供 MFSinkWriter 使用以实现内存输出
#[allow(dead_code)]
pub struct MemoryByteStream {
    inner: Arc<Mutex<Inner>>,
}

#[allow(dead_code)]
impl MemoryByteStream {
    /// 创建新的内存字节流
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                buffer: Vec::new(),
                position: 0u64,
                is_valid: true,
            })),
        }
    }

    /// 获取 Arc 包装的缓冲区
    pub fn buffer(&self) -> Arc<Mutex<Vec<u8>>> {
        // 为保持向后兼容，将 buffer 克隆到独立的 Arc<Mutex<Vec<u8>>>
        let inner = self.inner.lock();
        Arc::new(Mutex::new(inner.buffer.clone()))
    }

    /// 获取当前缓冲区数据（从当前位置到末尾）
    pub fn get_data(&self) -> Vec<u8> {
        let inner = self.inner.lock();
        if inner.position >= inner.buffer.len() as u64 {
            return Vec::new();
        }
        inner.buffer[inner.position as usize..].to_vec()
    }

    /// 获取所有数据（从位置 0 开始）
    pub fn get_new_data(&self) -> Vec<u8> {
        let inner = self.inner.lock();
        inner.buffer.clone()
    }

    /// 清空缓冲区
    pub fn clear(&mut self) {
        let mut inner = self.inner.lock();
        inner.buffer.clear();
        inner.position = 0;
    }

    /// 重置位置
    pub fn reset(&mut self) {
        let mut inner = self.inner.lock();
        inner.position = 0;
    }

    /// 检查是否有效
    pub fn is_valid(&self) -> bool {
        let inner = self.inner.lock();
        inner.is_valid
    }

    // 自定义 IMFByteStream 方案已废弃，当前推流模块直接从 MFT 输出 sample 读取数据。
}

impl Default for MemoryByteStream {
    fn default() -> Self {
        Self::new()
    }
}

// 自定义 IMFByteStream 接口非常复杂，当前保留 MemoryByteStream 的普通内存缓冲能力，
// H264 推流则使用 MFCreateMemoryBuffer 并直接读取编码器输出 sample。

/// 从内存缓冲区提取 NAL 单元
#[allow(dead_code)]
pub fn extract_nal_units(data: &[u8]) -> Vec<Vec<u8>> {
    if has_annex_b_start_code(data) {
        return extract_annex_b_nal_units(data);
    }

    extract_length_prefixed_nal_units(data)
}

fn has_annex_b_start_code(data: &[u8]) -> bool {
    data.windows(3).any(|w| w == [0x00, 0x00, 0x01])
        || data.windows(4).any(|w| w == [0x00, 0x00, 0x00, 0x01])
}

fn extract_annex_b_nal_units(data: &[u8]) -> Vec<Vec<u8>> {
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
            && data[i + 1] == 0x00
            && data[i + 2] == 0x00
            && data[i + 3] == 0x01
        {
            if let Some(start) = start_idx {
                if start < i {
                    nal_units.push(data[start..i].to_vec());
                }
            }
            start_idx = Some(i + 4);
        }
        // 检测 3 字节起始码: 0x00 0x00 0x01
        else if data[i] == 0x00 && data[i + 1] == 0x00 && data[i + 2] == 0x01 {
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

fn extract_length_prefixed_nal_units(data: &[u8]) -> Vec<Vec<u8>> {
    let mut nal_units = Vec::new();
    let mut offset = 0usize;

    while offset + 4 <= data.len() {
        let nal_len = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        offset += 4;

        if nal_len == 0 || offset + nal_len > data.len() {
            return Vec::new();
        }

        nal_units.push(data[offset..offset + nal_len].to_vec());
        offset += nal_len;
    }

    if offset == data.len() {
        nal_units
    } else {
        Vec::new()
    }
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
            0x00, 0x00, 0x00, 0x01, 0x68, 0x00, 0xF8, // PPS
            0x00, 0x00, 0x00, 0x01, 0x65, 0x41, 0xFF, 0xFF, // IDR
        ];

        let nal_units = extract_nal_units(&data);
        assert!(nal_units.len() >= 2);
    }

    #[test]
    fn test_extract_length_prefixed_nal_units() {
        let data = vec![
            0x00, 0x00, 0x00, 0x04, 0x67, 0x42, 0x00, 0x1E, 0x00, 0x00, 0x00, 0x03, 0x68, 0x00,
            0xF8,
        ];

        let nal_units = extract_nal_units(&data);

        assert_eq!(nal_units.len(), 2);
        assert_eq!(nal_units[0], vec![0x67, 0x42, 0x00, 0x1E]);
        assert_eq!(nal_units[1], vec![0x68, 0x00, 0xF8]);
    }

    #[test]
    fn test_reject_invalid_length_prefixed_nal_units() {
        let data = vec![0x00, 0x00, 0x00, 0x10, 0x67];

        assert!(extract_nal_units(&data).is_empty());
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
