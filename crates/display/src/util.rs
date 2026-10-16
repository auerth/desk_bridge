//! Small helpers shared by the session logic and the Windows implementation.

use std::io::Write;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use deskbridge_platform::{PlatformError, Result};

/// Writes `bytes` to `path` atomically: temp file in the same directory, flushed to disk, then
/// renamed over the target (`MoveFileEx(REPLACE_EXISTING)` on Windows). A crash leaves either
/// the old or the new content, never a torn file.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(d) = dir {
        std::fs::create_dir_all(d)?;
    }
    let mut tmp_name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Runs `f` on a helper thread and waits at most `timeout` for it.
///
/// On timeout the thread is abandoned (a blocked OS call cannot be cancelled) and an error is
/// returned, so callers can roll back instead of hanging forever. A panic inside `f` is
/// reported as an error as well.
pub fn run_with_timeout<T, F>(label: &str, timeout: Duration, f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name(format!("db-display-{label}"))
        .spawn(move || {
            let _ = tx.send(f());
        });
    if let Err(e) = spawned {
        return Err(PlatformError::other(format!(
            "{label}: cannot spawn worker: {e}"
        )));
    }
    match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(PlatformError::other(format!(
            "{label}: timed out after {} ms",
            timeout.as_millis()
        ))),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(PlatformError::other(format!("{label}: worker panicked")))
        }
    }
}

/// Milliseconds since the Unix epoch.
#[must_use]
pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("a.json");
        atomic_write(&p, b"one").unwrap();
        atomic_write(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        let n = std::fs::read_dir(p.parent().unwrap()).unwrap().count();
        assert_eq!(n, 1);
    }

    #[test]
    fn timeout_and_panic_are_errors() {
        let ok = run_with_timeout("t", Duration::from_secs(2), || Ok(5)).unwrap();
        assert_eq!(ok, 5);
        let slow: Result<()> = run_with_timeout("slow", Duration::from_millis(30), || {
            std::thread::sleep(Duration::from_millis(500));
            Ok(())
        });
        assert!(slow.unwrap_err().to_string().contains("timed out"));
        let p: Result<()> = run_with_timeout("p", Duration::from_secs(2), || panic!("boom"));
        assert!(p.unwrap_err().to_string().contains("panicked"));
    }
}
