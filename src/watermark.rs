//! 时间水印渲染器
//! 内置等宽点阵字体，在 D3D11 staging 纹理上绘制时间

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::error::RecorderError;

/// 水印渲染器
pub struct WatermarkRenderer {
    // 预渲染的字符点阵: 12个字符 (0-9, :, .), 每个16行每行1字节
    char_bitmaps: [[u8; 16]; 12],
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

impl WatermarkRenderer {
    /// 创建水印渲染器
    pub fn new() -> Self {
        // 内置 8x16 点阵字体 (0-9, :, .)
        // 格式: 每行 1 字节 (8 像素)，共 16 行
        let char_bitmaps = [
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

    /// 绘制单个字符到 BGRA 缓冲区
    fn draw_char(
        &self,
        buffer: *mut std::ffi::c_void,
        row_pitch: usize,
        x: u32,
        y: u32,
        ch: char,
    ) {
        let buffer = buffer as *mut u8; // 类型转换
        if let Some(idx) = self.get_char_index(ch) {
            let bitmap = &self.char_bitmaps[idx];
            for row in 0..16u32 {
                let src_byte = bitmap[row as usize];
                // 逐像素绘制 (高位在左)
                for col in 0..8u32 {
                    if src_byte & (0x80 >> col) != 0 {
                        // 绘制白色像素 (BGRA: 255, 255, 255, 255)
                        let dst_offset = (y + row) as usize * row_pitch + (x + col) as usize * 4;
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

    /// 在 staging 纹理上绘制水印
    pub fn render(
        &self,
        context: &ID3D11DeviceContext,
        staging_texture: &ID3D11Texture2D,
        width: u32,
        height: u32,
    ) -> Result<(), RecorderError> {
        // 检查最小分辨率 (12字符 * 8px + 20px margin = 116px 宽)
        if width < 116 || height < 36 {
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

            // 计算水印位置 (左下角，距边缘 20px)
            let margin = 20u32;
            let char_width = 8u32;
            let char_height = 16u32;
            let start_x = margin;
            let start_y = height.saturating_sub(margin).saturating_sub(char_height);

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
