# IMFByteStream 内存输出实现方案

> **创建日期**: 2026-06-08
> **状态**: 已批准
> **参考**: zq-platform/docs/superpowers/specs/2026-06-07-h264-streaming-design.md

## 1. 背景与目标

### 1.1 当前问题

- `StreamingEncoder.encode_frame()` 返回 `None`
- 编码器使用临时文件，无法直接获取编码数据
- 前端无法获取 H.264 流，降级到 JPEG 模式

### 1.2 目标

实现真正的内存输出，让 `encode_frame()` 返回 H.264 裸流数据

## 2. 架构设计

```
┌──────────────────────────────────────────────────────────────────────────┐
│                         StreamingEncoder                                 │
│  ┌─────────────────┐    ┌─────────────────┐    ┌────────���──────────┐   │
│  │ encode_frame() │───▶│ MFSinkWriter    │───▶│ IMFByteStream     │   │
│  │ (BGRA 输入)     │    │ (H.264 编码)    │    │ (内存输出)         │   │
│  └─────────────────┘    └─────────────────┘    └───────────────────┘   │
│                                                            │             │
│                                                            ▼             │
│                                                 ┌───────────────────┐    │
│                                                 │ Vec<u8> 缓冲区    │    │
│                                                 │ (编码后的 H.264)  │    │
│                                                 └───────────────────┘    │
└──────────────────────────────────────────────────────────────────────────┘
```

## 3. 实现方案

### 3.1 新增 IMFByteStream COM 实现

在 `memory_byte_stream.rs` 中使用 `windows-implement` 宏实现完整的 `IMFByteStream` 接口：

```rust
use windows::Win32::Media::MediaFoundation::IMFByteStream;
use windows::core::ComInterface;

#[windows_implement(IMFByteStream)]
impl MemoryByteStreamImpl {
    // 核心方法
    fn Write(&mut self, pb: *const u8, cb: u32) -> windows_core::Result<u32> { ... }
    fn Read(&mut self, pb: *mut u8, cb: u32, pcbread: *mut u32) -> windows_core::Result<()> { ... }
    fn Seek(&mut self, seekorigin: MFBYTESTREAM_SEEK_ORIGIN, llseekoffset: i64, dwseekflags: u32) -> windows_core::Result<u64> { ... }
    fn GetLength(&self) -> windows_core::Result<u64> { ... }
    fn SetLength(&mut self, qwlength: u64) -> windows_core::Result<()> { ... }
    fn Flush(&mut self) -> windows_core::Result<()> { ... }
    fn Close(&mut self) -> windows_core::Result<()> { ... }
    
    // 其他必需方法（可返回 E_NOTIMPL）
    fn GetCapabilities(&self) -> windows_core::Result<u32> { ... }
    fn IsEndOfStream(&self) -> windows_core::Result<BOOL> { ... }
    fn GetCurrentPosition(&self) -> windows_core::Result<u64> { ... }
    fn SetCurrentPosition(&mut self, qwposition: u64) -> windows_core::Result<()> { ... }
}
```

### 3.2 修改 MFSinkWriter

新增构造函数，支持传入 `IMFByteStream`：

```rust
impl MFSinkWriter {
    /// 从文件创建（现有方法）
    pub fn new(path: &str, device: &ID3D11Device, ...) -> Result<Self, RecorderError>
    
    /// 从 ByteStream 创建（新增）
    pub fn from_byte_stream(
        byte_stream: IMFByteStream,
        device: &ID3D11Device,
        width: u32,
        height: u32,
        fps: u32,
        audio: bool,
    ) -> Result<Self, RecorderError>
}
```

使用 `MFCreateSinkWriterFromURL`，传入 `IMFByteStream` 参数：

```rust
MFCreateSinkWriterFromURL(
    PCWSTR::null(),  // 不使用文件 URL
    Some(&byte_stream),  // 使用内存 ByteStream
    None,
    &mut sink_writer
)
```

### 3.3 修改 StreamingEncoder

使用新的 ByteStream 接口：

```rust
impl StreamingEncoder {
    pub fn start(&mut self) -> Result<Py<PyDict>, RecorderError> {
        // 创建内存 ByteStream
        let byte_stream = MemoryByteStream::new();
        
        // 使用 ByteStream 创建 SinkWriter
        let sink_writer = MFSinkWriter::from_byte_stream(
            byte_stream.as_raw(),
            &device,
            ...
        )?;
        
        // 保存 ByteStream ���用用于后续读取
        self.byte_stream = Some(byte_stream);
        ...
    }
    
    pub fn encode_frame(&mut self, frame_data: &[u8]) -> Result<Option<Vec<u8>>, RecorderError> {
        // 编码帧...
        
        // 从内存 ByteStream 获取新编码的数据
        if let Some(byte_stream) = &self.byte_stream {
            let new_data = byte_stream.get_new_data();
            if !new_data.is_empty() {
                let nal_units = extract_nal_units(&new_data);
                // 添加帧类型前缀并返回
                return Ok(Some(encode_with_prefix(nal_units)));
            }
        }
        
        Ok(None)
    }
}
```

## 4. 输出格式

完全遵循 zq-platform 的设计文档：

| 帧类型 | 前缀 | 内容 |
|--------|------|------|
| SPS/PPS | 0x01 | SPS NAL + PPS NAL（Annex-B 格式） |
| IDR 关键帧 | 0x02 | IDR NAL 单元 |
| P 帧 | 0x03 | P 帧 NAL 单元 |

```rust
// 编码格式示例
[0x01, 0x00, 0x00, 0x01, 0x67, ..., 0x00, 0x00, 0x01, 0x68, ...]  // SPS+PPS
[0x02, 0x00, 0x00, 0x01, 0x65, ...]  // IDR
[0x03, 0x00, 0x00, 0x01, 0x41, ...]  // P
```

## 5. 文件变更清单

| 文件 | 变更 |
|------|------|
| `Cargo.toml` | 添加 `windows-implement` crate 依赖 |
| `src/memory_byte_stream.rs` | 实现完整的 IMFByteStream COM 接口 |
| `src/mf_writer.rs` | 新增 `from_byte_stream()` 构造函数 |
| `src/streaming_encoder.rs` | 使用 ByteStream，从内存读取 NAL 单元 |
| `src/lib.rs` | 无变更（模块已注册） |

## 6. 验证清单

- [ ] `cargo build` 编译通过
- [ ] `encode_frame()` 返回非 None 的 H.264 数据
- [ ] 输出格式符合 Annex-B 规范
- [ ] 帧类型前缀正确（0x01/0x02/0x03）
- [ ] SPS/PPS 在 IDR 帧之前发送
- [ ] 集成测试：zq-platform 前端能正常解码播放