//! The virtual display driver's settings file and control pipe.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use deskbridge_display::util::{atomic_write, run_with_timeout};
use deskbridge_display::vdd::{DEFAULT_SETTINGS_DIR, PIPE_NAME, SETTINGS_FILE_NAME, pipe_message};
use deskbridge_platform::{PlatformError, Result};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::core::w;

/// Folder of `vdd_settings.xml`: `HKLM\SOFTWARE\MikeTheTech\VirtualDisplayDriver\VDDPATH` when
/// set, else `C:\VirtualDisplayDriver`.
#[must_use]
pub fn default_settings_dir() -> PathBuf {
    registry_vddpath().unwrap_or_else(|| PathBuf::from(DEFAULT_SETTINGS_DIR))
}

fn registry_vddpath() -> Option<PathBuf> {
    let mut buf = vec![0u16; 520];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: `buf` is `len` bytes large; RegGetValueW writes at most `len` bytes and updates it.
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"SOFTWARE\MikeTheTech\VirtualDisplayDriver"),
            w!("VDDPATH"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    buf.truncate((len as usize / 2).min(buf.len()));
    let s = super::ccd::wide(&buf);
    let s = s.trim();
    (!s.is_empty()).then(|| PathBuf::from(s))
}

#[must_use]
pub fn settings_file(dir: &Path) -> PathBuf {
    dir.join(SETTINGS_FILE_NAME)
}

/// The current settings text, or `None` when the file does not exist / is not valid UTF-8.
#[must_use]
pub fn read_settings(dir: &Path) -> Option<String> {
    let bytes = std::fs::read(settings_file(dir)).ok()?;
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    String::from_utf8(bytes.to_vec()).ok()
}

/// Atomically replaces the settings file (creates the folder if necessary).
pub fn write_settings(dir: &Path, xml: &str) -> Result<()> {
    atomic_write(&settings_file(dir), xml.as_bytes()).map_err(|e| {
        PlatformError::other(format!(
            "cannot write {} (administrator rights needed?): {e}",
            settings_file(dir).display()
        ))
    })
}

/// Sends one command to the driver's pipe (`RELOAD_DRIVER`, `SETGPU "<name>"`). The driver reads
/// a single UTF-16LE message and closes the pipe.
pub fn send_pipe_command(cmd: &str) -> Result<()> {
    let msg = pipe_message(cmd);
    run_with_timeout("vdd-pipe", Duration::from_secs(3), move || {
        let mut last = None;
        for _ in 0..3 {
            match std::fs::OpenOptions::new().write(true).open(PIPE_NAME) {
                Ok(mut pipe) => {
                    return pipe
                        .write_all(&msg)
                        .and_then(|()| pipe.flush())
                        .map_err(|e| {
                            PlatformError::other(format!("writing to the driver pipe: {e}"))
                        });
                }
                Err(e) => {
                    // 231 = ERROR_PIPE_BUSY: the driver serves one client at a time.
                    let busy = e.raw_os_error() == Some(231);
                    last = Some(e);
                    if !busy {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        Err(PlatformError::other(format!(
            "driver pipe not available: {}",
            last.map_or_else(|| "unknown".to_owned(), |e| e.to_string())
        )))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_in_temp_dir() {
        let d = tempfile::tempdir().unwrap();
        assert!(read_settings(d.path()).is_none());
        write_settings(d.path(), "\u{feff}<vdd_settings/>").ok();
        assert!(read_settings(d.path()).unwrap().contains("vdd_settings"));
        write_settings(&d.path().join("sub"), "<a/>").unwrap();
        assert_eq!(read_settings(&d.path().join("sub")).unwrap(), "<a/>");
    }

    #[test]
    fn default_dir_resolves() {
        // Read-only registry lookup; either the registry value or the default folder.
        let p = default_settings_dir();
        assert!(!p.as_os_str().is_empty());
    }
}
