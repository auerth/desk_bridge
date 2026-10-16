//! Publisher trust for user-mode drivers signed by a third-party certificate (ADR 0004).
//!
//! The virtual display driver is signed through SignPath Foundation, not by Microsoft. Windows
//! installs such a driver silently only if the publisher certificate is in `TrustedPublisher`.
//! This module imports exactly one certificate whose SHA-1 thumbprint is pinned in the driver
//! catalogue, and only when the owner enabled the driver (`driver.json`: `"enabled": true`).
//! What was added is recorded in `installed.json` and removed again on uninstall.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

fn certutil(args: &[&str]) -> Result<String> {
    let out = Command::new("certutil.exe")
        .args(args)
        .output()
        .context("running certutil.exe")?;
    if !out.status.success() {
        bail!(
            "certutil {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stdout).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Normalises a thumbprint: hex digits only, lowercase.
#[must_use]
pub fn normalize(t: &str) -> String {
    t.chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Extracts the SHA-1 hash from `certutil -hashfile <file> SHA1` output.
#[must_use]
pub fn parse_hashfile(out: &str) -> Option<String> {
    out.lines()
        .map(str::trim)
        .find(|l| {
            let n = normalize(l);
            n.len() == 40 && l.chars().all(|c| c.is_ascii_hexdigit() || c == ' ')
        })
        .map(normalize)
}

pub fn file_sha1(file: &Path) -> Result<String> {
    let out = certutil(&["-hashfile", &file.display().to_string(), "SHA1"])?;
    parse_hashfile(&out).context("could not read the certificate hash")
}

/// Is a certificate with this thumbprint in `TrustedPublisher` (machine store)?
pub fn is_trusted(sha1: &str) -> bool {
    certutil(&["-store", "TrustedPublisher", &normalize(sha1)]).is_ok()
}

/// Imports `file` into `TrustedPublisher` after checking its thumbprint against `expected_sha1`.
/// Returns `true` if the certificate was added by this call (so uninstall may remove it).
pub fn add_publisher(file: &Path, expected_sha1: &str) -> Result<bool> {
    let expected = normalize(expected_sha1);
    let actual = file_sha1(file)?;
    if actual != expected {
        bail!(
            "certificate {} has thumbprint {actual}, expected {expected}; not importing it",
            file.display()
        );
    }
    if is_trusted(&expected) {
        return Ok(false);
    }
    certutil(&[
        "-f",
        "-addstore",
        "TrustedPublisher",
        &file.display().to_string(),
    ])?;
    Ok(true)
}

pub fn remove_publisher(sha1: &str) -> Result<()> {
    let t = normalize(sha1);
    if !is_trusted(&t) {
        return Ok(());
    }
    certutil(&["-delstore", "TrustedPublisher", &t])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbprints_are_normalised() {
        assert_eq!(
            normalize("3C F8 CF 26-D8BA266C3A483AB7D26D4A818E317D76"),
            "3cf8cf26d8ba266c3a483ab7d26d4a818e317d76"
        );
    }

    #[test]
    fn hashfile_output_is_parsed() {
        let out = "SHA1-Hash von x.cer:\r\n3c f8 cf 26 d8 ba 26 6c 3a 48 3a b7 d2 6d 4a 81 8e 31 7d 76\r\nCertUtil: erfolgreich.\r\n";
        assert_eq!(
            parse_hashfile(out).as_deref(),
            Some("3cf8cf26d8ba266c3a483ab7d26d4a818e317d76")
        );
        assert!(parse_hashfile("nothing here").is_none());
    }
}
