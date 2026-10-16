//! Diagnostics: a local ZIP with logs, config (no keys), peers (fingerprints only) and system info.
//! Nothing is ever uploaded; the user exports and shares it on purpose (plan 13).

use crate::paths::AppPaths;
use crate::zipwriter::ZipWriter;
use std::path::{Path, PathBuf};

/// Builds the diagnostics archive. `system_info` is a human readable text block.
pub fn export(
    paths: &AppPaths,
    target: Option<&Path>,
    system_info: &str,
) -> std::io::Result<PathBuf> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let out = match target {
        Some(p) if p.extension().is_some() => p.to_path_buf(),
        Some(dir) => dir.join(format!("deskbridge-diagnostics-{ts}.zip")),
        None => paths
            .data_dir
            .join(format!("deskbridge-diagnostics-{ts}.zip")),
    };
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(&out)?;
    let mut zip = ZipWriter::new(std::io::BufWriter::new(file));

    zip.add("system-info.txt", system_info.as_bytes())?;
    if let Ok(cfg) = std::fs::read_to_string(paths.config_file()) {
        zip.add("config.toml", cfg.as_bytes())?;
    }
    // Peers contain fingerprints only (public information), never the private key.
    if let Ok(p) = std::fs::read_to_string(paths.peers_file()) {
        zip.add("peers.toml", p.as_bytes())?;
    }
    if let Ok(rd) = std::fs::read_dir(paths.logs_dir()) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if e.metadata()
                .is_ok_and(|m| m.is_file() && m.len() < 32 * 1024 * 1024)
                && let Ok(data) = std::fs::read(e.path())
            {
                zip.add(&format!("logs/{name}"), &data)?;
            }
        }
    }
    zip.finish()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_contains_logs_and_never_the_identity() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(dir.path());
        paths.ensure().unwrap();
        std::fs::write(paths.identity_file(), b"SECRET-KEY-MATERIAL").unwrap();
        std::fs::write(paths.config_file(), "schema_version = 1\n").unwrap();
        std::fs::write(paths.logs_dir().join("agent.log.2026-10-16"), "hello log").unwrap();
        let out = export(&paths, None, "os: test").unwrap();
        if let Ok(keep) = std::env::var("DB_KEEP_ZIP") {
            std::fs::copy(&out, keep).unwrap();
        }
        let bytes = std::fs::read(out).unwrap();
        assert!(!bytes.windows(10).any(|w| w == b"SECRET-KEY"));
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("system-info.txt"));
        assert!(text.contains("config.toml"));
        assert!(text.contains("logs/agent.log.2026-10-16"));
        assert!(!text.contains("identity"));
    }
}
