//! Who is using a capture endpoint right now (for the "Laptop nutzt dein Mikro (Teams)" hint).

use super::wasapi::{ComGuard, enumerator, open_device, os_err};
use deskbridge_platform::Result;
use deskbridge_platform::audio::DeviceKind;
use windows::Win32::Foundation::{CloseHandle, S_OK};
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2, eCommunications,
};
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::core::{Interface, PWSTR};

/// A process holding an active audio session on a capture endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicSession {
    pub pid: u32,
    /// Executable file name, e.g. `ms-teams.exe`; `pid <n>` if it cannot be determined.
    pub process_name: String,
}

/// File name component of a Windows path.
pub(crate) fn file_name_of(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn process_name(pid: u32) -> String {
    // SAFETY: the process handle is closed before returning; buffer/len are consistent.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return format!("pid {pid}");
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        match r {
            Ok(()) => file_name_of(&String::from_utf16_lossy(&buf[..len as usize])).to_string(),
            Err(_) => format!("pid {pid}"),
        }
    }
}

/// Lists the processes with an *active* capture session on the capture endpoint `device_id`
/// (`None` = default communications microphone). The calling process and the system-sounds
/// session are excluded, so Desk Bridge's own capture never shows up as "someone uses the mic".
pub fn mic_session_enumeration(device_id: Option<&str>) -> Result<Vec<MicSession>> {
    let _com = ComGuard::init();
    let en = enumerator().map_err(os_err)?;
    let dev = open_device(&en, DeviceKind::Capture, device_id, eCommunications)?;
    let mut out: Vec<MicSession> = Vec::new();
    // SAFETY: all interfaces are owned wrappers released on drop, on this thread.
    unsafe {
        let mgr: IAudioSessionManager2 = dev.Activate(CLSCTX_ALL, None).map_err(os_err)?;
        let sessions = mgr.GetSessionEnumerator().map_err(os_err)?;
        let own = GetCurrentProcessId();
        for i in 0..sessions.GetCount().map_err(os_err)? {
            let Ok(ctl) = sessions.GetSession(i) else {
                continue;
            };
            if ctl.GetState().ok() != Some(AudioSessionStateActive) {
                continue;
            }
            let Ok(ctl2) = ctl.cast::<IAudioSessionControl2>() else {
                continue;
            };
            // S_OK means "is the system-sounds session" (S_FALSE: it is not).
            if ctl2.IsSystemSoundsSession() == S_OK {
                continue;
            }
            let Ok(pid) = ctl2.GetProcessId() else {
                continue;
            };
            if pid == 0 || pid == own || out.iter().any(|s| s.pid == pid) {
                continue;
            }
            out.push(MicSession {
                pid,
                process_name: process_name(pid),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names() {
        assert_eq!(
            file_name_of(r"C:\Program Files\WindowsApps\ms-teams.exe"),
            "ms-teams.exe"
        );
        assert_eq!(file_name_of("zoom.exe"), "zoom.exe");
        assert_eq!(file_name_of("C:/a/b.exe"), "b.exe");
    }

    #[test]
    fn enumeration_tolerates_missing_devices() {
        match mic_session_enumeration(None) {
            Ok(v) => assert!(v.iter().all(|s| s.pid != 0)),
            Err(e) => eprintln!("skipping: {e}"),
        }
    }
}
