mod error;

use pyo3::prelude::*;

/// win-recorder: Windows 硬编录屏库
#[pymodule]
fn win_recorder(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    // TODO: 添加 WinRecorder 类
    Ok(())
}