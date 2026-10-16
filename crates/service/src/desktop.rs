//! Input-desktop detection and attachment.
//!
//! Windows has several desktops per session: `Default` (the user's windows), `Winlogon` (UAC
//! consent, lock screen, logon screen, Ctrl+Alt+Del screen) and `Screen-saver`. Only the thread
//! attached to the *input* desktop can inject input there, and a normal user process is not even
//! allowed to open the Winlogon desktop.

use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, HANDLE};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, GetUserObjectInformationW, HDESK,
    OpenInputDesktop, SetThreadDesktop, UOI_NAME,
};

/// Which side of the desktop split input must go to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopKind {
    /// The user's `Default` desktop: normal injection works.
    Normal,
    /// Winlogon / screen saver / anything else: only the SYSTEM helper can inject.
    Secure,
}

/// Pure classification of what `OpenInputDesktop` + `UOI_NAME` returned.
///
/// * `name = Some(n)`: `Default` (any case) is normal, everything else is secure.
/// * `name = None` with `access_denied`: the typical answer of a user process while the secure
///   desktop is active, so secure.
/// * `name = None` otherwise (transient failure during a desktop switch): assume normal rather
///   than flapping to the helper.
#[must_use]
pub fn classify(name: Option<&str>, access_denied: bool) -> DesktopKind {
    match name {
        Some(n) if n.eq_ignore_ascii_case("Default") => DesktopKind::Normal,
        Some(_) => DesktopKind::Secure,
        None if access_denied => DesktopKind::Secure,
        None => DesktopKind::Normal,
    }
}

/// Result of probing the input desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDesktop {
    pub kind: DesktopKind,
    pub name: Option<String>,
}

/// Name of the desktop a handle refers to.
fn desktop_name(h: HDESK) -> Option<String> {
    let mut buf = [0u16; 128];
    let mut needed = 0u32;
    // SAFETY: `buf` is valid for `size_of_val(&buf)` bytes; UOI_NAME yields a NUL-terminated
    // UTF-16 string.
    unsafe {
        GetUserObjectInformationW(
            HANDLE(h.0),
            UOI_NAME,
            Some(buf.as_mut_ptr().cast()),
            size_of_val(&buf) as u32,
            Some(&mut needed),
        )
    }
    .ok()?;
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Opens the current input desktop with `access`. `Err(code)` carries the Win32 error.
fn open_input(access: u32) -> Result<HDESK, u32> {
    // SAFETY: no pointer arguments.
    unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS_FLAGS(access)) }
        .map_err(|e| (e.code().0 & 0xFFFF) as u32)
}

/// Cheap probe used by the agent: which desktop receives input right now?
#[must_use]
pub fn probe_input_desktop() -> InputDesktop {
    // DESKTOP_READOBJECTS (0x1) is enough to read the name.
    match open_input(0x1) {
        Ok(h) => {
            let name = desktop_name(h);
            // SAFETY: `h` was just opened by us.
            unsafe {
                let _ = CloseDesktop(h);
            }
            InputDesktop { kind: classify(name.as_deref(), false), name }
        }
        Err(code) => {
            InputDesktop { kind: classify(None, code == ERROR_ACCESS_DENIED.0), name: None }
        }
    }
}

/// Keeps the calling thread attached to the input desktop (helper side).
///
/// `SetThreadDesktop` fails for threads that own windows or hooks, so this must be used from a
/// dedicated plain thread.
#[derive(Default)]
pub struct ThreadDesktop {
    current: Option<(HDESK, String)>,
}

// SAFETY: the desktop handle is only used by the owning thread; the struct is moved to that
// thread once before use.
unsafe impl Send for ThreadDesktop {}

impl ThreadDesktop {
    /// Attaches the thread to the current input desktop if it changed. Returns `Ok(true)` when
    /// the attachment changed.
    pub fn follow(&mut self) -> Result<bool, u32> {
        // GENERIC_ALL: SetThreadDesktop needs DESKTOP_SWITCHDESKTOP-class rights and SYSTEM has them.
        let h = open_input(0x1000_0000)?;
        let name = desktop_name(h).unwrap_or_default();
        if self.current.as_ref().is_some_and(|(_, n)| *n == name) {
            // SAFETY: handle opened above and not stored.
            unsafe {
                let _ = CloseDesktop(h);
            }
            return Ok(false);
        }
        // SAFETY: `h` is a valid desktop handle; on success the thread keeps using it until the
        // next switch, so it is closed only after being replaced.
        if let Err(e) = unsafe { SetThreadDesktop(h) } {
            // SAFETY: as above.
            unsafe {
                let _ = CloseDesktop(h);
            }
            return Err((e.code().0 & 0xFFFF) as u32);
        }
        if let Some((old, _)) = self.current.replace((h, name)) {
            // SAFETY: the thread no longer uses `old`.
            unsafe {
                let _ = CloseDesktop(old);
            }
        }
        Ok(true)
    }

    #[must_use]
    pub fn current_name(&self) -> Option<&str> {
        self.current.as_ref().map(|(_, n)| n.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(classify(Some("Default"), false), DesktopKind::Normal);
        assert_eq!(classify(Some("default"), false), DesktopKind::Normal);
        assert_eq!(classify(Some("Winlogon"), false), DesktopKind::Secure);
        assert_eq!(classify(Some("Screen-saver"), false), DesktopKind::Secure);
        assert_eq!(classify(Some(""), false), DesktopKind::Secure);
        assert_eq!(classify(None, true), DesktopKind::Secure);
        assert_eq!(classify(None, false), DesktopKind::Normal);
        // A readable name wins over the error flag.
        assert_eq!(classify(Some("Default"), true), DesktopKind::Normal);
    }

    #[test]
    fn probe_on_a_normal_session_is_normal() {
        // The tests run on the interactive desktop (or a CI desktop named "Default").
        let d = super::probe_input_desktop();
        if let Some(name) = &d.name {
            assert_eq!(d.kind, classify(Some(name), false));
        }
    }
}
