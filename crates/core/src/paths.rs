//! Data locations (plan section 10).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AppPaths {
    /// `%APPDATA%\softwelop\DeskBridge`
    pub config_dir: PathBuf,
    /// `%LOCALAPPDATA%\softwelop\DeskBridge`
    pub data_dir: PathBuf,
}

impl AppPaths {
    /// Standard locations for the current user.
    #[must_use]
    pub fn system() -> Self {
        let dirs = directories::ProjectDirs::from("", "softwelop", "DeskBridge");
        match dirs {
            Some(d) => Self {
                // ProjectDirs on Windows yields ...\softwelop\DeskBridge\config and ...\data.
                config_dir: d
                    .config_dir()
                    .parent()
                    .map_or_else(|| d.config_dir().to_path_buf(), Path::to_path_buf),
                data_dir: d
                    .data_local_dir()
                    .parent()
                    .map_or_else(|| d.data_local_dir().to_path_buf(), Path::to_path_buf),
            },
            None => {
                let base = std::env::temp_dir().join("softwelop").join("DeskBridge");
                Self {
                    config_dir: base.clone(),
                    data_dir: base,
                }
            }
        }
    }

    /// Everything below one directory (tests, `dev-two-instances.ps1`).
    #[must_use]
    pub fn under(root: &Path) -> Self {
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
        }
    }

    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    #[must_use]
    pub fn peers_file(&self) -> PathBuf {
        self.config_dir.join("peers.toml")
    }

    #[must_use]
    pub fn identity_file(&self) -> PathBuf {
        self.config_dir.join("identity.bin")
    }

    #[must_use]
    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    #[must_use]
    pub fn incoming_dir(&self) -> PathBuf {
        self.data_dir.join("Incoming")
    }

    #[must_use]
    pub fn topology_file(&self) -> PathBuf {
        self.data_dir.join("topology.json")
    }

    #[must_use]
    pub fn history_file(&self) -> PathBuf {
        self.data_dir.join("transfers.json")
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(self.logs_dir())?;
        Ok(())
    }
}
