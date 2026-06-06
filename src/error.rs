use thiserror::Error;
use pyo3::PyErr;

/// win-recorder 错误类型
#[derive(Error, Debug)]
pub enum RecorderError {
    #[error("D3D11 device creation failed: {0}")]
    D3D11Error(String),

    #[error("D3D11 texture operation failed: {0}")]
    D3D11TextureError(String),

    #[error("Media Foundation error: {0}")]
    MFError(String),

    #[error("WASAPI audio error: {0}")]
    AudioError(String),

    #[error("Invalid parameter: {0}")]
    InvalidParam(String),

    #[error("Frame size mismatch: expected {expected} bytes, got {actual} bytes")]
    FrameSizeMismatch { expected: usize, actual: usize },

    #[error("Monitor not found: monitor={monitor}")]
    MonitorNotFound { monitor: u32 },

    #[error("Not recording")]
    NotRecording,

    #[error("Already recording")]
    AlreadyRecording,

    #[error("Recording failed: {0}")]
    RecordingFailed(String),
}

/// 转换为 Python 异常
impl From<RecorderError> for pyo3::PyErr {
    fn from(err: RecorderError) -> Self {
        use pyo3::exceptions::*;
        match err {
            RecorderError::InvalidParam(_) |
            RecorderError::FrameSizeMismatch { .. } => {
                PyValueError::new_err(err.to_string())
            }
            _ => PyRuntimeError::new_err(err.to_string()),
        }
    }
}

/// 将 Python 异常转换为 RecorderError
impl From<pyo3::PyErr> for RecorderError {
    fn from(err: pyo3::PyErr) -> Self {
        RecorderError::RecordingFailed(err.to_string())
    }
}