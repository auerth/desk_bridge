//! Well-known locations (plan section 10) and install-dir resolution.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::brand::{AGENT_EXE, COMPANY};

/// Product folder name below `%APPDATA%` / `%LOCALAPPDATA%` (matches `deskbridge-core::paths`).
const PRODUCT_DIR: &str = "DeskBridge";

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `%LOCALAPPDATA%\softwelop\DeskBridge` (logs, Incoming, topology, lid backup).
#[must_use]
pub fn local_data_dir() -> Option<PathBuf> {
    env_path("LOCALAPPDATA").map(|p| p.join(COMPANY).join(PRODUCT_DIR))
}

/// `%APPDATA%\softwelop\DeskBridge` (config, peers, identity).
#[must_use]
pub fn roaming_data_dir() -> Option<PathBuf> {
    env_path("APPDATA").map(|p| p.join(COMPANY).join(PRODUCT_DIR))
}

/// `%APPDATA%\Microsoft\Windows\SendTo`.
#[must_use]
pub fn sendto_dir() -> Option<PathBuf> {
    env_path("APPDATA").map(|p| p.join("Microsoft").join("Windows").join("SendTo"))
}

#[must_use]
pub fn setup_log_file() -> Option<PathBuf> {
    local_data_dir().map(|p| p.join("logs").join("setup.log"))
}

/// Machine-wide copy of the setup log: the installer may run under another (admin) account
/// than the user's, and then `%LOCALAPPDATA%` is not where the user looks.
#[must_use]
pub fn machine_log_file() -> Option<PathBuf> {
    std::env::var_os("ProgramData").map(|p| {
        PathBuf::from(p)
            .join(COMPANY)
            .join(PRODUCT_DIR)
            .join("logs")
            .join("setup.log")
    })
}

#[must_use]
pub fn lid_backup_file() -> Option<PathBuf> {
    local_data_dir().map(|p| p.join(crate::brand::LID_BACKUP_FILE))
}

/// `std::fs::canonicalize` returns `\\?\C:\...` on Windows, which several Win32 APIs reject.
#[must_use]
pub fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p.to_path_buf()
    }
}

/// `--install-dir` if given, else the directory of this executable.
pub fn resolve_install_dir(arg: Option<&Path>) -> Result<PathBuf> {
    let dir = match arg {
        Some(p) => p.to_path_buf(),
        None => std::env::current_exe()
            .context("cannot determine the path of this executable")?
            .parent()
            .context("executable has no parent directory")?
            .to_path_buf(),
    };
    // Make relative arguments absolute without requiring the directory to exist (dry runs).
    let dir = if dir.is_absolute() {
        dir
    } else {
        std::env::current_dir().context("current dir")?.join(dir)
    };
    Ok(strip_verbatim(&dir))
}

#[must_use]
pub fn agent_exe(install_dir: &Path) -> PathBuf {
    install_dir.join(AGENT_EXE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_verbatim_prefixes() {
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\C:\a\b")),
            PathBuf::from(r"C:\a\b")
        );
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\UNC\srv\share")),
            PathBuf::from(r"\\srv\share")
        );
        assert_eq!(strip_verbatim(Path::new(r"C:\a")), PathBuf::from(r"C:\a"));
    }

    #[test]
    fn explicit_install_dir_wins() {
        let d = resolve_install_dir(Some(Path::new(r"C:\Program Files\softwelop\Desk Bridge")))
            .unwrap();
        assert_eq!(d, PathBuf::from(r"C:\Program Files\softwelop\Desk Bridge"));
        assert_eq!(agent_exe(&d).file_name().unwrap(), AGENT_EXE);
    }

    #[test]
    fn default_install_dir_is_exe_dir() {
        let d = resolve_install_dir(None).unwrap();
        assert!(d.is_absolute());
    }

    #[test]
    fn data_dirs_follow_plan_layout() {
        // Only meaningful when the variables exist (always on Windows runners).
        if let Some(d) = local_data_dir() {
            assert!(d.ends_with(Path::new("softwelop").join("DeskBridge")));
        }
        if let Some(f) = setup_log_file() {
            assert!(f.ends_with(Path::new("logs").join("setup.log")));
        }
    }
}
