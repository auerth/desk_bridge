//! `SoftwareSASGeneration` policy: allows the Desk Bridge *service* to generate the secure
//! attention sequence (Ctrl+Alt+Del) for the "Strg+Alt+Ende" hotkey (plan 8.1, ADR 0006).
//!
//! The installer sets it only if the administrator has not configured the value, records that it
//! did, and removes exactly its own change on uninstall. Opt out with `--no-sas-policy`.

use anyhow::{Context, Result, bail};
use std::process::Command;

const KEY: &str = r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
const VALUE: &str = "SoftwareSASGeneration";

fn reg(args: &[&str]) -> Result<std::process::Output> {
    Command::new("reg.exe").args(args).output().context("running reg.exe")
}

/// Current value, `None` if not configured.
pub fn current() -> Result<Option<u32>> {
    let out = reg(&["query", KEY, "/v", VALUE])?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(parse_query(&String::from_utf8_lossy(&out.stdout)))
}

/// Parses `reg query` output: `    SoftwareSASGeneration    REG_DWORD    0x1`.
#[must_use]
pub fn parse_query(text: &str) -> Option<u32> {
    let line = text.lines().find(|l| l.contains(VALUE))?;
    let raw = line.split_whitespace().last()?;
    match raw.strip_prefix("0x") {
        Some(h) => u32::from_str_radix(h, 16).ok(),
        None => raw.parse().ok(),
    }
}

/// Sets the policy to 1 (services only) unless it is configured already. Returns whether we
/// changed anything (so uninstall knows whether to remove it).
pub fn enable_if_unset() -> Result<bool> {
    if current()?.is_some() {
        tracing::info!("{VALUE} is configured already, leaving it alone");
        return Ok(false);
    }
    let out = reg(&["add", KEY, "/v", VALUE, "/t", "REG_DWORD", "/d", "1", "/f"])?;
    if !out.status.success() {
        bail!("reg add failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(true)
}

/// Removes the value we created.
pub fn remove() -> Result<()> {
    let out = reg(&["delete", KEY, "/v", VALUE, "/f"])?;
    if !out.status.success() && current()?.is_some() {
        bail!("reg delete failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reg_output() {
        let t = "\r\nHKEY_LOCAL_MACHINE (key)\r\n    SoftwareSASGeneration    REG_DWORD    0x1\r\n\r\n";
        assert_eq!(parse_query(t), Some(1));
        assert_eq!(parse_query("    SoftwareSASGeneration    REG_DWORD    0x3"), Some(3));
        assert_eq!(parse_query("nothing"), None);
    }
}
