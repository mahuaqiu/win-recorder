# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概述

win-recorder 是一个 Windows 硬件加速屏幕录制库，使用 Rust 编写并通过 PyO3 提供Python绑定。核心依赖 D3D11 和 Windows Media Foundation 实现 GPU 视频编码，输出 H.264 MP4 文件。

**平台限制**：仅支持 Windows，所有核心功能依赖 Win32 API（D3D11、DXGI、Media Foundation）。

## 构建与开发命令

```bash
# 开发模式（编译 Rust cdylib 并安装到当前 Python venv）
maturin develop

# Release 模式构建 wheel
maturin build --release

# 运行测试（手动脚本，非 pytest）
python tests/test_minimal.py
python tests/test_basic.py
python tests/test_recorder.py
```

没有 Rust 级别的单元测试（无 `#[test]`），测试完全通过 Python 脚本进行。

## 架构

数据流水线（文件录制）：
```
Python (BGRA bytearray)
  → PyO3 零拷贝 (PyByteArray.as_bytes())
  → D3D11 Staging Texture (CPU 写入)
  → GPU Texture (DEFAULT + SHARED, CopyResource)
  → IMFMediaBuffer (MFCreateMemoryBuffer, 经 CPU 回读)
  → IMFSample → IMFSinkWriter
  → [Color Converter MFT: BGRA → NV12] → [H.264 Encoder MFT]
  → MP4 文件
```

实时推流流水线（H264Encoder）：
```
Python (BGRA bytearray)
  → PyO3 零拷贝
  → IMFTransform Pipeline
  → [Color Converter MFT: BGRA → NV12] → [H.264 Encoder MFT]
  → 内存输出（带帧类型前缀）
```

### 模块结构

- `src/lib.rs` — PyO3 模块入口，注册 `win_recorder` Python 模块和 `WinRecorder`、`H264Encoder` 类
- `src/recorder.rs` — `WinRecorder` PyO3 类，核心 API：`new()`/`start()`/`write_frame()`/`stop()`/`get_monitor_size()`；内部使用 `Arc<D3D11TextureManager>` 和 `Arc<Mutex<MFSinkWriter>>`
- `src/h264_encoder.rs` — `H264Encoder` PyO3 类，基于 IMFTransform 的实时 H.264 编码器，用于推流场景。直接使用 MFT 管线（颜色转换 MFT + H264 编码 MFT），输出带帧类型前缀的 Annex-B 格式数据
- `src/d3d11.rs` — `D3D11TextureManager`，双纹理架构（Staging + GPU），处理 BGRA 上传和 MF Sample 创建；`detect_monitor()` 通过 DXGI 枚举显示器
- `src/mf_writer.rs` — `MFSinkWriter` 封装，配置 H.264 编码（分辨率对齐到 16 像素倍数，按分辨率分级设置码率），管理 Sample 时间戳
- `src/memory_byte_stream.rs` — 内存 ByteStream 工具，提供 NAL 单元提取和类型检测功能
- `src/error.rs` — `RecorderError` 枚举（thiserror 派生），映射到 `PyValueError`/`PyRuntimeError`

### 关键实现细节

- **分辨率对齐**：宽高向上取整到 16 的倍数（`(w + 15) & !15`），Python 侧需要手动填充 BGRA 帧到对齐后的尺寸
- **码率分级**：4K → 8 Mbps，1080p → 5 Mbps，≤720p → 2 Mbps
- **显示器编号**：monitor=1 为主显示器（left=0），monitor=2 为副显示器
- **音频未实现**：`audio` 参数存在但返回 `InvalidParam` 错误，WASAPI 音频捕获计划在 v0.2.0
- **推流编码**：使用 `H264Encoder` 实时编码推流，输出格式为帧类型前缀 + Annex-B NAL 单元（0x01=SPS/PPS, 0x02=IDR, 0x03=P帧）

## 版本号同步

**重要**：每次发布新版本时，需要同时更新以下位置：

| 文件 | 字段 |
|------|------|
| `Cargo.toml` | `version` |
| `pyproject.toml` | `[project].version` |

### 已知问题

- README 中的架构图描述了 `MFCreateDXGISurfaceBuffer`（零拷贝 GPU 路径），但实际代码使用 `MFCreateMemoryBuffer`（经 CPU 回读），存在额外拷贝开销
- `anyhow` 依赖已声明但未使用，错误处理全部使用 `thiserror`
- `MFSinkWriter` 手动实现了 `unsafe impl Send`，需要审慎评估线程安全性
