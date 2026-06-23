# 时间水印功能实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**目标:** 为 win-recorder 录制功能增加左下角时间水印（HH:MM:SS.mmm 格式）

**架构:** 在 D3D11 staging 纹理上叠加内置点阵字体渲染的时间水印，通过 PyO3 参数控制开关

**技术栈:** Rust + PyO3 + D3D11 + Windows API GetLocalTime

---

## 文件变更清单

| 文件 | 变更类型 | 说明 |
|------|----------|------|
| `src/watermark.rs` | 新增 | 水印渲染器，内置点阵字体 |
| `src/d3d11.rs` | 修改 | 拆分 upload_bgra，新增 copy_staging_to_gpu/context/staging_texture 访问器 |
| `src/recorder.rs` | 修改 | 新增 watermark 参数，write_frame 调用水印绘制 |
| `src/lib.rs` | 修改 | 注册 watermark 模块 |
| `tests/test_watermark.py` | 新增 | Python 测试脚本 |
| `D:\code\autotest\worker\screen\recorder.py` | 修改 | 传入 watermark=True |

---

## 实现任务

### Task 1: 创建 watermark.rs 水印渲染器

**Files:**
- Create: `src/watermark.rs`
- Modify: `Cargo.toml` (添加 Windows feature)

- [ ] **Step 1: 检查并更新 Cargo.toml 添加 Win32_System_Time feature**

```toml
[dependencies.windows]
version = "0.58"
features = [
    "Win32_Foundation",
    "Win32_Graphics_Direct3D11",
    "Win32_Graphics_Dxgi",
    "Win32_Media_MediaFoundation",
    "Win32_System_Time",  # 添加此行以使用 GetLocalTime
]
```

- [ ] **Step 2: 创建 src/watermark.rs 文件**

```rust
//! 时间水印渲染器
//! 内置等宽点阵字体，在 D3D11 staging 纹理上绘制时间

use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::System::Time::GetLocalTime;
use crate::error::RecorderError;

/// 水印渲染器
pub struct WatermarkRenderer {
    // 预渲染的字符点阵: 12个字符 (0-9, :, .), 每个16行每行1字节
    char_bitmaps: [[u8; 16]; 12],
}

/// 字符索引映射
const CHAR_INDEX: [(char, usize); 12] = [
    ('0', 0), ('1', 1), ('2', 2), ('3', 3), ('4', 4),
    ('5', 5), ('6', 6), ('7', 7), ('8', 8), ('9', 9),
    (':', 10), ('.', 11),
];

impl WatermarkRenderer {
    /// 创建水印渲染器
    pub fn new() -> Self {
        // 内置 8x16 点阵字体 (0-9, :, .)
        // 格式: 每行 1 字节 (8 像素)，共 16 行
        let char_bitmaps = [
            // 0: 8x16 点阵
            [0x00, 0x00, 0x3C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00, 0x00],
            // 1: 
            [0x00, 0x00, 0x18, 0x38, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0x00, 0x00],
            // 2:
            [0x00, 0x00, 0x3C, 0x66, 0x66, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x40, 0x66, 0x66, 0x7C, 0x00, 0x00],
            // 3:
            [0x00, 0x00, 0x3C, 0x66, 0x66, 0x06, 0x1C, 0x06, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00, 0x00],
            // 4:
            [0x00, 0x00, 0x0C, 0x1C, 0x3C, 0x6C, 0xCC, 0xFE, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00, 0x00],
            // 5:
            [0x00, 0x00, 0x7C, 0x60, 0x60, 0x60, 0x7C, 0x06, 0x06, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00, 0x00],
            // 6:
            [0x00, 0x00, 0x1C, 0x30, 0x60, 0x60, 0x7C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00, 0x00],
            // 7:
            [0x00, 0x00, 0x7E, 0x66, 0x66, 0x06, 0x0C, 0x18, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x00, 0x00],
            // 8:
            [0x00, 0x00, 0x3C, 0x66, 0x66, 0x66, 0x3C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00, 0x00],
            // 9:
            [0x00, 0x00, 0x3C, 0x66, 0x66, 0x66, 0x66, 0x3E, 0x06, 0x06, 0x06, 0x0C, 0x18, 0x30, 0x00, 0x00],
            // : (冒号)
            [0x00, 0x00, 0x00, 0x18, 0x18, 0x18, 0x00, 0x18, 0x18, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            // . (句点)
            [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x18, 0x00, 0x00],
        ];
        
        Self { char_bitmaps }
    }
    
    /// 获取字符索引
    fn get_char_index(&self, ch: char) -> Option<usize> {
        CHAR_INDEX.iter().find(|(c, _)| *c == ch).map(|(_, idx)| *idx)
    }
    
    /// 获取当前时间字符串 HH:MM:SS.mmm
    pub fn get_time_string(&self) -> String {
        unsafe {
            let mut st = SYSTEMTIME::default();
            GetLocalTime(&mut st);
            format!(
                "{:02}:{:02}:{:02}.{:03}",
                st.wHour,
                st.wMinute,
                st.wSecond,
                st.wMilliseconds
            )
        }
    }
    
    /// 绘制单个字符到 BGRA 缓冲区
    fn draw_char(&self, buffer: *mut u8, row_pitch: usize, x: u32, y: u32, ch: char) {
        if let Some(idx) = self.get_char_index(ch) {
            let bitmap = &self.char_bitmaps[idx];
            for row in 0..16u32 {
                let src_byte = bitmap[row as usize];
                // 逐像素绘制 (高位在左)
                for col in 0..8u32 {
                    if src_byte & (0x80 >> col) != 0 {
                        // 绘制白色像素 (BGRA: 255, 255, 255, 255)
                        let dst_offset = ((y + row) as usize * row_pitch + ((x + col) as usize * 4));
                        unsafe {
                            *buffer.add(dst_offset) = 255;      // B
                            *buffer.add(dst_offset + 1) = 255;  // G
                            *buffer.add(dst_offset + 2) = 255;  // R
                            *buffer.add(dst_offset + 3) = 255;  // A
                        }
                    }
                }
            }
        }
    }
    
    /// 在 staging 纹理上绘制水印
    pub fn render(&self, context: &ID3D11DeviceContext, staging_texture: &ID3D11Texture2D, width: u32, height: u32) -> Result<(), RecorderError> {
        // 检查最小分辨率
        if width < 100 || height < 30 {
            return Ok(());  // 跳过
        }
        
        // 映射 staging 纹理 (READ_WRITE)
        unsafe {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context.Map(staging_texture, 0, D3D11_MAP_READ_WRITE, 0, Some(&mut mapped))
                .map_err(|e| RecorderError::D3D11TextureError(format!("Map staging 失败: {}", e)))?;
            
            // 获取时间字符串
            let time_str = self.get_time_string();
            
            // 计算水印位置 (左下角，距边缘 20px)
            let margin = 20u32;
            let char_width = 8u32;
            let char_height = 16u32;
            let start_x = margin;
            let start_y = height - margin - char_height;
            
            // 绘制每个字符
            for (i, ch) in time_str.chars().enumerate() {
                self.draw_char(
                    mapped.pData,
                    mapped.RowPitch as usize,
                    start_x + i as u32 * char_width,
                    start_y,
                    ch,
                );
            }
            
            context.Unmap(staging_texture, 0);
        }
        
        Ok(())
    }
}
```

- [ ] **Step 3: 提交初始版本**

```bash
git add src/watermark.rs Cargo.toml
git commit -m "feat: 添加时间水印渲染器 (watermark.rs)"
```

---

### Task 2: 修改 d3d11.rs 拆分 upload_bgra

**Files:**
- Modify: `src/d3d11.rs:145-193` (upload_bgra 方法)
- Add: `src/d3d11.rs` (新增方法)

- [ ] **Step 1: 在 D3D11TextureManager 中添加拆分方法**

```rust
/// 仅上传数据到 staging 纹理（不拷贝到 GPU）
pub fn upload_bgra_to_staging(&self, frame_data: &[u8]) -> Result<(), RecorderError> {
    // ... 原有 upload_bgra 的 Map/拷贝/Unmap 逻辑
}

/// 将 staging 纹理拷贝到 GPU 纹理  
pub fn copy_staging_to_gpu(&self) {
    unsafe {
        self.context.CopyResource(&self.gpu_texture, &self.staging_texture);
    }
}

/// 获取 device context（用于水印绘制）
pub fn context(&self) -> &ID3D11DeviceContext {
    &self.context
}

/// 获取 staging 纹理（用于水印绘制）
pub fn staging_texture(&self) -> &ID3D11Texture2D {
    &self.staging_texture
}
```

- [ ] **Step 2: 修改 upload_bgra 调用拆分方法**

```rust
/// 上传 BGRA 帧数据到 GPU 纹理（原有兼容性方法）
pub fn upload_bgra(&self, frame_data: &[u8]) -> Result<(), RecorderError> {
    self.upload_bgra_to_staging(frame_data)?;
    self.copy_staging_to_gpu();
    Ok(())
}
```

- [ ] **Step 3: 提交**

```bash
git add src/d3d11.rs
git commit -m "refactor: 拆分 upload_bgra 为 upload_bgra_to_staging + copy_staging_to_gpu"
```

---

### Task 3: 修改 recorder.rs 新增 watermark 参数

**Files:**
- Modify: `src/recorder.rs`
- Add: `use crate::watermark::WatermarkRenderer;`

- [ ] **Step 1: 添加 watermark 字段到结构体**

```rust
#[pyclass]
pub struct WinRecorder {
    // ... 现有字段
    watermark: bool,
    watermark_renderer: Option<WatermarkRenderer>,
}
```

- [ ] **Step 2: 修改构造函数参数**

```rust
#[pyclass]
#[pyo3(signature = (output_path, fps=30, audio=false, monitor=1, watermark=false))]
pub fn new(output_path: String, fps: u32, audio: bool, monitor: u32, watermark: bool) -> Self {
    // ... 现有初始化
    let watermark_renderer = if watermark {
        Some(WatermarkRenderer::new())
    } else {
        None
    };
    // ...
}
```

- [ ] **Step 3: 修改 write_frame 逻辑**

```rust
pub fn write_frame(&mut self, frame_data: &Bound<'_, PyByteArray>) -> Result<(), RecorderError> {
    // ... 现有检查
    
    // 上传到 staging（不拷贝到 GPU）
    texture_manager.upload_bgra_to_staging(frame_bytes)?;
    
    // 如果开启水印，绘制水印到 staging 纹理
    // 水印绘制失败不中断录制，只记录警告
    if self.watermark {
        if let Some(renderer) = &self.watermark_renderer {
            if let Err(e) = renderer.render(
                texture_manager.context(),
                texture_manager.staging_texture(),
                self.width,
                self.height,
            ) {
                // 水印绘制失败，记录警告但不中断录制
                eprintln!("警告: 水印绘制失败: {}", e);
            }
        }
    }
    
    // 拷贝 staging 到 GPU
    texture_manager.copy_staging_to_gpu();
    
    // ... 后续编码逻辑
}
```

> **注意**: 水印绘制使用 `if let Err(e) = ...` 捕获错误并打印警告，不使用 `?` 传播，确保水印失败不会中断录制流程。这符合规格第 3.4 节的要求。

- [ ] **Step 4: 提交**

```bash
git add src/recorder.rs
git commit -m "feat: WinRecorder 新增 watermark 参数支持时间水印"
```

---

### Task 4: 修改 lib.rs 注册 watermark 模块

**Files:**
- Modify: `src/lib.rs`

- [ ] **Step 1: 添加 watermark 模块声明**

```rust
mod watermark;
```

- [ ] **Step 2: 提交**

```bash
git add src/lib.rs
git commit -m "feat: 注册 watermark 模块"
```

---

### Task 5: 构建和测试

**Files:**
- Test: `tests/test_watermark.py` (新建)

- [ ] **Step 1: 构建 Rust 库**

```bash
maturin develop
```

- [ ] **Step 2: 编写测试脚本**

```python
"""测试时间水印功能"""
import win_recorder
import mss
import os
import time

print("=== 测试时间水印功能 ===")

# 测试获取显示器尺寸
orig_width, orig_height = win_recorder.WinRecorder.get_monitor_size(1)
print(f"主显示器尺寸: {orig_width} x {orig_height}")

# 测试创建带水印的录屏器
output_path = "test_watermark.mp4"
recorder = win_recorder.WinRecorder(
    output_path=output_path,
    fps=10,
    audio=False,
    monitor=1,
    watermark=True  # 开启水印
)
print("录屏器创建成功 (watermark=True)")

# 开始录制
recorder.start()
print("录制已开始")

# 获取对齐后的分辨率
aligned_width = recorder.width
aligned_height = recorder.height
print(f"对齐后分辨率: {aligned_width} x {aligned_height}")

# 使用 mss 截取屏幕，录制 10 秒
with mss.mss() as sct:
    monitor_config = sct.monitors[1]
    
    start_time = time.time()
    frame_count = 0
    
    while time.time() - start_time < 10:  # 录制 10 秒
        screenshot = sct.grab(monitor_config)
        raw_frame = bytearray(screenshot.raw)
        
        # 扩展帧到对齐尺寸
        if aligned_width != orig_width or aligned_height != orig_height:
            aligned_frame = bytearray(aligned_width * aligned_height * 4)
            for row in range(orig_height):
                src_offset = row * orig_width * 4
                dst_offset = row * aligned_width * 4
                aligned_frame[dst_offset:dst_offset + orig_width * 4] = raw_frame[src_offset:src_offset + orig_width * 4]
            frame_data = aligned_frame
        else:
            frame_data = raw_frame
        
        recorder.write_frame(frame_data)
        frame_count += 1
        print(f"  已写入第 {frame_count} 帧")

# 结束录制
recorder.stop()
print(f"录制已结束，共 {frame_count} 帧")

# 检查文件
if os.path.exists(output_path):
    file_size = os.path.getsize(output_path)
    print(f"输出文件: {output_path}")
    print(f"文件大小: {file_size / 1024 / 1024:.2f} MB")
    print("\n请检查视频左下角是否显示时间水印 HH:MM:SS.mmm")
    # 不删除，方便用户检查
else:
    print("错误: 输出文件不存在")
```

- [ ] **Step 3: 运行测试**

```bash
python tests/test_watermark.py
```

- [ ] **Step 4: 提交测试脚本**

```bash
git add tests/test_watermark.py
git commit -m "test: 添加时间水印功能测试脚本"
```

---

### Task 6: 编译并验证

- [ ] **Step 1: 执行完整构建**

```bash
maturin develop
```

- [ ] **Step 2: 运行测试脚本录制 10 秒视频**

```bash
python tests/test_watermark.py
```

- [ ] **Step 3: 检查输出**

用户需要检查生成的 `test_watermark.mp4` 文件，确认左下角显示时间水印（格式：HH:MM:SS.mmm）

---

## 验收标准

1. ✅ `WinRecorder` 支持 `watermark=True` 参数
2. ✅ 录制 10 秒视频后，左下角显示时间水印（HH:MM:SS.mmm 格式）
3. ✅ 时间随帧递增
4. ✅ 默认 `watermark=False` 时无水印
5. ✅ H264Encoder 推流不受影响（无水印）

---

### Task 7: autotest 集成

**Files:**
- Modify: `D:\code\autotest\worker\screen\recorder.py`

- [ ] **Step 1: 修改 autotest 的 recorder.py 传入 watermark=True**

在 `D:\code\autotest\worker\screen\recorder.py` 中找到 `WinRecorder` 构造调用，添加 `watermark=True` 参数。

```python
self._win_recorder = win_recorder.WinRecorder(
    output_path=self.output_path,
    fps=self.fps,
    audio=self.audio,
    monitor=self.monitor,
    watermark=True,  # 启用时间水印
)
```

- [ ] **Step 2: 提交 (在 autotest 仓库中)**