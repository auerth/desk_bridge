//! Security descriptors for the service pipes.
//!
//! The helper injects input as SYSTEM, so the pipe is an elevation-of-privilege surface: a
//! non-elevated process of the same user must not be able to use it to bypass UIPI (typing into
//! the UAC prompt). Two layers: the DACL admits only SYSTEM and the interactive user, and a
//! mandatory label `NW` at *High* integrity keeps medium-integrity processes from opening the
//! pipe for writing (the elevated agent runs at High).

use crate::winutil::wide;
use std::ffi::c_void;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::core::PCWSTR;

/// Who may open a service pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeAccess {
    /// SID string of the interactive user; `None` admits SYSTEM only.
    pub user_sid: Option<String>,
    /// Require a High-integrity (elevated) client for write access.
    pub require_high_integrity: bool,
}

impl PipeAccess {
    /// Production policy: SYSTEM + `user_sid`, elevated clients only.
    #[must_use]
    pub fn production(user_sid: Option<String>) -> Self {
        Self {
            user_sid,
            require_high_integrity: true,
        }
    }
}

/// Accepts only `S-1-<digits and dashes>`; the SID ends up inside an SDDL string, so anything
/// else (parentheses, semicolons) could change the meaning of the descriptor.
#[must_use]
pub fn is_valid_sid_string(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("S-1-") else {
        return false;
    };
    !rest.is_empty()
        && rest.len() <= 120
        && rest
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

/// SDDL for [`PipeAccess`]. Invalid SIDs are dropped (SYSTEM-only is the safe fallback).
#[must_use]
pub fn build_sddl(access: &PipeAccess) -> String {
    let mut s = String::from("D:P(A;;GA;;;SY)");
    if let Some(sid) = access
        .user_sid
        .as_deref()
        .filter(|s| is_valid_sid_string(s))
    {
        s.push_str(&format!("(A;;GA;;;{sid})"));
    }
    if access.require_high_integrity {
        s.push_str("S:(ML;;NW;;;HI)");
    }
    s
}

/// Owns a security descriptor converted from SDDL.
pub struct SecurityDescriptor {
    sd: PSECURITY_DESCRIPTOR,
}

// SAFETY: the descriptor is immutable heap memory freed on drop.
unsafe impl Send for SecurityDescriptor {}

impl SecurityDescriptor {
    pub fn from_sddl(sddl: &str) -> std::io::Result<Self> {
        let w = wide(sddl);
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `w` is NUL-terminated; the result is LocalFree'd in Drop.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(w.as_ptr()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
        }
        .map_err(|e| std::io::Error::other(format!("SDDL '{sddl}': {e}")))?;
        Ok(Self { sd })
    }

    /// `SECURITY_ATTRIBUTES` borrowing this descriptor (valid while `self` lives).
    #[must_use]
    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.sd.0,
            bInheritHandle: false.into(),
        }
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by the SDDL conversion with LocalAlloc.
        unsafe {
            let _ = LocalFree(HLOCAL(self.sd.0.cast::<c_void>()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "S-1-5-21-1004336348-1177238915-682003330-1000";

    #[test]
    fn sid_validation() {
        assert!(is_valid_sid_string(SID));
        assert!(is_valid_sid_string("S-1-5-18"));
        for bad in [
            "",
            "S-1-",
            "S-1-5-",
            "S-2-5-18",
            "S-1-5-18)(A;;GA;;;WD",
            "S-1-5-1 8",
            "x",
            "S-1-a",
        ] {
            assert!(!is_valid_sid_string(bad), "{bad}");
        }
    }

    #[test]
    fn production_sddl() {
        let s = build_sddl(&PipeAccess::production(Some(SID.into())));
        assert_eq!(s, format!("D:P(A;;GA;;;SY)(A;;GA;;;{SID})S:(ML;;NW;;;HI)"));
    }

    #[test]
    fn no_user_means_system_only() {
        let s = build_sddl(&PipeAccess {
            user_sid: None,
            require_high_integrity: false,
        });
        assert_eq!(s, "D:P(A;;GA;;;SY)");
    }

    #[test]
    fn hostile_sid_is_dropped() {
        let s = build_sddl(&PipeAccess {
            user_sid: Some("S-1-5-18)(A;;GA;;;WD".into()),
            require_high_integrity: true,
        });
        assert_eq!(s, "D:P(A;;GA;;;SY)S:(ML;;NW;;;HI)");
        assert!(!s.contains("WD"));
    }

    #[test]
    fn descriptors_convert() {
        let user = crate::winutil::current_user_sid().expect("own SID");
        for high in [false, true] {
            let sddl = build_sddl(&PipeAccess {
                user_sid: Some(user.clone()),
                require_high_integrity: high,
            });
            let sd = SecurityDescriptor::from_sddl(&sddl).expect("valid SDDL");
            assert_eq!(
                sd.attributes().nLength as usize,
                size_of::<SECURITY_ATTRIBUTES>()
            );
        }
    }
}
