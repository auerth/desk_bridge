#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("not supported on this platform: {0}")]
    Unsupported(&'static str),
    #[error("device not found: {0}")]
    DeviceNotFound(String),
    #[error("os error {code:#x}: {msg}")]
    Os { code: i64, msg: String },
    #[error("{0}")]
    Other(String),
}

impl PlatformError {
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}

pub type Result<T> = std::result::Result<T, PlatformError>;
