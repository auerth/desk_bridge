//! Clipboard synchronisation policy and state machine (plan section 8.5).
//!
//! No OS calls: the OS side is a [`deskbridge_platform::clipboard::ClipboardBackend`], the network
//! side is a [`Fetcher`] (and a [`FileSource`] for file lists) supplied by the glue code.
//!
//! * text below `eager_limit_bytes` travels inside the offer and is written immediately
//! * everything else is registered for delayed rendering and fetched on paste (with a timeout)
//! * everything we write ourselves is remembered so it is never echoed back (loop avoidance)
//! * sensitive content never leaves the machine

mod sync;

pub use sync::ClipboardSync;

use deskbridge_platform::clipboard::VirtualFileProvider;
use deskbridge_proto::pb;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// User-facing synchronisation settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConfig {
    /// Send local clipboard changes to the peer.
    pub send: bool,
    /// Apply clipboard offers from the peer.
    pub receive: bool,
    /// Largest single clipboard format that is synchronised (files are streamed and not limited).
    pub max_size_bytes: u64,
    /// Plain text below this size travels inside the offer (default 1 MiB).
    pub eager_limit_bytes: u64,
    pub sync_images: bool,
    /// HTML, RTF and URL lists.
    pub sync_rich_text: bool,
    pub sync_files: bool,
    /// Do not put the peer's files on the clipboard when they are copied, but only once the
    /// user arrives on this device (`ClipboardSync::claim_pending_files`).
    pub files_on_arrival: bool,
    /// Never sync content flagged by password managers.
    pub respect_sensitive: bool,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            send: true,
            receive: true,
            max_size_bytes: 64 * 1024 * 1024,
            eager_limit_bytes: 1024 * 1024,
            sync_images: true,
            sync_rich_text: true,
            sync_files: true,
            files_on_arrival: false,
            respect_sensitive: true,
        }
    }
}

/// Why a delayed fetch from the peer failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    #[error("Zeitüberschreitung beim Abruf aus der Zwischenablage des Partners")]
    Timeout,
    /// The peer's clipboard changed or the offer is unknown.
    #[error("Inhalt nicht mehr verfügbar")]
    NotFound,
    #[error("Verbindung zum Partner getrennt")]
    Disconnected,
    #[error("{0}")]
    Other(String),
}

/// Fetches clipboard data from the peer. Implemented by the glue on top of the network layer.
pub trait Fetcher: Send + Sync {
    /// Blocking fetch of one format from the peer (called from the OS clipboard owner thread on
    /// paste, must honour `timeout`).
    fn fetch(&self, offer_id: u64, mime: &str, timeout: Duration) -> Result<Vec<u8>, FetchError>;
}

/// Plugs the file transfer engine into clipboard file lists.
pub trait FileSource: Send + Sync {
    /// Virtual-file provider for a file list offered by the peer, or `None` if virtual files are
    /// not available (then [`FileSource::staged`] is used).
    fn provider(
        &self,
        offer_id: u64,
        files: &[pb::FileEntry],
    ) -> Option<Arc<dyn VirtualFileProvider>>;

    /// Staged fallback: copies the files to a local folder (blocking) and returns the top-level
    /// paths to offer as `CF_HDROP`.
    fn staged(&self, _offer_id: u64, _files: &[pb::FileEntry]) -> Option<Vec<PathBuf>> {
        None
    }
}

/// Timeout for one delayed fetch: 5 s plus the time a 10 MB/s link would need for `size` bytes.
#[must_use]
pub fn fetch_timeout(size: u64) -> Duration {
    Duration::from_secs(5) + Duration::from_secs_f64(size as f64 / 10_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_scales_with_size() {
        assert_eq!(fetch_timeout(0), Duration::from_secs(5));
        assert_eq!(fetch_timeout(10_000_000), Duration::from_secs(6));
        assert!(fetch_timeout(100_000_000) >= Duration::from_secs(15));
    }
}
