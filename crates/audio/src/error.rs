//! Error type of the audio crate.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("opus: {0}")]
    Opus(String),
    #[error("invalid argument: {0}")]
    Invalid(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;
