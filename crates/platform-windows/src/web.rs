//! [`WebClient`] over WinHTTP: system TLS, and the system / PAC proxy configuration, so the
//! release check also works behind company proxies. No extra crate.

use deskbridge_platform::web::WebClient;
use deskbridge_platform::{PlatformError, Result};
use std::ffi::c_void;
use windows::Win32::Networking::WinHttp::{
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect,
    WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR};

/// Largest accepted body (a GitHub release JSON is a few KB).
const MAX_BODY: usize = 4 * 1024 * 1024;
const TIMEOUT_MS: i32 = 15_000;

/// Closes a WinHTTP handle on drop.
struct Handle(*mut c_void);

impl Handle {
    fn new(h: *mut c_void, what: &str) -> Result<Self> {
        if h.is_null() {
            Err(PlatformError::Other(format!(
                "{what}: {}",
                windows::core::Error::from_win32()
            )))
        } else {
            Ok(Self(h))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by WinHTTP and is closed exactly once.
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

fn os(what: &str) -> impl Fn(windows::core::Error) -> PlatformError + '_ {
    move |e| PlatformError::Other(format!("{what}: {e}"))
}

/// `https://host/path` → `(host, /path)`.
fn split_https(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = rest.find('/').map_or((rest, "/"), |i| rest.split_at(i));
    (!host.is_empty()).then_some((host, path))
}

#[derive(Debug, Default, Clone)]
pub struct WinHttpClient {
    user_agent: String,
}

impl WinHttpClient {
    #[must_use]
    pub fn new(user_agent: impl Into<String>) -> Self {
        Self {
            user_agent: user_agent.into(),
        }
    }
}

impl WebClient for WinHttpClient {
    fn get(&self, url: &str) -> Result<(u16, String)> {
        let (host, path) = split_https(url)
            .ok_or_else(|| PlatformError::Other(format!("only https URLs: {url}")))?;
        let agent = HSTRING::from(self.user_agent.as_str());
        let host = HSTRING::from(host);
        let path = HSTRING::from(path);
        let headers: Vec<u16> = "Accept: application/vnd.github+json\r\n"
            .encode_utf16()
            .collect();
        // SAFETY: all strings outlive the calls; handles are closed by `Handle` in reverse order.
        unsafe {
            let session = Handle::new(
                WinHttpOpen(
                    &agent,
                    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    0,
                ),
                "WinHttpOpen",
            )?;
            WinHttpSetTimeouts(session.0, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS)
                .map_err(os("WinHttpSetTimeouts"))?;
            let conn = Handle::new(
                WinHttpConnect(session.0, &host, INTERNET_DEFAULT_HTTPS_PORT, 0),
                "WinHttpConnect",
            )?;
            let req = Handle::new(
                WinHttpOpenRequest(
                    conn.0,
                    windows::core::w!("GET"),
                    &path,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    std::ptr::null(),
                    WINHTTP_FLAG_SECURE,
                ),
                "WinHttpOpenRequest",
            )?;
            WinHttpSendRequest(req.0, Some(&headers), None, 0, 0, 0)
                .map_err(os("WinHttpSendRequest"))?;
            WinHttpReceiveResponse(req.0, std::ptr::null_mut())
                .map_err(os("WinHttpReceiveResponse"))?;
            let mut status = 0u32;
            let mut len = size_of::<u32>() as u32;
            WinHttpQueryHeaders(
                req.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some((&raw mut status).cast()),
                &mut len,
                std::ptr::null_mut(),
            )
            .map_err(os("WinHttpQueryHeaders"))?;
            let mut body = Vec::new();
            loop {
                let mut avail = 0u32;
                WinHttpQueryDataAvailable(req.0, &mut avail)
                    .map_err(os("WinHttpQueryDataAvailable"))?;
                if avail == 0 {
                    break;
                }
                let start = body.len();
                body.resize(start + avail as usize, 0u8);
                let mut read = 0u32;
                WinHttpReadData(req.0, body[start..].as_mut_ptr().cast(), avail, &mut read)
                    .map_err(os("WinHttpReadData"))?;
                body.truncate(start + read as usize);
                if body.len() > MAX_BODY {
                    return Err(PlatformError::Other("response too large".into()));
                }
            }
            Ok((
                u16::try_from(status).unwrap_or(0),
                String::from_utf8_lossy(&body).into_owned(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_split() {
        assert_eq!(
            split_https("https://api.github.com/repos/a/b/releases/latest"),
            Some(("api.github.com", "/repos/a/b/releases/latest"))
        );
        assert_eq!(
            split_https("https://example.com"),
            Some(("example.com", "/"))
        );
        assert_eq!(split_https("http://example.com/x"), None);
        assert_eq!(split_https("https:///x"), None);
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Manual (`--ignored --nocapture`): real request to the GitHub releases API.
    #[test]
    #[ignore = "manual: needs internet"]
    fn github_latest_release() {
        let c = WinHttpClient::new("DeskBridge/test");
        let (status, body) = c
            .get("https://api.github.com/repos/auerth/desk_bridge/releases/latest")
            .expect("request");
        eprintln!("status={status} body={}", &body[..body.len().min(300)]);
        assert!(status == 200 || status == 404, "status {status}");
    }
}
