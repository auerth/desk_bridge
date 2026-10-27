//! Keeps Teams from showing "away": Teams goes away after about five minutes without input on the
//! machine, so on a PC that is only used through Desk Bridge the idle timer is reset now and then.
//!
//! * `teams_running`: a ToolHelp snapshot looks for `ms-teams.exe` (new Teams) or `Teams.exe`
//!   (classic) in this process's session, so another user's Teams on a terminal server does not count.
//! * `idle_for`: `GetLastInputInfo` (the same clock Teams reads), which injected input also moves.
//! * `nudge`: F15 down and up. No keyboard has the key and no application binds it, but it is real
//!   keyboard input to Windows. Marked with [`INJECT_MARKER`] so our own capture hook ignores it.
//!   It does nothing on a locked desktop; Teams shows "away" there anyway.

use std::mem::size_of;
use std::time::Duration;

use deskbridge_platform::presence::Presence;
use deskbridge_platform::{PlatformError, Result};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetLastInputInfo, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, LASTINPUTINFO,
    SendInput, VIRTUAL_KEY, VK_F15,
};

use crate::input::INJECT_MARKER;

/// Executable names of Microsoft Teams (new and classic).
const TEAMS_EXES: [&str; 2] = ["ms-teams.exe", "teams.exe"];

#[derive(Debug, Default, Clone, Copy)]
pub struct WinPresence;

impl WinPresence {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Presence for WinPresence {
    fn teams_running(&self) -> bool {
        let mut own = 0u32;
        // SAFETY: plain out-parameter.
        if unsafe { ProcessIdToSessionId(std::process::id(), &mut own) }.is_err() {
            return false;
        }
        // SAFETY: standard snapshot walk; `entry.dwSize` is initialised as required and the
        // snapshot handle is closed on every path.
        unsafe {
            let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                return false;
            };
            let mut entry = PROCESSENTRY32W {
                dwSize: size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut found = false;
            let mut ok = Process32FirstW(snap, &mut entry).is_ok();
            while ok && !found {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                if TEAMS_EXES.iter().any(|t| name.eq_ignore_ascii_case(t)) {
                    let mut sid = u32::MAX;
                    found =
                        ProcessIdToSessionId(entry.th32ProcessID, &mut sid).is_ok() && sid == own;
                }
                ok = Process32NextW(snap, &mut entry).is_ok();
            }
            let _ = CloseHandle(snap);
            found
        }
    }

    fn idle_for(&self) -> Option<Duration> {
        let mut info = LASTINPUTINFO {
            cbSize: size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: `info.cbSize` is set; both tick counts wrap together, so the wrapping
        // difference is right for up to 49 days of idle time.
        unsafe {
            if !GetLastInputInfo(&mut info).as_bool() {
                return None;
            }
            let ms = GetTickCount().wrapping_sub(info.dwTime);
            Some(Duration::from_millis(u64::from(ms)))
        }
    }

    fn nudge(&self) -> Result<()> {
        let key = |flags| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(VK_F15.0),
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: INJECT_MARKER,
                },
            },
        };
        let inputs = [key(Default::default()), key(KEYEVENTF_KEYUP)];
        // SAFETY: `inputs` is a valid slice of fully initialised INPUT structs.
        let n = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
        if n as usize == inputs.len() {
            Ok(())
        } else {
            Err(PlatformError::other(format!(
                "SendInput took {n}/{} events: {}",
                inputs.len(),
                windows::core::Error::from_win32()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_time_and_process_scan_work() {
        let p = WinPresence::new();
        assert!(p.idle_for().is_some(), "GetLastInputInfo failed");
        // Whether Teams runs depends on the machine; the scan must simply not fail or hang.
        let _ = p.teams_running();
    }
}
