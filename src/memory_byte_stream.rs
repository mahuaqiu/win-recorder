//! NAL 单元解析工具
//!
//! 提供 H.264 编码数据的 NAL 单元提取和类型检测功能

/// 从内存缓冲区提取 NAL 单元
pub fn extract_nal_units(data: &[u8]) -> Vec<Vec<u8>> {
    if has_annex_b_start_code(data) {
        return extract_annex_b_nal_units(data);
    }

    extract_length_prefixed_nal_units(data)
}

fn has_annex_b_start_code(data: &[u8]) -> bool {
    data.windows(3).any(|w| w == [0x00, 0x00, 0x01])
        || data.windows(4).any(|w| w == [0x00, 0x00, 0x00, 0x01])
}

fn extract_annex_b_nal_units(data: &[u8]) -> Vec<Vec<u8>> {
    let mut nal_units = Vec::new();

    if data.len() < 5 {
        return nal_units;
    }

    // 查找 NAL 单元起始码: 0x00 0x00 0x00 0x01 或 0x00 0x00 0x01
    let mut start_idx: Option<usize> = None;

    for i in 0..data.len() - 3 {
        // 检测 4 字节起始码: 0x00 0x00 0x00 0x01
        if i + 4 <= data.len()
            && data[i] == 0x00
            && data[i + 1] == 0x00
            && data[i + 2] == 0x00
            && data[i + 3] == 0x01
        {
            if let Some(start) = start_idx {
                if start < i {
                    nal_units.push(data[start..i].to_vec());
                }
            }
            start_idx = Some(i + 4);
        }
        // 检测 3 字节起始码: 0x00 0x00 0x01
        else if data[i] == 0x00 && data[i + 1] == 0x00 && data[i + 2] == 0x01 {
            if let Some(start) = start_idx {
                if start < i {
                    nal_units.push(data[start..i].to_vec());
                }
            }
            start_idx = Some(i + 3);
        }
    }

    // 添加最后一个 NAL 单元
    if let Some(start) = start_idx {
        if start < data.len() {
            nal_units.push(data[start..].to_vec());
        }
    }

    nal_units
}

fn extract_length_prefixed_nal_units(data: &[u8]) -> Vec<Vec<u8>> {
    let mut nal_units = Vec::new();
    let mut offset = 0usize;

    while offset + 4 <= data.len() {
        let nal_len = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        offset += 4;

        if nal_len == 0 || offset + nal_len > data.len() {
            return Vec::new();
        }

        nal_units.push(data[offset..offset + nal_len].to_vec());
        offset += nal_len;
    }

    if offset == data.len() {
        nal_units
    } else {
        Vec::new()
    }
}

/// 检测 NAL 单元类型
pub fn get_nal_type(data: &[u8]) -> Option<u8> {
    if data.is_empty() {
        return None;
    }

    // 跳过起始码后获取 NAL 头
    let mut i = 0;
    while i < data.len() && data[i] == 0x00 {
        i += 1;
    }

    if i < data.len() && data[i] == 0x01 {
        i += 1;
    }

    if i < data.len() {
        // NAL 单元类型在低 5 位
        Some(data[i] & 0x1F)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_nal_units() {
        // 模拟 H.264 比特流，包含 SPS、PPS 和一个 IDR 帧
        let data = vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0x00, // SPS
            0x00, 0x00, 0x00, 0x01, 0x68, 0x00, 0xF8, // PPS
            0x00, 0x00, 0x00, 0x01, 0x65, 0x41, 0xFF, 0xFF, // IDR
        ];

        let nal_units = extract_nal_units(&data);
        assert!(nal_units.len() >= 2);
    }

    #[test]
    fn test_extract_length_prefixed_nal_units() {
        let data = vec![
            0x00, 0x00, 0x00, 0x04, 0x67, 0x42, 0x00, 0x1E, 0x00, 0x00, 0x00, 0x03, 0x68, 0x00,
            0xF8,
        ];

        let nal_units = extract_nal_units(&data);

        assert_eq!(nal_units.len(), 2);
        assert_eq!(nal_units[0], vec![0x67, 0x42, 0x00, 0x1E]);
        assert_eq!(nal_units[1], vec![0x68, 0x00, 0xF8]);
    }

    #[test]
    fn test_reject_invalid_length_prefixed_nal_units() {
        let data = vec![0x00, 0x00, 0x00, 0x10, 0x67];

        assert!(extract_nal_units(&data).is_empty());
    }
}
