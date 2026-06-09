//! BGRA 到 NV12 的 CPU 转换器
//!
//! 提供纯软件的格式转换，不依赖任何 Windows MFT 组件
//!
//! NV12 格式：
//! - Y 平面：width * height 字节，YUV420 Planar
//! - UV 平面：width * height / 2 字节，UV 交错 (U V U V ...)

/// 将 BGRA 数据转换为 NV12 格式
///
/// # 参数
/// - bgra_data: BGRA 格式的帧数据 (BGRABGRABGRA...)
/// - width: 帧宽度
/// - height: 帧高度
///
/// # 返回
/// NV12 格式的数据
pub fn bgra_to_nv12(bgra_data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let y_size = (width * height) as usize;
    let uv_size = (width * height / 2) as usize;
    let mut nv12 = vec![0u8; y_size + uv_size];

    let width = width as usize;
    let height = height as usize;

    // 1. 转换 Y 平面 (BGRA -> Y)
    // Y = 0.299*R + 0.587*G + 0.114*B
    // 简化版本: Y = (306*R + 601*G + 117*B) >> 10
    for y in 0..height {
        for x in 0..width {
            let bgra_idx = (y * width + x) * 4;
            let b = bgra_data[bgra_idx] as u32;
            let g = bgra_data[bgra_idx + 1] as u32;
            let r = bgra_data[bgra_idx + 2] as u32;
            
            // BT.601 亮度公式的简化整数版本
            let y_val = ((r * 306 + g * 601 + b * 117) >> 10) as u8;
            
            let y_idx = y * width + x;
            nv12[y_idx] = y_val;
        }
    }

    // 2. 转换 UV 平面 (BGRA -> UV 交错)
    // U = -0.147*R - 0.289*G + 0.436*B
    // V = 0.615*R - 0.515*G - 0.100*B
    // 简化版本，使用查表或近似
    for y in 0..height / 2 {
        for x in 0..width / 2 {
            // 取 2x2 块的平均
            let bgra_idx00 = ((y * 2) * width + (x * 2)) * 4;
            let bgra_idx01 = ((y * 2) * width + (x * 2 + 1)) * 4;
            let bgra_idx10 = ((y * 2 + 1) * width + (x * 2)) * 4;
            let bgra_idx11 = ((y * 2 + 1) * width + (x * 2 + 1)) * 4;

            // 平均 R, G, B
            let r = ((bgra_data[bgra_idx00] as u32 + bgra_data[bgra_idx01] as u32 
                   + bgra_data[bgra_idx10] as u32 + bgra_data[bgra_idx11] as u32) / 4) as i32;
            let g = ((bgra_data[bgra_idx00 + 1] as u32 + bgra_data[bgra_idx01 + 1] as u32 
                   + bgra_data[bgra_idx10 + 1] as u32 + bgra_data[bgra_idx11 + 1] as u32) / 4) as i32;
            let b = ((bgra_data[bgra_idx00 + 2] as u32 + bgra_data[bgra_idx01 + 2] as u32 
                   + bgra_data[bgra_idx10 + 2] as u32 + bgra_data[bgra_idx11 + 2] as u32) / 4) as i32;

            // BT.601 色度公式
            let u_val = (((-r * -147 - g * 289 + b * 436) >> 10) + 128) as u8;
            let v_val = (((r * 615 - g * 515 - b * 100) >> 10) + 128) as u8;

            let uv_idx = y_size + (y * width / 2 + x) * 2;
            nv12[uv_idx] = u_val;         // U
            nv12[uv_idx + 1] = v_val;     // V
        }
    }

    nv12
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bgra_to_nv12_size() {
        let bgra = vec![0u8; 1920 * 1080 * 4];
        let nv12 = bgra_to_nv12(&bgra, 1920, 1080);
        
        // NV12: Y 平面 + UV 平面
        let expected_size = 1920 * 1080 + 1920 * 1080 / 2;
        assert_eq!(nv12.len(), expected_size);
    }

    #[test]
    fn test_bgra_to_nv12_small() {
        let bgra = vec![0u8; 16 * 16 * 4];
        let nv12 = bgra_to_nv12(&bgra, 16, 16);
        
        let expected_size = 16 * 16 + 16 * 16 / 2;
        assert_eq!(nv12.len(), expected_size);
    }
}
