# win-recorder

Windows 硬编录屏库（Rust + Python 绑定）

## 特性

- 🚀 **GPU 管线硬编**：D3D11 + Media Foundation，CPU 占用 <5%
- ⚡ **零拷贝传输**：PyByteArray slice 直接引用，Python→Rust 无内存拷贝
- 📦 **体积极小**：DLL < 1MB，无 FFmpeg 依赖
- 🎬 **高性能录制**：支持 30fps + 4K
- 🔊 **可选音频**：v0.2.0 支持 WASAPI LOOPBACK 音频捕获

## 安装

```bash
pip install win-recorder
```

## 使用示例

```python
import win_recorder
import mss

# 创建录制器
recorder = win_recorder.WinRecorder(
    output_path="output.mp4",
    fps=30,
    audio=False,
    monitor=1,  # 主屏幕
)

# 开始录制
recorder.start()

# 获取对齐后的分辨率（编码器实际使用的尺寸）
aligned_width = recorder.width
aligned_height = recorder.height

# 截屏并录制
with mss.mss() as sct:
    monitor_config = sct.monitors[1]
    
    for _ in range(100):  # 录制 100 帧
        screenshot = sct.grab(monitor_config)
        frame_data = bytearray(screenshot.raw)
        
        # 如果分辨率需要对齐，扩展帧数据
        # （详见下面的分辨率对齐说明）
        recorder.write_frame(frame_data)

# 结束录制
recorder.stop()
```

## Monitor 参数

| monitor | 说明 |
|---------|------|
| `1` | 主屏幕（left=0） |
| `2` | 副屏幕 |

## API

| 方法 | 说明 |
|------|------|
| `WinRecorder(output_path, fps=30, audio=False, monitor=1)` | 创建录制器 |
| `get_monitor_size(monitor)` | 静态方法，获取显示器尺寸 |
| `start()` | 开始录制 |
| `write_frame(frame_data)` | 写入 BGRA 帧数据 |
| `stop()` | 结束录制 |
| `width` / `height` | 对齐后的分辨率（getter） |
| `fps` | 帧率（getter） |
| `is_recording` | 是否正在录制（getter） |

## 分辨率对齐

H264 硬编码器要求分辨率是 16 的倍数。如果显示器分辨率不是 16 的倍数（如 1920×1080），编码器会自动对齐（1080 → 1088）。

**处理方法**：
```python
orig_width, orig_height = win_recorder.WinRecorder.get_monitor_size(1)
recorder.start()

# 使用对齐后的分辨率扩展帧数据
aligned_width = recorder.width  # 可能是 1920
aligned_height = recorder.height  # 可能是 1088（1080 对齐）

if aligned_height > orig_height:
    # 扩展帧数据，填充黑边
    aligned_frame = bytearray(aligned_width * aligned_height * 4)
    # 复制原始数据...
    recorder.write_frame(aligned_frame)
```

## 构建

```bash
# 开发模式
maturin develop

# 发布模式
maturin build --release
```

## 技术架构

```
Python (BGRA bytearray)
    │
    ▼ PyO3 (PyByteArray.as_bytes())
Rust (&[u8] slice)
    │
    ▼ D3D11 Staging Texture
GPU Texture (DEFAULT + SHARED)
    │
    ▼ MFCreateDXGISurfaceBuffer
IMFSample
    │
    ▼ IMFSinkWriter
    │   ├── Input: MFVideoFormat_RGB32
    │   ├── [Color Converter MFT] (GPU BGRA→NV12)
    │   └── [H.264 Encoder MFT] (NVENC/QSV)
MP4 File
```

## License

MIT