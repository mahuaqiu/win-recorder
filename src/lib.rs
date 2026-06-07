mod d3d11;
mod error;
mod memory_byte_stream;
mod mf_writer;
mod recorder;
mod streaming_encoder;

use pyo3::prelude::*;
use recorder::WinRecorder;
use streaming_encoder::StreamingEncoder;

/// win-recorder: Windows 硬编录屏库
#[pymodule]
fn win_recorder(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    // 注册 WinRecorder 类
    m.add_class::<WinRecorder>()?;
    // 注册 StreamingEncoder 类
    m.add_class::<StreamingEncoder>()?;
    Ok(())
}