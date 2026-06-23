# 时间水印功能设计

**版本**: 1.0  
**日期**: 2026-06-24  
**目标**: 为 win-recorder 录制功能增加左下角时间水印（HH:MM:SS.mmm 格式）

---

## 1. 需求概述

- **功能**：在录制的视频左下角叠加当前时间水印
- **格式**：`HH:MM:SS.mmm`（时:分:秒.毫秒）
- **位置**：左下角，距底边和左边各 20px 边距
- **实现层级**：Rust 侧叠加（win-recorder 库内部）
- **时间来源**：Rust 内部取系统本地时间，不需要 Python 传入
- **适用范围**：仅 `WinRecorder`（文件录制）路径，`H264Encoder`（推流）不加水印
- **默认行为**：`watermark=False`，不改变现有行为

---

## 2. 架构设计

### 2.1 数据流

```
Python BGRA → upload_bgra() [写入 staging 纹理]
                ↓
           draw_watermark() [叠加时间水印]
                ↓
           CopyResource [staging → GPU]
                ↓
           create_mf_sample() → MF SinkWriter 编码 → MP4
```

### 2.2 模块划分

| 模块 | 职责 |
|------|------|
| `src/watermark.rs` | 新增：水印渲染逻辑，内置点阵字体 |
| `src/d3d11.rs` | 调整：拆分 upload_bgra() 和 CopyResource，允许外部控制时机 |
| `src/recorder.rs` | 调整：新增 watermark 参数，write_frame() 中调用水印绘制 |
| `src/lib.rs` | 调整：注册 watermark 参数 |

---

## 3. 详细设计

### 3.1 watermark.rs — 水印渲染器

```rust
/// 水印渲染器
/// 内置等宽点阵字体，直接操作 D3D11 staging 纹理
pub struct WatermarkRenderer { ... }

impl WatermarkRenderer {
    /// 创建水印渲染器
    pub fn new() -> Self;

    /// 在 staging 纹理上绘制时间水印
    ///
    /// # 参数
    /// - staging_texture: D3D11 staging 纹理（需要用 MAP_READ_WRITE 映射）
    /// - width: 帧宽度（像素）
    /// - height: 帧高度（像素）
    ///
    /// # 说明
    /// 在左下角 (20, height - 20 - font_height) 位置绘制时间
    /// 需要先 Map 纹理为 READ_WRITE，获取 BGRA 像素指针后绘制
    pub fn render_on_texture(&self, context: &ID3D11DeviceContext, staging_texture: &ID3D11Texture2D, width: u32, height: u32);
}
```

#### 3.1.1 内置点阵字体

- **字符集**：0-9、:、. 共 12 个字符
- **字号**：每个字符 8×16 像素（宽×高）
- **颜色**：白色 (B=255, G=255, R=255, A=255)
- **格式**：每个字符 16 行，每行 1 字节（8 像素）的位图

示例：数字 "0" 的点阵（8×16，共 16 字节）：

```rust
// 16 行，每行 1 字节（8 像素），高位在左
const CHAR_0: [u8; 16] = [
    0x00, // 行 0: ........ (全空)
    0x00, // 行 1: ........
    0x3C, // 行 2: ..XXXX.. (0x3C = 00111100)
    0x66, // 行 3: .XX..XX. (0x66 = 01100110)
    0x66, // 行 4: .XX..XX.
    0x66, // 行 5: .XX..XX.
    0x66, // 行 6: .XX..XX.
    0x66, // 行 7: .XX..XX.
    0x66, // 行 8: .XX..XX.
    0x66, // 行 9: .XX..XX.
    0x66, // 行 10: .XX..XX.
    0x66, // 行 11: .XX..XX.
    0x66, // 行 12: .XX..XX.
    0x3C, // 行 13: ..XXXX..
    0x00, // 行 14: ........
    0x00, // 行 15: ........
];
// ... 类似定义 1-9 : .
```

#### 3.1.2 时间获取与格式化

使用 Windows API `GetLocalTime` 获取系统本地时间：

```rust
use windows::Win32::Foundation::SYSTEMTIME;

fn get_current_time_string() -> String {
    unsafe {
        let mut st = SYSTEMTIME::default();
        windows::Win32::System::Time::GetLocalTime(&mut st);

        format!(
            "{:02}:{:02}:{:02}.{:03}",
            st.wHour,
            st.wMinute,
            st.wSecond,
            st.wMilliseconds
        )
    }
}
```

> 使用 Windows API `GetLocalTime` 而非 `chrono`，避免引入额外依赖。

### 3.2 d3d11.rs — 纹理管理调整

当前 `upload_bgra()` 内部流程：

```rust
pub fn upload_bgra(&self, frame_data: &[u8]) -> Result<(), RecorderError> {
    // 1. Map staging 纹理 (WRITE)
    // 2. 拷贝数据到 staging
    // 3. Unmap staging
    // 4. CopyResource staging → gpu  // ← 这步需要拆出来
}
```

**调整方案**：

```rust
/// 仅上传数据到 staging 纹理（不拷贝到 GPU）
pub fn upload_bgra_to_staging(&self, frame_data: &[u8]) -> Result<(), RecorderError> { ... }

/// 将 staging 纹理拷贝到 GPU 纹理
pub fn copy_staging_to_gpu(&self) { ... }
```

- `upload_bgra_to_staging()`：保留原有逻辑，仅做步骤 1-3
- `copy_staging_to_gpu()`：新增，仅执行步骤 4
- 为兼容现有代码，保留 `upload_bgra()` 方法（调用 `upload_bgra_to_staging()` + `copy_staging_to_gpu()`）

### 3.3 recorder.rs — WinRecorder 调整

#### 3.3.1 结构体新增字段

```rust
#[pyclass]
pub struct WinRecorder {
    // ... 现有字段
    watermark: bool,           // 是否开启水印
    watermark_renderer: Option<WatermarkRenderer>,  // 水印渲染器
}
```

#### 3.3.2 构造函数新增参数

```rust
#[pyclass]
#[pyo3(signature = (output_path, fps=30, audio=false, monitor=1, watermark=false))]
pub struct WinRecorder { ... }
```

#### 3.3.3 write_frame() 调整

```rust
pub fn write_frame(&mut self, frame_data: &Bound<'_, PyByteArray>) -> Result<(), RecorderError> {
    // ... 现有检查

    let frame_bytes = unsafe { frame_data.as_bytes() };

    // 上传到 staging（不拷贝到 GPU）
    texture_manager.upload_bgra_to_staging(frame_bytes)?;

    // 如果开启水印，绘制水印到 staging 纹理
    if self.watermark {
        if let Some(renderer) = &self.watermark_renderer {
            // 重新映射 staging 纹理为 READ_WRITE，并绘制水印
            renderer.render_on_texture(
                texture_manager.context(),
                texture_manager.staging_texture(),
                self.width,
                self.height,
            )?;
        }
    }

    // 拷贝 staging 到 GPU
    texture_manager.copy_staging_to_gpu();

    // 创建 MF Sample 并写入
    let sample = texture_manager.create_mf_sample()?;
    let mut writer = sink_writer.lock();
    writer.write_sample(&sample)?;

    Ok(())
}
```

> **注意**：由于水印绘制需要读写 staging 纹理，`upload_bgra_to_staging()` 内部必须使用 `D3D11_MAP_READ_WRITE` 而非 `D3D11_MAP_WRITE`。

### 3.4 错误处理策略

- **水印绘制失败**：记录警告日志（`tracing::warn!`），但不中断录制
- **最小分辨率检查**：如果帧宽 < 100 或帧高 < 30，跳过水印绘制
- **异常隔离**：水印绘制代码使用 `catch_unwind` 或 `Result` 包裹，确保异常不影响主流程

---

## 4. API 设计

### 4.1 Python 绑定

```python
import win_recorder

# 录制（默认无水印）
recorder = win_recorder.WinRecorder(
    output_path="test.mp4",
    fps=10,
    audio=False,
    monitor=1,
    watermark=False  # 默认 False
)

# 录制（开启水印）
recorder = win_recorder.WinRecorder(
    output_path="test.mp4",
    fps=10,
    audio=False,
    monitor=1,
    watermark=True   # 开启时间水印
)
```

### 4.2 autotest 集成

修改 `D:\code\autotest\worker\screen\recorder.py`：

```python
self._win_recorder = win_recorder.WinRecorder(
    output_path=self.output_path,
    fps=self.fps,
    audio=self.audio,
    monitor=self.monitor,
    watermark=True,  # 启用时间水印
)
```

---

## 5. 时间同步说明

用户确认：水印时间直接使用帧被捕获时的系统本地时间，autotest 侧和录制侧自然一致。

- Rust 内部取 `SystemTime`（本地时间）
- 每帧调用 `write_frame()` 时实时获取当前时间
- 不需要与外部时间源同步

---

## 6. 测试计划

### 6.1 手动测试

| 测试项 | 说明 |
|--------|------|
| 水印显示 | 录制 10 秒视频，检查左下角是否显示 `HH:MM:SS.mmm` |
| 时间递增 | 录制 1 分钟视频，验证时间随帧递增 |
| 720p 录制 | 1280×720 分辨率下录制正常，水印清晰可读 |
| 默认行为 | 不传 `watermark` 参数或 `watermark=False` 时无水印 |
| 推流不受影响 | 使用 H264Encoder 推流时不应有水印 |

---

## 7. 文件变更清单

| 文件 | 变更类型 | 说明 |
|------|----------|------|
| `src/watermark.rs` | 新增 | 水印渲染器，内置点阵字体 |
| `src/d3d11.rs` | 修改 | 拆分 upload_bgra，新增 copy_staging_to_gpu |
| `src/recorder.rs` | 修改 | 新增 watermark 参数，write_frame 调用水印绘制 |
| `src/lib.rs` | 修改 | watermark 参数注册 |
| `Cargo.toml` | 无需修改 | 使用 Windows API 获取本地时间，无需额外依赖 |
| `pyproject.toml` | 无需修改 | Python 参数自动映射 |
| `D:\code\autotest\worker\screen\recorder.py` | 修改 | 传入 watermark=True |

---

## 8. 风险与限制

1. **字体样式固定**：使用内置 8×16 点阵字体，无法更换字体
2. **时区处理**：使用 Windows API `GetLocalTime` 获取准确的系统本地时间
3. **最小分辨率**：帧尺寸小于 100×30 时跳过水印绘制
4. **性能**：每帧额外一次纹理 Map/Unmap（READ_WRITE），预计增加 <1ms 延迟（720p 帧）
5. **D3D11 MAP 标志**：staging 纹理需要同时支持读写，使用 `D3D11_CPU_ACCESS_WRITE | D3D11_CPU_ACCESS_READ`