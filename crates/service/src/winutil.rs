//! Small Win32 helpers shared by the service modules.

use std::ffi::c_void;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TOKEN_USER, TokenUser};
use windows::core::PWSTR;

/// A kernel handle closed on drop.
#[derive(Debug)]
pub struct OwnedHandle(pub HANDLE);

// SAFETY: kernel handles may be used and closed from any thread.
unsafe impl Send for OwnedHandle {}
// SAFETY: see above; all uses are plain wait/query calls.
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
    #[must_use]
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: the handle is owned by this wrapper and closed exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// NUL-terminated UTF-16 copy of `s`.
#[must_use]
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// String form (`S-1-5-21-...`) of the user SID in `token`.
#[must_use]
pub fn token_user_sid(token: HANDLE) -> Option<String> {
    // SAFETY: two-step GetTokenInformation with a buffer sized by the first call; the SID pointer
    // inside the buffer stays valid while `buf` lives.
    unsafe {
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast::<c_void>()), len, &mut len)
            .ok()?;
        let user = &*buf.as_ptr().cast::<TOKEN_USER>();
        let mut sid = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid).ok()?;
        let s = sid.to_string().ok();
        let _ = LocalFree(HLOCAL(sid.0.cast()));
        s
    }
}

/// SID string of the user the current process runs as.
#[must_use]
pub fn current_user_sid() -> Option<String> {
    use windows::Win32::Security::TOKEN_QUERY;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token = HANDLE::default();
    // SAFETY: valid out pointer; the token is closed by the wrapper.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.ok()?;
    let token = OwnedHandle(token);
    token_user_sid(token.raw())
}

/// `%ProgramData%\softwelop\DeskBridge`.
#[must_use]
pub fn program_data_dir() -> std::path::PathBuf {
    let base = std::env::var_os("ProgramData")
        .map_or_else(|| std::path::PathBuf::from(r"C:\ProgramData"), std::path::PathBuf::from);
    base.join("softwelop").join("DeskBridge")
}

/// Directory of the running executable (agent and helper live next to it).
#[must_use]
pub fn exe_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_default()
}
