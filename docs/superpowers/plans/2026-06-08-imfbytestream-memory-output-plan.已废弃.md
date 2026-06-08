# IMFByteStream 内存输出实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现真正的内存输出，让 `StreamingEncoder.encode_frame()` 返回 H.264 裸流数据

**Architecture:** 通过实现 IMFByteStream COM 接口，将编码数据直接写入内存缓冲区，替代临时文件方案

**Tech Stack:** Rust, windows-rs 0.58, windows-implement crate, PyO3

---

## 文件变更清单

| 文件 | 变更 |
|------|------|
| `Cargo.toml` | 添加 `windows-implement` crate 依赖 |
| `src/memory_byte_stream.rs` | 实现完整的 IMFByteStream COM 接口 |
| `src/mf_writer.rs` | 新增 `from_byte_stream()` 构造函数 |
| `src/streaming_encoder.rs` | 使用 ByteStream，从内存读取 NAL 单元 |

---

## 实现步骤

### Task 1: 添加 windows-implement 依赖

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: 添加 windows-implement 依赖**

在 `[dependencies]` 中添加：

```toml
[target.'cfg(windows)'.dependencies]
windows-implement = "0.58"
```

---

### Task 2: 实现 IMFByteStream COM 接口

**Files:**
- Modify: `src/memory_byte_stream.rs`

关键点：
- 使用 `#[windows_implement(IMFByteStream)]` 宏
- 实现核心方法：Write, Read, Seek, GetLength, Flush, Close
- 可选方法返回 `E_NOTIMPL`：BeginRead, EndRead, BeginWrite, EndWrite

- [ ] **Step 1: 添加必要的导入**

```rust
#[cfg(windows)]
use windows::{
    core::*,
    Win32::Media::MediaFoundation::*,
    Win32::System::Com::*,
};
```

- [ ] **Step 2: 重写 MemoryByteStream 结构体，使用 windows-implement**

```rust
#[cfg(windows)]
#[windows_implement(IMFByteStream)]
impl MemoryByteStream {
    pub fn new() -> Self {
        Self {
            buffer: Arc::new(Mutex::new(Vec::new())),
            position: Arc::new(Mutex::new(0u64)),
            is_valid: Arc::new(Mutex::new(true)),
        }
    }

    pub fn buffer(&self) -> Arc<Mutex<Vec<u8>>> {
        self.buffer.clone()
    }

    fn GetCapabilities(&self) -> Result<u32> {
        // MFBYTESTREAM_IS_SEEKABLE | MFBYTESTREAM_IS_READABLE | MFBYTESTREAM_IS_WRITABLE
        Ok(0x3)
    }

    fn GetLength(&self) -> Result<u64> {
        let buffer = self.buffer.lock();
        Ok(buffer.len() as u64)
    }

    fn SetLength(&self, _qwlength: u64) -> Result<()> {
        // 可选实现，当前不需要
        Ok(())
    }

    fn GetCurrentPosition(&self) -> Result<u64> {
        let pos = self.position.lock();
        Ok(*pos)
    }

    fn SetCurrentPosition(&self, qwposition: u64) -> Result<()> {
        let mut pos = self.position.lock();
        *pos = qwposition;
        Ok(())
    }

    fn IsEndOfStream(&self) -> Result<BOOL> {
        let buffer = self.buffer.lock();
        let pos = self.position.lock();
        Ok(BOOL::from(*pos >= buffer.len() as u64))
    }

    fn Read(&self, pb: *mut u8, cb: u32, pcbread: *mut u32) -> Result<()> {
        if pb.is_null() || pcbread.is_null() {
            return Ok(());
        }

        let mut buffer = self.buffer.lock();
        let mut position = self.position.lock();

        let available = (buffer.len() as u64 - *position) as u32;
        let to_read = cb.min(available);

        if to_read > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    buffer.as_ptr().add(*position as usize),
                    pb,
                    to_read as usize,
                );
            }
            *position += to_read as u64;
        }

        unsafe { *pcbread = to_read; }
        Ok(())
    }

    fn Write(&self, pb: *const u8, cb: u32) -> Result<u32> {
        if pb.is_null() {
            return Ok(0);
        }

        let mut buffer = self.buffer.lock();
        let mut position = self.position.lock();

        if *position < buffer.len() as u64 {
            // 覆盖模式：写入到当前位置
            let overwrite_len = (cb as usize).min(buffer.len() - *position as usize);
            unsafe {
                std::ptr::copy_nonoverlapping(
                    pb,
                    buffer.as_mut_ptr().add(*position as usize),
                    overwrite_len,
                );
            }
            *position += overwrite_len as u64;
            return Ok(overwrite_len as u32);
        }

        // 追加模式
        unsafe {
            let slice = std::slice::from_raw_parts(pb, cb as usize);
            buffer.extend_from_slice(slice);
        }
        *position += cb as u64;
        Ok(cb)
    }

    fn Seek(&self, seekorigin: MFBYTESTREAM_SEEK_ORIGIN, llseekoffset: i64, _dwseekflags: u32) -> Result<u64> {
        let mut buffer = self.buffer.lock();
        let mut position = self.position.lock();

        let new_pos = match seekorigin {
            MFBYTESTREAM_SEEK_ORIGIN::Current => {
                (*position as i64 + llseekoffset).max(0) as u64
            }
            MFBYTESTREAM_SEEK_ORIGIN::Begin => {
                llseekoffset.max(0) as u64
            }
            MFBYTESTREAM_SEEK_ORIGIN::End => {
                (buffer.len() as i64 + llseekoffset).max(0) as u64
            }
            _ => *position,
        };

        *position = new_pos.min(buffer.len() as u64);
        Ok(*position)
    }

    fn Flush(&self) -> Result<()> {
        // 当前实现无需刷新
        Ok(())
    }

    fn Close(&self) -> Result<()> {
        let mut is_valid = self.is_valid.lock();
        *is_valid = false;
        Ok(())
    }

    // 异步方法可以返回 E_NOTIMPL
    fn BeginRead(&self, _pb: *mut u8, _cb: u32, _pcallback: Option<&IMFAsyncCallback>, _punkstate: Option<&IUnknown>) -> Result<()> {
        Err(Error::from_win32())
    }

    fn EndRead(&self, _presult: Option<&IMFAsyncResult>) -> Result<u32> {
        Err(Error::from_win32())
    }

    fn BeginWrite(&self, _pb: *const u8, _cb: u32, _pcallback: Option<&IMFAsyncCallback>, _punkstate: Option<&IUnknown>) -> Result<()> {
        Err(Error::from_win32())
    }

    fn EndWrite(&self, _presult: Option<&IMFAsyncResult>) -> Result<u32> {
        Err(Error::from_win32())
    }
}
```

- [ ] **Step 3: 添加辅助方法用于获取新数据**

```rust
impl MemoryByteStream {
    /// 获取自上次读取后的新数据
    pub fn get_new_data(&self) -> Vec<u8> {
        let buffer = self.buffer.lock();
        // 返回所有数据，由调用方去重
        buffer.clone()
    }

    /// 清空缓冲区
    pub fn clear(&self) {
        let mut buffer = self.buffer.lock();
        buffer.clear();
        let mut position = self.position.lock();
        *position = 0;
    }
}
```

- [ ] **Step 4: 添加 Default 实现**

```rust
impl Default for MemoryByteStream {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 5: 提交变更**

```bash
git add src/memory_byte_stream.rs Cargo.toml
git commit -m "feat: 实现 IMFByteStream COM 接口用于内存输出"
```

---

### Task 3: 新增 MFSinkWriter from_byte_stream 构造函数

**Files:**
- Modify: `src/mf_writer.rs`

- [ ] **Step 1: 添加 from_byte_stream 构造函数**

在 `MFSinkWriter` impl 块中添加新方法：

```rust
/// 从 ByteStream 创建 SinkWriter（内存输出）
///
/// # 参数
/// - byte_stream: IMFByteStream 内存流
/// - device: D3D11 设备
/// - width: 视频宽度
/// - height: 视频高度
/// - fps: 帧率
/// - audio: 是否包含音频
pub fn from_byte_stream(
    byte_stream: IMFByteStream,
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

    // 分辨率对齐：宽高必须是 16 的倍数
    let aligned_width = (width + 15) & !15;
    let aligned_height = (height + 15) & !15;

    unsafe {
        // 启动 Media Foundation
        MFStartup(MFSTARTUP_LITE, 0)
            .map_err(|e| RecorderError::MFError(format!("MFStartup 失败: {}", e)))?;

        // 使用 MFCreateSinkWriterFromURL，传入 ByteStream
        let sink_writer = MFCreateSinkWriterFromURL(
            PCWSTR::null(),  // 不使用文件 URL
            Some(&byte_stream),  // 使用内存 ByteStream
            None::<&IMFAttributes>,
        )
        .map_err(|e| RecorderError::MFError(format!("创建 SinkWriter 失败: {}", e)))?;

        // ... 其余配置与 new() 方法相同 ...

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
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| RecorderError::MFError(format!("设置输出交错模式失败: {}", e)))?;

        output_type
            .SetUINT64(&MF_MT_FRAME_SIZE, ((aligned_width as u64) << 32) | (aligned_height as u64))
            .map_err(|e| RecorderError::MFError(format!("设置输出帧大小失败: {}", e)))?;

        output_type
            .SetUINT64(&MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1u64)
            .map_err(|e| RecorderError::MFError(format!("设置输出帧率失败: {}", e)))?;

        output_type
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1u64)
            .map_err(|e| RecorderError::MFError(format!("设置输出像素宽高比失败: {}", e)))?;

        let bitrate = if aligned_width >= 3840 {
            8000000
        } else if aligned_width >= 1920 {
            5000000
        } else {
            2000000
        };
        output_type
            .SetUINT32(&MF_MT_AVG_BITRATE, bitrate)
            .map_err(|e| RecorderError::MFError(format!("设置输出码率失败: {}", e)))?;

        let stream_index = sink_writer
            .AddStream(&output_type)
            .map_err(|e| RecorderError::MFError(format!("添加流失败: {}", e)))?;

        // 设置输入类型
        let input_type = MFCreateMediaType()
            .map_err(|e| RecorderError::MFError(format!("创建 Input MediaType 失败: {}", e)))?;

        input_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| RecorderError::MFError(format!("设置输入主类型失败: {}", e)))?;

        input_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
            .map_err(|e| RecorderError::MFError(format!("设置输入子类型失败: {}", e)))?;

        input_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| RecorderError::MFError(format!("设置输入交错模式失败: {}", e)))?;

        input_type
            .SetUINT64(&MF_MT_FRAME_SIZE, ((aligned_width as u64) << 32) | (aligned_height as u64))
            .map_err(|e| RecorderError::MFError(format!("设置输入帧大小失败: {}", e)))?;

        input_type
            .SetUINT64(&MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1u64)
            .map_err(|e| RecorderError::MFError(format!("设置输入帧率失败: {}", e)))?;

        input_type
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1u64)
            .map_err(|e| RecorderError::MFError(format!("设置输入像素宽高比失败: {}", e)))?;

        input_type
            .SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)
            .map_err(|e| RecorderError::MFError(format!("设置样本独立属性失败: {}", e)))?;

        let stride = (aligned_width * 4) as i32 as u32;
        input_type
            .SetUINT32(&MF_MT_DEFAULT_STRIDE, stride)
            .map_err(|e| RecorderError::MFError(format!("设置默认 stride 失败: {}", e)))?;

        sink_writer
            .SetInputMediaType(stream_index, &input_type, None)
            .map_err(|e| RecorderError::MFError(format!("设置输入类型失败: {}", e)))?;

        let frame_duration = 10_000_000_i64 / fps as i64;

        Ok(Self {
            sink_writer,
            stream_index,
            frame_duration,
            frame_count: 0,
            width: aligned_width,
            height: aligned_height,
        })
    }
}
```

- [ ] **Step 2: 添加获取编码数据的方法**

在 `MFSinkWriter` 中添加：

```rust
/// 获取 IMFByteStream 引用（用于读取编码数据）
pub fn byte_stream(&self) -> Option<&IMFByteStream> {
    // 需要保存 byte_stream 引用
    None  // TODO: 实现
}
```

注意：这需要修改 `MFSinkWriter` 结构体，添加保存 `IMFByteStream` 的字段。

- [ ] **Step 3: 提交变更**

```bash
git add src/mf_writer.rs
git commit -m "feat: 添加 MFSinkWriter::from_byte_stream() 构造函数"
```

---

### Task 4: 修改 StreamingEncoder 使用 ByteStream

**Files:**
- Modify: `src/streaming_encoder.rs`

- [ ] **Step 1: 修改 start() 方法使用 ByteStream**

将现有的临时文件方案替换为 ByteStream：

```rust
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
        byte_stream.as_raw(),  // 需要实现 as_raw() 或类似方法
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
    self.byte_stream = Some(byte_stream);
    self.output_buffer.clear();
    self.sps_pps_sent = false;
    self.frame_count = 0;

    self.texture_manager = Some(Arc::new(texture_manager));
    self.sink_writer = Some(Arc::new(Mutex::new(sink_writer)));
    self.encoding = true;

    // 生成 SPS/PPS 数据（需要从编码器提取真实数据）
    self.sps_data = vec![
        0x00, 0x00, 0x00, 0x01,
        0x67, 0x42, 0x00, 0x1e, 0x00, 0x80, 0x05, 0x65, 0x94,
    ];
    self.pps_data = vec![
        0x00, 0x00, 0x00, 0x01,
        0x68, 0x00, 0xf8, 0x00,
    ];

    // ... 返回信息
}
```

- [ ] **Step 2: 修改 encode_frame() 方法获取编码数据**

```rust
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

    // 从内存 ByteStream 获取编码数据
    if let Some(byte_stream) = &self.byte_stream {
        let encoded_data = byte_stream.get_new_data();
        if !encoded_data.is_empty() {
            // 提取 NAL 单元
            let nal_units = extract_nal_units(&encoded_data);
            if !nal_units.is_empty() {
                // 添加帧类型前缀
                return Ok(Some(self.encode_with_prefix(nal_units)));
            }
        }
    }

    Ok(None)
}
```

- [ ] **Step 3: 添加 encode_with_prefix 方法**

```rust
fn encode_with_prefix(&self, nal_units: Vec<Vec<u8>>) -> Vec<u8> {
    let mut result = Vec::new();

    for nal in nal_units {
        let nal_type = get_nal_type(&nal).unwrap_or(0);
        match nal_type {
            7 => {
                // SPS
                result.push(0x01);
                result.extend_from_slice(&nal);
            }
            8 => {
                // PPS
                result.push(0x01);
                result.extend_from_slice(&nal);
            }
            5 => {
                // IDR
                result.push(0x02);
                result.extend_from_slice(&nal);
            }
            1 => {
                // P frame
                result.push(0x03);
                result.extend_from_slice(&nal);
            }
            _ => {
                // 其他类型，跳过或添加
            }
        }
    }

    result
}
```

- [ ] **Step 4: 修改 stop() 方法清理 ByteStream**

```rust
pub fn stop(&mut self) -> Result<(), RecorderError> {
    if !self.encoding {
        return Ok(());
    }

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

    // 关闭 Media Foundation
    unsafe {
        let _ = MFShutdown();
    }

    Ok(())
}
```

- [ ] **Step 5: 添加 byte_stream 字段**

在 `StreamingEncoder` 结构体中添加：

```rust
/// 内存 ByteStream
byte_stream: Option<MemoryByteStream>,
```

- [ ] **Step 6: 提交变更**

```bash
git add src/streaming_encoder.rs
git commit -m "feat: StreamingEncoder 使用 ByteStream 实现内存输出"
```

---

## 验证清单

- [ ] `cargo build --release` 编译通过
- [ ] `encode_frame()` 返回非 None 的 H.264 数据
- [ ] 输出格式符合 Annex-B 规范
- [ ] 帧类型前缀正确（0x01/0x02/0x03）
- [ ] SPS/PPS 在 IDR 帧之前发送
- [ ] 集成测试：zq-platform 前端能正常解码播放