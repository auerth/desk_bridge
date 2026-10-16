//! Console-session queries and session-change events.

use windows::Win32::System::RemoteDesktop::{
    WTS_CURRENT_SERVER_HANDLE, WTSDomainName, WTSFreeMemory, WTSGetActiveConsoleSessionId,
    WTSQuerySessionInformationW, WTSUserName,
};
use windows::core::PWSTR;

/// Value returned by `WTSGetActiveConsoleSessionId` when there is no console session.
pub const NO_SESSION: u32 = 0xFFFF_FFFF;

/// Session id of the physical console, if there is an interactive one (session 0 is the
/// non-interactive services session and never counts).
#[must_use]
pub fn active_console_session() -> Option<u32> {
    // SAFETY: no arguments.
    let id = unsafe { WTSGetActiveConsoleSessionId() };
    usable_session(id)
}

/// Pure part of [`active_console_session`].
#[must_use]
pub fn usable_session(id: u32) -> Option<u32> {
    (id != NO_SESSION && id != 0).then_some(id)
}

fn query_string(session: u32, class: windows::Win32::System::RemoteDesktop::WTS_INFO_CLASS) -> Option<String> {
    let mut p = PWSTR::null();
    let mut bytes = 0u32;
    // SAFETY: out pointers are valid; the returned buffer is released with WTSFreeMemory.
    unsafe {
        WTSQuerySessionInformationW(WTS_CURRENT_SERVER_HANDLE, session, class, &mut p, &mut bytes)
            .ok()?;
        let s = p.to_string().ok();
        WTSFreeMemory(p.0.cast());
        s
    }
}

/// `DOMAIN\user` of the user logged on in `session`, `None` if nobody is (logon screen).
#[must_use]
pub fn session_user(session: u32) -> Option<String> {
    let user = query_string(session, WTSUserName)?;
    if user.is_empty() {
        return None;
    }
    let domain = query_string(session, WTSDomainName).unwrap_or_default();
    Some(format!("{domain}\\{user}"))
}

/// `WTS_SESSION_*` event codes of `SERVICE_CONTROL_SESSIONCHANGE`.
pub mod event {
    pub const CONSOLE_CONNECT: u32 = 1;
    pub const CONSOLE_DISCONNECT: u32 = 2;
    pub const REMOTE_CONNECT: u32 = 3;
    pub const REMOTE_DISCONNECT: u32 = 4;
    pub const LOGON: u32 = 5;
    pub const LOGOFF: u32 = 6;
    pub const LOCK: u32 = 7;
    pub const UNLOCK: u32 = 8;
    pub const REMOTE_CONTROL: u32 = 9;
}

/// Name for logging.
#[must_use]
pub fn describe_event(event_type: u32) -> &'static str {
    match event_type {
        event::CONSOLE_CONNECT => "console connect",
        event::CONSOLE_DISCONNECT => "console disconnect",
        event::REMOTE_CONNECT => "remote connect",
        event::REMOTE_DISCONNECT => "remote disconnect",
        event::LOGON => "logon",
        event::LOGOFF => "logoff",
        event::LOCK => "lock",
        event::UNLOCK => "unlock",
        event::REMOTE_CONTROL => "remote control",
        _ => "unknown",
    }
}

/// Events after which the supervisor must re-evaluate immediately (instead of at the next
/// 1-second tick). Lock/unlock do not change anything: the agent keeps running on the locked
/// session and the helper is already waiting.
#[must_use]
pub fn wakes_supervisor(event_type: u32) -> bool {
    matches!(
        event_type,
        event::CONSOLE_CONNECT | event::CONSOLE_DISCONNECT | event::LOGON | event::LOGOFF
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids() {
        assert_eq!(usable_session(NO_SESSION), None);
        assert_eq!(usable_session(0), None);
        assert_eq!(usable_session(1), Some(1));
        assert_eq!(usable_session(3), Some(3));
    }

    #[test]
    fn events() {
        assert_eq!(describe_event(event::LOGON), "logon");
        assert_eq!(describe_event(99), "unknown");
        assert!(wakes_supervisor(event::LOGOFF));
        assert!(wakes_supervisor(event::CONSOLE_CONNECT));
        assert!(!wakes_supervisor(event::LOCK));
        assert!(!wakes_supervisor(event::UNLOCK));
    }

    #[test]
    fn console_session_query_does_not_panic() {
        let s = active_console_session();
        if let Some(id) = s {
            let _ = session_user(id);
        }
    }
}
