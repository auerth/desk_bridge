//! Desk Bridge file transfer engine (plan section 8.6).
//!
//! Transport-agnostic: everything runs over `AsyncRead`/`AsyncWrite`, so the same code works on
//! QUIC uni-streams and on `tokio::io::duplex` in tests.
//!
//! * [`scan`]: plans files and folders (recursive, stable order, empty folders survive)
//! * [`sanitize`]: hardens received relative paths against traversal and Windows name tricks
//! * [`wire`]: one file per stream, 256 KiB chunks, BLAKE3 verification, resume
//! * [`manager`]: transfer bookkeeping and history for the UI

pub mod manager;
pub mod sanitize;
pub mod scan;
pub mod wire;

use std::path::PathBuf;

/// Chunk size of the raw data frames.
pub const CHUNK: usize = 256 * 1024;

/// Errors of the transfer engine.
#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("E/A-Fehler: {0}")]
    Io(#[from] std::io::Error),
    #[error("Protokollfehler: {0}")]
    Frame(#[from] deskbridge_proto::frame::FrameError),
    #[error("Ungültiger Pfad: {0}")]
    Path(#[from] sanitize::PathError),
    #[error("Protokollverletzung: {0}")]
    Protocol(&'static str),
    #[error("Prüfsumme stimmt nicht überein")]
    HashMismatch,
    #[error("Dateigröße stimmt nicht überein")]
    SizeMismatch,
    #[error("Fortsetzen nicht möglich: {0}")]
    ResumeInvalid(&'static str),
    #[error("Quelldatei hat sich während der Übertragung geändert")]
    SourceChanged,
}

impl FileError {
    /// True if the partial file can never become valid and must be discarded.
    #[must_use]
    pub fn is_corrupt(&self) -> bool {
        matches!(
            self,
            Self::HashMismatch | Self::SizeMismatch | Self::ResumeInvalid(_)
        )
    }
}

pub type Result<T> = std::result::Result<T, FileError>;

/// Default folder for received files: `%USERPROFILE%\Downloads\Desk Bridge`.
#[must_use]
pub fn default_target_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    match home {
        Some(h) if !h.is_empty() => PathBuf::from(h).join("Downloads").join("Desk Bridge"),
        _ => std::env::temp_dir().join("Desk Bridge"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dir_ends_with_desk_bridge() {
        assert!(default_target_dir().ends_with("Desk Bridge"));
    }
}
