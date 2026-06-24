//! 时间水印渲染器
//! 内置等宽点阵字体，在 D3D11 staging 纹理上绘制时间

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::error::RecorderError;

/// 字符宽度（像素）
const CHAR_WIDTH: u32 = 32;
/// 字符高度（像素）
const CHAR_HEIGHT: u32 = 64;
/// 时间字符串字符数 (HH:MM:SS.mmm)
const TIME_CHARS: u32 = 12;
/// 水印边距（像素）
const MARGIN: u32 = 20;
/// 背景框内边距（像素）
const BG_PADDING: u32 = 4;

/// 水印渲染器
pub struct WatermarkRenderer {
    // 预渲染的字符点阵: 12个字符 (0-9, :, .), 每个字符 64 行，每行 4 字节 (32 像素宽)
    char_bitmaps: [[u8; 256]; 12],
}

/// 字符索引映射
const CHAR_INDEX: [(char, usize); 12] = [
    ('0', 0),
    ('1', 1),
    ('2', 2),
    ('3', 3),
    ('4', 4),
    ('5', 5),
    ('6', 6),
    ('7', 7),
    ('8', 8),
    ('9', 9),
    (':', 10),
    ('.', 11),
];

/// 将 8x16 点阵放大为 32x64 点阵
/// 原始每行 1 字节 (8 像素)，放大后每行 4 字节 (32 像素)
/// 放大策略：每个原始像素在水平和垂直方向各重复 4 次 / 4 次
const fn scale_bitmap(small: [u8; 16]) -> [u8; 256] {
    let mut result = [0u8; 256];
    let mut src_row = 0usize;
    while src_row < 16 {
        let src_byte = small[src_row];
        // 水平放大 4 倍：每个 bit 展开为 4 bit
        // 将 8 bit 输入扩展为 32 bit (4 字节)，每个原始 bit 变成 4 bit
        let b0 = ((src_byte & 0x80) >> 7) * 0xF0
            | ((src_byte & 0x40) >> 6) * 0x0F;
        let b1 = ((src_byte & 0x20) >> 5) * 0xF0
            | ((src_byte & 0x10) >> 4) * 0x0F;
        let b2 = ((src_byte & 0x08) >> 3) * 0xF0
            | ((src_byte & 0x04) >> 2) * 0x0F;
        let b3 = ((src_byte & 0x02) >> 1) * 0xF0
            | ((src_byte & 0x01) >> 0) * 0x0F;

        // 垂直放大 4 倍：每行重复 4 次写入
        let dst_row0 = src_row * 4;
        let mut dst_offset = 0usize;
        while dst_offset < 4 {
            let r = dst_row0 + dst_offset;
            result[r * 4] = b0;
            result[r * 4 + 1] = b1;
            result[r * 4 + 2] = b2;
            result[r * 4 + 3] = b3;
            dst_offset += 1;
        }

        src_row += 1;
    }
    result
}

impl WatermarkRenderer {
    /// 创建水印渲染器
    pub fn new() -> Self {
        // 内置 8x16 基础点阵字体 (0-9, :, .)
        // 格式: 每行 1 字节 (8 像素)，共 16 行
        let base_bitmaps: [[u8; 16]; 12] = [
            // 0: 8x16 点阵
            [
                0x00, 0x00, 0x3C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
                0x3C, 0x00, 0x00,
            ],
            // 1:
            [
                0x00, 0x00, 0x18, 0x38, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18,
                0x3C, 0x00, 0x00,
            ],
            // 2:
            [
                0x00, 0x00, 0x3C, 0x66, 0x66, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x40, 0x66, 0x66,
                0x7C, 0x00, 0x00,
            ],
            // 3:
            [
                0x00, 0x00, 0x3C, 0x66, 0x66, 0x06, 0x1C, 0x06, 0x66, 0x66, 0x66, 0x66, 0x66,
                0x3C, 0x00, 0x00,
            ],
            // 4:
            [
                0x00, 0x00, 0x0C, 0x1C, 0x3C, 0x6C, 0xCC, 0xFE, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C,
                0x1E, 0x00, 0x00,
            ],
            // 5:
            [
                0x00, 0x00, 0x7C, 0x60, 0x60, 0x60, 0x7C, 0x06, 0x06, 0x66, 0x66, 0x66, 0x66,
                0x3C, 0x00, 0x00,
            ],
            // 6:
            [
                0x00, 0x00, 0x1C, 0x30, 0x60, 0x60, 0x7C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
                0x3C, 0x00, 0x00,
            ],
            // 7:
            [
                0x00, 0x00, 0x7E, 0x66, 0x66, 0x06, 0x0C, 0x18, 0x30, 0x30, 0x30, 0x30, 0x30,
                0x30, 0x00, 0x00,
            ],
            // 8:
            [
                0x00, 0x00, 0x3C, 0x66, 0x66, 0x66, 0x3C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
                0x3C, 0x00, 0x00,
            ],
            // 9:
            [
                0x00, 0x00, 0x3C, 0x66, 0x66, 0x66, 0x66, 0x3E, 0x06, 0x06, 0x06, 0x0C, 0x18,
                0x30, 0x00, 0x00,
            ],
            // : (冒号)
            [
                0x00, 0x00, 0x00, 0x18, 0x18, 0x18, 0x00, 0x18, 0x18, 0x18, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00,
            ],
            // . (句点)
            [
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18,
                0x18, 0x00, 0x00,
            ],
        ];

        // 将 8x16 基础点阵放大为 32x64
        let char_bitmaps = [
            scale_bitmap(base_bitmaps[0]),
            scale_bitmap(base_bitmaps[1]),
            scale_bitmap(base_bitmaps[2]),
            scale_bitmap(base_bitmaps[3]),
            scale_bitmap(base_bitmaps[4]),
            scale_bitmap(base_bitmaps[5]),
            scale_bitmap(base_bitmaps[6]),
            scale_bitmap(base_bitmaps[7]),
            scale_bitmap(base_bitmaps[8]),
            scale_bitmap(base_bitmaps[9]),
            scale_bitmap(base_bitmaps[10]),
            scale_bitmap(base_bitmaps[11]),
        ];

        Self { char_bitmaps }
    }

    /// 获取字符索引
    fn get_char_index(&self, ch: char) -> Option<usize> {
        CHAR_INDEX
            .iter()
            .find(|(c, _)| *c == ch)
            .map(|(_, idx)| *idx)
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

    /// 绘制单个字符到 BGRA 缓冲区 (32x64 点阵)
    fn draw_char(
        &self,
        buffer: *mut std::ffi::c_void,
        row_pitch: usize,
        x: u32,
        y: u32,
        ch: char,
        width: u32,
        height: u32,
    ) {
        let buffer = buffer as *mut u8;
        if let Some(idx) = self.get_char_index(ch) {
            let bitmap = &self.char_bitmaps[idx];
            for row in 0..CHAR_HEIGHT {
                // 每行 4 字节 (32 像素)
                let src_b0 = bitmap[(row * 4) as usize];
                let src_b1 = bitmap[(row * 4 + 1) as usize];
                let src_b2 = bitmap[(row * 4 + 2) as usize];
                let src_b3 = bitmap[(row * 4 + 3) as usize];
                for col in 0..CHAR_WIDTH {
                    let src_byte = if col < 8 {
                        src_b0
                    } else if col < 16 {
                        src_b1
                    } else if col < 24 {
                        src_b2
                    } else {
                        src_b3
                    };
                    let bit_pos = col % 8;
                    if src_byte & (0x80 >> bit_pos) != 0 {
                        let dst_x = x + col;
                        let dst_y = y + row;
                        if dst_x < width && dst_y < height {
                            let dst_offset = dst_y as usize * row_pitch + dst_x as usize * 4;
                            unsafe {
                                *buffer.add(dst_offset) = 255; // B
                                *buffer.add(dst_offset + 1) = 255; // G
                                *buffer.add(dst_offset + 2) = 255; // R
                                *buffer.add(dst_offset + 3) = 255; // A
                            }
                        }
                    }
                }
            }
        }
    }

    /// 绘制半透明背景框
    /// 背景色: 黑色半透明 (B=0, G=0, R=0, A=128)
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
                    // Alpha 混合: 背景为黑色半透明 (0,0,0,128)
                    // 结果 = src * alpha + dst * (1 - alpha)
                    // 简化: 因为背景色为黑色 (0,0,0)，结果 = dst * (128/255) ≈ dst / 2
                    let b = *buffer.add(dst_offset);
                    let g = *buffer.add(dst_offset + 1);
                    let r = *buffer.add(dst_offset + 2);
                    let a = *buffer.add(dst_offset + 3);
                    *buffer.add(dst_offset) = b >> 1; // B: 原值 * 128/255 ≈ 原值/2
                    *buffer.add(dst_offset + 1) = g >> 1; // G
                    *buffer.add(dst_offset + 2) = r >> 1; // R
                    *buffer.add(dst_offset + 3) = a; // 保持原始 alpha
                }
            }
        }
    }

    /// 在 staging 纹理上绘制水印
    pub fn render(
        &self,
        context: &ID3D11DeviceContext,
        staging_texture: &ID3D11Texture2D,
        width: u32,
        height: u32,
    ) -> Result<(), RecorderError> {
        // 计算水印所需最小尺寸
        // 文字宽度: 12 字符 * 32px = 384px
        // 背景框: 384 + 2 * 4(padding) = 392px
        // 加上边距: 392 + 2 * 20(margin) = 432px (但左侧 margin 内就是文字)
        // 实际: margin(20) + padding(4) + 384 + padding(4) + margin(20) = 432
        // 最小宽度需要: margin + padding + 文字宽度 + padding = 20 + 4 + 384 + 4 = 412
        let text_width = TIME_CHARS * CHAR_WIDTH; // 384px
        let bg_width = text_width + 2 * BG_PADDING; // 392px
        let bg_height = CHAR_HEIGHT + 2 * BG_PADDING; // 72px
        let min_width = MARGIN + bg_width + BG_PADDING; // 左边距 + 背景 + 右余量 ≈ 416
        let min_height = MARGIN + bg_height + BG_PADDING; // 下边距 + 背景 + 上余量 ≈ 96

        if width < min_width || height < min_height {
            return Ok(()); // 跳过
        }

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

            // 获取时间字符串
            let time_str = Self::get_time_string();

            // 计算水印位置 (左下角)
            // 背景框位置
            let bg_x = MARGIN;
            let bg_y = height.saturating_sub(MARGIN).saturating_sub(bg_height);

            // 先绘制半透明背景框
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

            // 文字起始位置（背景框内部，加 padding）
            let start_x = bg_x + BG_PADDING;
            let start_y = bg_y + BG_PADDING;

            // 绘制每个字符
            for (i, ch) in time_str.chars().enumerate() {
                self.draw_char(
                    mapped.pData,
                    mapped.RowPitch as usize,
                    start_x + i as u32 * CHAR_WIDTH,
                    start_y,
                    ch,
                    width,
                    height,
                );
            }

            context.Unmap(staging_texture, 0);
        }

        Ok(())
    }
}
