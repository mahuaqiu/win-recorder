mod bgra_to_nv12;
mod d3d11;
mod error;
mod h264_encoder;
mod memory_byte_stream;
mod mf_writer;
mod recorder;

use pyo3::prelude::*;
use h264_encoder::H264Encoder;
use recorder::WinRecorder;

/// win-recorder: Windows 硬编录屏库
#[pymodule]
fn win_recorder(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    // 注册 WinRecorder 类
    m.add_class::<WinRecorder>()?;
    // 注册 H264Encoder 类（用于实时推流）
    m.add_class::<H264Encoder>()?;
    Ok(())
}