mod d3d11;
mod error;
mod mf_writer;
mod recorder;

use pyo3::prelude::*;
use recorder::WinRecorder;

/// win-recorder: Windows 硬编录屏库
#[pymodule]
fn win_recorder(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    // 注册 WinRecorder 类
    m.add_class::<WinRecorder>()?;
    Ok(())
}