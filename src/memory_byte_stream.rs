//! 内存字节流实现
//!
//! 用于 Media Foundation 内存输出，将编码数据直接写入内存缓冲区
//! 替代临时文件方案

use std::sync::Arc;
use parking_lot::Mutex;

#[cfg(windows)]
use windows::{
    core::*,
    Win32::Foundation::*,
    Win32::Media::MediaFoundation::*,
    Win32::System::Com::*,
};

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

    /// 获取 COM 接口指针 (IMFByteStream)
    ///
    /// 使用 windows-implement 宏创建的对象会自动实现 ComInterface trait，
    /// 可以通过 .cast() 方法转换为对应的 COM 接口类型
    #[cfg(windows)]
    pub fn as_raw(&self) -> windows::Win32::Media::MediaFoundation::IMFByteStream {
        use windows::core::ComInterface;
        self.cast().expect("MemoryByteStream 应该能转换为 IMFByteStream")
    }
}

impl Default for MemoryByteStream {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
#[implement(IMFByteStream)]
impl MemoryByteStream {
    /// 获取字节流能力
    ///
    /// 支持读取、写入和查找
    fn GetCapabilities(&self) -> Result<u32> {
        // MFBYTESTREAM_IS_SEEKABLE | MFBYTESTREAM_IS_READABLE | MFBYTESTREAM_IS_WRITABLE
        Ok(MFBYTESTREAM_IS_SEEKABLE.0 | MFBYTESTREAM_IS_READABLE.0 | MFBYTESTREAM_IS_WRITABLE.0)
    }

    /// 获取流长度
    fn GetLength(&self) -> Result<u64> {
        let inner = self.inner.lock();
        Ok(inner.buffer.len() as u64)
    }

    /// 设置流长度（本实现不支持扩展或截断）
    fn SetLength(&self, _qwlength: u64) -> Result<()> {
        // 不支持设置长度，直接返回成功
        Ok(())
    }

    /// 获取当前读取/写入位置
    fn GetCurrentPosition(&self) -> Result<u64> {
        let inner = self.inner.lock();
        Ok(inner.position)
    }

    /// 设置当前读取/写入位置
    fn SetCurrentPosition(&self, qwposition: u64) -> Result<()> {
        let mut inner = self.inner.lock();
        inner.position = qwposition;
        Ok(())
    }

    /// 检查是否到达流末尾
    fn IsEndOfStream(&self) -> Result<BOOL> {
        let inner = self.inner.lock();
        Ok(BOOL::from(inner.position >= inner.buffer.len() as u64))
    }

    /// 读取数据
    ///
    /// 从当前 position 读取最多 cb 字节的数据
    fn Read(&self, pb: *mut u8, cb: u32, pcbread: *mut u32) -> Result<()> {
        if pb.is_null() || pcbread.is_null() {
            return Err(E_POINTER.into());
        }

        let mut inner = self.inner.lock();

        // 先检查 position 是否超出 buffer 长度，避免整数溢出
        let buffer_len = inner.buffer.len() as u64;
        let pos = inner.position;
        if pos >= buffer_len {
            unsafe { *pcbread = 0; }
            return Ok(());
        }
        let available = (buffer_len - pos) as u32;
        let to_read = cb.min(available);

        if to_read > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    inner.buffer.as_ptr().add(pos as usize),
                    pb,
                    to_read as usize,
                );
            }
            inner.position += to_read as u64;
        }

        unsafe { *pcbread = to_read; }
        Ok(())
    }

    /// 写入数据
    ///
    /// 从当前位置写入数据
    fn Write(&self, pb: *const u8, cb: u32) -> Result<u32> {
        if pb.is_null() {
            return Err(E_POINTER.into());
        }

        let mut inner = self.inner.lock();

        if inner.position < inner.buffer.len() as u64 {
            // 覆盖模式：当前位置在已有数据范围内
            let overwrite_len = (cb as usize).min(inner.buffer.len() - inner.position as usize);
            unsafe {
                std::ptr::copy_nonoverlapping(
                    pb,
                    inner.buffer.as_mut_ptr().add(inner.position as usize),
                    overwrite_len,
                );
            }
            inner.position += overwrite_len as u64;

            // 如果写入的数据超过现有数据范围，需要追加剩余部分
            if cb > overwrite_len as u32 {
                let remaining_offset = overwrite_len;
                unsafe {
                    let remaining_slice = std::slice::from_raw_parts(
                        pb.add(remaining_offset),
                        (cb as usize) - remaining_offset,
                    );
                    inner.buffer.extend_from_slice(remaining_slice);
                }
                inner.position += (cb - overwrite_len as u32) as u64;
            }

            return Ok(cb);
        }

        // 追加模式：当前位置在缓冲区末尾
        unsafe {
            let slice = std::slice::from_raw_parts(pb, cb as usize);
            inner.buffer.extend_from_slice(slice);
        }
        inner.position += cb as u64;
        Ok(cb)
    }

    /// 查找操作
    fn Seek(&self, seekorigin: MFBYTESTREAM_SEEK_ORIGIN, llseekoffset: i64, _dwseekflags: u32) -> Result<u64> {
        let mut inner = self.inner.lock();

        let new_pos = match seekorigin.0 {
            0 => { // Begin
                llseekoffset.max(0) as u64
            }
            1 => { // Current
                (inner.position as i64 + llseekoffset).max(0) as u64
            }
            2 => { // End
                (inner.buffer.len() as i64 + llseekoffset).max(0) as u64
            }
            _ => inner.position,
        };

        inner.position = new_pos.min(inner.buffer.len() as u64);
        Ok(inner.position)
    }

    /// 刷新数据（本实现无需刷新操作）
    fn Flush(&self) -> Result<()> {
        Ok(())
    }

    /// 关闭字节流
    fn Close(&self) -> Result<()> {
        let mut inner = self.inner.lock();
        inner.is_valid = false;
        Ok(())
    }

    // 异步方法 - 本实现不支持异步操作，返回 E_NOTIMPL
    fn BeginRead(&self, _pb: *mut u8, _cb: u32, _pcallback: Option<&IMFAsyncCallback>, _punkstate: Option<&IUnknown>) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn EndRead(&self, _presult: Option<&IMFAsyncResult>) -> Result<u32> {
        Err(E_NOTIMPL.into())
    }

    fn BeginWrite(&self, _pb: *const u8, _cb: u32, _pcallback: Option<&IMFAsyncCallback>, _punkstate: Option<&IUnknown>) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn EndWrite(&self, _presult: Option<&IMFAsyncResult>) -> Result<u32> {
        Err(E_NOTIMPL.into())
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