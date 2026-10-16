//! `installed.json`: what `install` created, so `uninstall` removes exactly that.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::brand::MANIFEST_FILE;

pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverRecord {
    pub name: String,
    /// INF path at install time (inside the install dir).
    pub inf: String,
    pub hwid: String,
    /// Files copied next to the driver (removed again on uninstall).
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub version: String,
    pub install_dir: String,
    pub installed_at_unix: u64,
    #[serde(default)]
    pub firewall_rule: Option<String>,
    #[serde(default)]
    pub scheduled_task: Option<String>,
    /// Full path of the `.lnk` in the installing user's SendTo folder.
    #[serde(default)]
    pub send_to_shortcut: Option<String>,
    #[serde(default)]
    pub service: Option<String>,
    /// The installer created the `SoftwareSASGeneration` policy value (and removes it again).
    #[serde(default)]
    pub sas_policy: bool,
    #[serde(default)]
    pub drivers: Vec<DriverRecord>,
}

impl Manifest {
    #[must_use]
    pub fn new(install_dir: &Path) -> Self {
        Self {
            schema: SCHEMA,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            install_dir: install_dir.display().to_string(),
            installed_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            firewall_rule: None,
            scheduled_task: None,
            send_to_shortcut: None,
            service: None,
            sas_policy: false,
            drivers: Vec::new(),
        }
    }

    #[must_use]
    pub fn path(install_dir: &Path) -> PathBuf {
        install_dir.join(MANIFEST_FILE)
    }

    /// `Ok(None)` when no manifest exists (never installed or already uninstalled).
    pub fn load(install_dir: &Path) -> Result<Option<Self>> {
        let p = Self::path(install_dir);
        match std::fs::read_to_string(&p) {
            Ok(text) => {
                let m: Self = serde_json::from_str(&text)
                    .with_context(|| format!("parsing {}", p.display()))?;
                Ok(Some(m))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
        }
    }

    /// Atomic write (temp file + rename).
    pub fn save(&self, install_dir: &Path) -> Result<()> {
        let p = Self::path(install_dir);
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &p).with_context(|| format!("renaming to {}", p.display()))?;
        Ok(())
    }

    pub fn remove_file(install_dir: &Path) -> Result<()> {
        match std::fs::remove_file(Self::path(install_dir)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Merge a driver record, replacing an older one for the same hardware id.
    pub fn upsert_driver(&mut self, rec: DriverRecord) {
        self.drivers
            .retain(|d| !d.hwid.eq_ignore_ascii_case(&rec.hwid));
        self.drivers.push(rec);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Manifest::load(dir.path()).unwrap().is_none());
        let mut m = Manifest::new(dir.path());
        m.firewall_rule = Some("rule".into());
        m.upsert_driver(DriverRecord {
            name: "a".into(),
            inf: "a.inf".into(),
            hwid: r"Root\A".into(),
            files: Vec::new(),
        });
        m.upsert_driver(DriverRecord {
            name: "a2".into(),
            inf: "a2.inf".into(),
            hwid: r"root\a".into(),
            files: Vec::new(),
        });
        m.save(dir.path()).unwrap();
        let back = Manifest::load(dir.path()).unwrap().unwrap();
        assert_eq!(back, m);
        assert_eq!(back.drivers.len(), 1);
        assert_eq!(back.drivers[0].name, "a2");
        Manifest::remove_file(dir.path()).unwrap();
        Manifest::remove_file(dir.path()).unwrap();
        assert!(Manifest::load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn tolerates_unknown_and_missing_fields() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            Manifest::path(dir.path()),
            r#"{"schema":1,"version":"0","install_dir":"x","installed_at_unix":1,"future":true}"#,
        )
        .unwrap();
        let m = Manifest::load(dir.path()).unwrap().unwrap();
        assert!(m.drivers.is_empty() && m.firewall_rule.is_none());
    }

    #[test]
    fn corrupt_manifest_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(Manifest::path(dir.path()), "{ nope").unwrap();
        assert!(Manifest::load(dir.path()).is_err());
    }
}
