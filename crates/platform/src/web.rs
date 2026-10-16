//! Plain HTTPS GET, used for the release check ("a new version is available", no auto-update).

use crate::Result;

pub trait WebClient: Send + Sync {
    /// Blocking GET of `url` (https only). Returns the HTTP status and the body.
    fn get(&self, url: &str) -> Result<(u16, String)>;
}

/// No network access (tests, platforms without an implementation).
#[derive(Debug, Default, Clone, Copy)]
pub struct NullWebClient;

impl WebClient for NullWebClient {
    fn get(&self, _url: &str) -> Result<(u16, String)> {
        Err(crate::PlatformError::Unsupported("web requests"))
    }
}
