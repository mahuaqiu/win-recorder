//! 时间水印渲染器
//! 使用 Windows GDI 字体渲染，在 D3D11 staging 纹理上绘制时间
//! 优势：系统矢量字体渲染，OCR 识别效果远优于点阵字体

use windows::Win32::Foundation::{COLORREF, SIZE};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::error::RecorderError;

/// 基准分辨率宽度（2560×1440 作为基准）
const BASE_WIDTH: u32 = 2560;
/// 基准分辨率高度
const BASE_HEIGHT: u32 = 1440;
/// 基准背景框宽度（像素，在 2560×1440 下）
const BASE_BG_WIDTH: u32 = 360;
/// 基准背景框高度（像素，在 2560×1440 下）
const BASE_BG_HEIGHT: u32 = 80;
/// 基准边距（像素，在 2560×1440 下）
const BASE_MARGIN: u32 = 55;
/// 基准字体高度（像素，在 2560×1440 下）
const BASE_FONT_HEIGHT: u32 = 52;
/// 背景框内边距（像素）
const BASE_BG_PADDING: u32 = 6;

/// 背景框不透明度 (80%) - 用于参考计算
// const BG_ALPHA: u8 = 204;

/// 水印渲染器
pub struct WatermarkRenderer {
    // 缓存上次的缩放因子，避免重复计算
    cached_scale: f32,
    cached_bg_width: u32,
    cached_bg_height: u32,
    cached_margin: u32,
    cached_font_height: i32,
    cached_bg_padding: u32,
}

/// 根据分辨率计算缩放因子
/// 取宽度和高度缩放因子的较小值，确保水印不会超出屏幕
fn calc_scale(width: u32, height: u32) -> f32 {
    let scale_w = width as f32 / BASE_WIDTH as f32;
    let scale_h = height as f32 / BASE_HEIGHT as f32;
    scale_w.min(scale_h)
}

/// 根据缩放因子计算像素值，确保至少为 1
fn scale_px(base: u32, scale: f32) -> u32 {
    ((base as f32 * scale).round() as u32).max(1)
}

impl WatermarkRenderer {
    /// 创建水印渲染器
    pub fn new() -> Self {
        Self {
            cached_scale: 0.0,
            cached_bg_width: 0,
            cached_bg_height: 0,
            cached_margin: 0,
            cached_font_height: 0,
            cached_bg_padding: 0,
        }
    }

    /// 更新缓存参数（分辨率变化时调用）
    fn update_cache(&mut self, width: u32, height: u32) {
        let scale = calc_scale(width, height);
        if (scale - self.cached_scale).abs() > 0.001 {
            self.cached_scale = scale;
            self.cached_bg_width = scale_px(BASE_BG_WIDTH, scale);
            self.cached_bg_height = scale_px(BASE_BG_HEIGHT, scale);
            self.cached_margin = scale_px(BASE_MARGIN, scale);
            self.cached_font_height = (BASE_FONT_HEIGHT as f32 * scale).round() as i32;
            self.cached_bg_padding = scale_px(BASE_BG_PADDING, scale);
        }
    }

    /// 获取当前时间字符串 HH:MM:SS.mmm
    pub fn get_time_string() -> String {
        unsafe {
            let st = GetLocalTime();
            format!(
                "{:02}:{:02}:{:02}.{:03}",
                st.wHour, st.wMinute, st.wSecond, st.wMilliseconds
            )
        }
    }

    /// 使用 GDI 渲染文字到 BGRA 缓冲区
    fn render_text_gdi(
        time_str: &str,
        font_height: i32,
        bg_width: u32,
        bg_height: u32,
    ) -> Result<Option<(Vec<u8>, u32, u32)>, RecorderError> {
        unsafe {
            // 创建内存 DC
            let hdc = CreateCompatibleDC(None);
            if hdc.is_invalid() {
                return Err(RecorderError::D3D11TextureError(
                    "CreateCompatibleDC 失败".into(),
                ));
            }

            // 创建 DIB Section 用于 GDI 渲染（BGRA 格式）
            let dib_width = bg_width;
            let dib_height = bg_height;
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: dib_width as i32,
                    biHeight: -(dib_height as i32), // 负值 = 自顶向下
                    biPlanes: 1,
                    biBitCount: 32, // BGRA
                    biCompression: BI_RGB.0 as u32,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [RGBQUAD::default()],
            };

            let mut ppv_bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let hbitmap = CreateDIBSection(
                hdc,
                &bmi,
                DIB_RGB_COLORS,
                &mut ppv_bits,
                None,
                0,
            );

            let hbitmap = match hbitmap {
                Ok(h) => h,
                Err(e) => {
                    let _ = DeleteDC(hdc);
                    return Err(RecorderError::D3D11TextureError(
                        format!("CreateDIBSection 失败: {}", e),
                    ));
                }
            };

            if ppv_bits.is_null() {
                let _ = DeleteDC(hdc);
                return Err(RecorderError::D3D11TextureError(
                    "CreateDIBSection ppv_bits 为空".into(),
                ));
            }

            // 选入位图
            let old_bitmap = SelectObject(hdc, hbitmap);

            // 清空为全透明
            let total_bytes = (dib_width * dib_height * 4) as usize;
            std::ptr::write_bytes(ppv_bits as *mut u8, 0, total_bytes);

            // 创建字体 - 使用微软雅黑
            let font = CreateFontW(
                font_height,         // 高度
                0,                   // 宽度（自动）
                0,                   // 文字倾斜
                0,                   // 基线倾斜
                FW_BOLD.0 as i32,    // 粗体
                0,                   // 斜体
                0,                   // 下划线
                0,                   // 删除线
                DEFAULT_CHARSET.0 as u32,   // 字符集
                OUT_DEFAULT_PRECIS.0 as u32,
                CLIP_DEFAULT_PRECIS.0 as u32,
                CLEARTYPE_QUALITY.0 as u32, // ClearType 渲染
                (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32, // 微软雅黑不是等宽字体
                windows::core::PCWSTR(windows::core::w!("Microsoft YaHei").as_ptr()),
            );

            let old_font = SelectObject(hdc, font);

            // 设置文字颜色为白色 (0x00FFFFFF = BGR 格式)
            SetTextColor(hdc, COLORREF(0x00FFFFFF));
            // 设置背景透明（不填充背景）
            SetBkMode(hdc, TRANSPARENT);

            // 测量文字尺寸
            let time_wide: Vec<u16> = time_str.encode_utf16().collect();
            let mut size = SIZE::default();
            let _ = GetTextExtentPoint32W(
                hdc,
                &time_wide,
                &mut size,
            );

            // 计算文字居中偏移
            let text_x = ((dib_width as i32 - size.cx) / 2).max(0);
            let text_y = ((dib_height as i32 - size.cy) / 2).max(0);

            // 绘制文字
            let _ = TextOutW(
                hdc,
                text_x,
                text_y,
                &time_wide,
            );

            // 读取渲染结果
            let dib_data = std::slice::from_raw_parts(
                ppv_bits as *const u8,
                total_bytes,
            )
            .to_vec();

            // 恢复并清理 GDI 对象
            let _ = SelectObject(hdc, old_font);
            let _ = DeleteObject(HGDIOBJ(font.0));
            let _ = SelectObject(hdc, old_bitmap);
            let _ = DeleteObject(hbitmap);
            let _ = DeleteDC(hdc);

            Ok(Some((dib_data, dib_width, dib_height)))
        }
    }

    /// 绘制 70% 不透明黑色背景框（alpha 混合叠加）
    fn draw_background(
        buffer: *mut std::ffi::c_void,
        row_pitch: usize,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        frame_width: u32,
        frame_height: u32,
    ) {
        let buffer = buffer as *mut u8;
        // 70% 黑色叠加: 结果 = src * 0.3 + dst * 0.7
        // 简化: 原始 * 77 / 255 ≈ 原始 * 0.3
        for row in 0..h {
            let dst_y = y + row;
            if dst_y >= frame_height {
                break;
            }
            for col in 0..w {
                let dst_x = x + col;
                if dst_x >= frame_width {
                    break;
                }
                let dst_offset = dst_y as usize * row_pitch + dst_x as usize * 4;
                unsafe {
                    let b = *buffer.add(dst_offset);
                    let g = *buffer.add(dst_offset + 1);
                    let r = *buffer.add(dst_offset + 2);
                    // 70% 黑色叠加: 保留 30% 原始颜色
                    *buffer.add(dst_offset) = (b as u32 * 77 / 255) as u8;
                    *buffer.add(dst_offset + 1) = (g as u32 * 77 / 255) as u8;
                    *buffer.add(dst_offset + 2) = (r as u32 * 77 / 255) as u8;
                }
            }
        }
    }

    /// 将 GDI 渲染的文字合成到帧缓冲区
    /// 文字强制为纯白色，只要有像素就画
    fn composite_text(
        buffer: *mut std::ffi::c_void,
        row_pitch: usize,
        dst_x: u32,
        dst_y: u32,
        dib_data: &[u8],
        dib_width: u32,
        dib_height: u32,
        frame_width: u32,
        frame_height: u32,
    ) {
        let buffer = buffer as *mut u8;
        for row in 0..dib_height {
            let fy = dst_y + row;
            if fy >= frame_height {
                break;
            }
            for col in 0..dib_width {
                let fx = dst_x + col;
                if fx >= frame_width {
                    break;
                }
                let src_offset = (row * dib_width * 4 + col * 4) as usize;
                // 检查是否有像素（任何非零值表示有文字）
                let _src_a = dib_data[src_offset + 3]; // alpha
                let src_b = dib_data[src_offset];     // B
                let src_g = dib_data[src_offset + 1]; // G
                let src_r = dib_data[src_offset + 2]; // R

                // 如果 GDI 渲染了这个像素（有颜色），就画白色
                if src_b > 0 || src_g > 0 || src_r > 0 {
                    let dst_offset = fy as usize * row_pitch + fx as usize * 4;
                    unsafe {
                        *buffer.add(dst_offset) = 255; // B
                        *buffer.add(dst_offset + 1) = 255; // G
                        *buffer.add(dst_offset + 2) = 255; // R
                        *buffer.add(dst_offset + 3) = 255;
                    }
                }
            }
        }
    }

    /// 在 staging 纹理上绘制水印
    pub fn render(
        &mut self,
        context: &ID3D11DeviceContext,
        staging_texture: &ID3D11Texture2D,
        width: u32,
        height: u32,
    ) -> Result<(), RecorderError> {
        // 更新缩放缓存
        self.update_cache(width, height);

        let bg_width = self.cached_bg_width;
        let bg_height = self.cached_bg_height;
        let margin = self.cached_margin;
        let font_height = self.cached_font_height;
        let _bg_padding = self.cached_bg_padding;

        // 最小分辨率检查
        let min_width = margin + bg_width;
        let min_height = margin + bg_height;
        if width < min_width || height < min_height {
            return Ok(()); // 分辨率太小，跳过水印
        }

        // 使用 GDI 渲染文字（文字居中渲染在较小区域）
        let time_str = Self::get_time_string();
        let gdi_result = Self::render_text_gdi(&time_str, font_height, bg_width, bg_height)?;

        let (dib_data, dib_width, dib_height) = match gdi_result {
            Some(data) => data,
            None => return Ok(()), // 渲染失败，静默跳过
        };

        // 映射 staging 纹理 (READ_WRITE)
        unsafe {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context
                .Map(
                    staging_texture,
                    0,
                    D3D11_MAP_READ_WRITE,
                    0,
                    Some(&mut mapped),
                )
                .map_err(|e| {
                    RecorderError::D3D11TextureError(format!("Map staging 失败: {}", e))
                })?;

            // 计算水印位置（左下角）
            let bg_x = margin;
            let bg_y = height.saturating_sub(margin).saturating_sub(bg_height);

            // 先绘制 80% 不透明黑色背景框（带内边距）
            Self::draw_background(
                mapped.pData,
                mapped.RowPitch as usize,
                bg_x,
                bg_y,
                bg_width,
                bg_height,
                width,
                height,
            );

            // 文字在背景框内居中
            let text_x = bg_x + (bg_width - dib_width) / 2;
            let text_y = bg_y + (bg_height - dib_height) / 2;

            // 将 GDI 渲染的文字合成到帧上
            Self::composite_text(
                mapped.pData,
                mapped.RowPitch as usize,
                text_x,
                text_y,
                &dib_data,
                dib_width,
                dib_height,
                width,
                height,
            );

            context.Unmap(staging_texture, 0);
        }

        Ok(())
    }
}