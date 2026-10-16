use crate::{Result, UnboundedSender};
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

/// Clipboard formats are MIME types internally (plan section 17).
pub mod mime {
    pub const TEXT: &str = "text/plain";
    pub const HTML: &str = "text/html";
    pub const RTF: &str = "text/rtf";
    pub const PNG: &str = "image/png";
    pub const URI_LIST: &str = "text/uri-list";
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipFormatInfo {
    pub mime: String,
    /// 0 = unknown.
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOffer {
    /// Path relative to the transfer root, '/' separated.
    pub rel_path: String,
    pub size: u64,
    pub mtime_unix_ms: i64,
    pub is_dir: bool,
}

#[derive(Debug, Clone)]
pub enum ClipboardEvent {
    /// The clipboard content changed (not caused by us).
    Changed {
        formats: Vec<ClipFormatInfo>,
        /// Content is marked sensitive (password manager flags) and must not be synchronised.
        sensitive: bool,
        /// Eager text, if the clipboard holds plain text below the eager limit.
        text: Option<String>,
        /// Local files if the clipboard holds a file list.
        files: Vec<PathBuf>,
    },
}

/// Supplies data for delayed rendering. Called from the clipboard owner thread when a program
/// pastes; it may block (the backend applies the 5 s timeout policy).
pub trait ClipboardProvider: Send + Sync {
    fn fetch(&self, mime: &str) -> Result<Vec<u8>>;
}

/// Supplies file contents for virtual files (Explorer `FILECONTENTS`).
pub trait VirtualFileProvider: Send + Sync {
    fn open(&self, index: usize) -> Result<Box<dyn Read + Send>>;
}

pub trait ClipboardBackend: Send + Sync {
    fn start(&self, tx: UnboundedSender<ClipboardEvent>) -> Result<()>;
    fn stop(&self);
    fn read(&self, mime: &str) -> Result<Option<Vec<u8>>>;
    /// Sets plain text immediately and marks it as ours (no echo event).
    fn set_text(&self, text: &str) -> Result<()>;
    /// Registers formats for delayed rendering (marked as ours).
    fn offer(&self, formats: Vec<ClipFormatInfo>, provider: Arc<dyn ClipboardProvider>) -> Result<()>;
    /// Offers a list of files as virtual files (or a staged `CF_HDROP` fallback).
    fn offer_files(
        &self,
        files: Vec<FileOffer>,
        provider: Arc<dyn VirtualFileProvider>,
    ) -> Result<()>;
    /// Offers already staged local files as `CF_HDROP`.
    fn offer_local_files(&self, paths: Vec<PathBuf>) -> Result<()>;
}
