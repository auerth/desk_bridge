//! Restore of the lid-close action that the agent's power module changed (plan 8.7).
//!
//! The agent writes `%LOCALAPPDATA%\softwelop\DeskBridge\lid-backup.json`:
//! `{"scheme":"<guid>","ac":<u32>,"dc":<u32>}` before it sets "do nothing" for AC power.
//! On uninstall we put the values back with `powercfg`.

use anyhow::{Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LidBackup {
    #[serde(default)]
    pub scheme: String,
    pub ac: u32,
    pub dc: u32,
}

/// Lid action values: 0 nothing, 1 sleep, 2 hibernate, 3 shut down.
const MAX_LID_ACTION: u32 = 3;

impl LidBackup {
    pub fn parse(json: &str) -> Result<Self> {
        let b: Self = serde_json::from_str(json)?;
        b.validate()?;
        Ok(b)
    }

    pub fn validate(&self) -> Result<()> {
        if self.ac > MAX_LID_ACTION || self.dc > MAX_LID_ACTION {
            bail!("lid action out of range (ac={}, dc={})", self.ac, self.dc);
        }
        if !self.scheme.is_empty() && !is_guid(&self.scheme) {
            bail!("invalid power scheme guid {:?}", self.scheme);
        }
        Ok(())
    }

    /// Scheme argument for powercfg (`SCHEME_CURRENT` when the backup has none).
    #[must_use]
    pub fn scheme_arg(&self) -> &str {
        if self.scheme.is_empty() {
            "SCHEME_CURRENT"
        } else {
            &self.scheme
        }
    }

    /// The `powercfg` argument lists that restore the saved values.
    #[must_use]
    pub fn powercfg_commands(&self) -> Vec<Vec<String>> {
        let s = self.scheme_arg().to_owned();
        let mk = |verb: &str, v: u32| {
            vec![
                format!("/{verb}"),
                s.clone(),
                "SUB_BUTTONS".to_owned(),
                "LIDACTION".to_owned(),
                v.to_string(),
            ]
        };
        vec![
            mk("setacvalueindex", self.ac),
            mk("setdcvalueindex", self.dc),
        ]
    }
}

/// `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` (no braces).
#[must_use]
pub fn is_guid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    let lens = [8, 4, 4, 4, 12];
    parts.len() == 5
        && parts
            .iter()
            .zip(lens)
            .all(|(p, l)| p.len() == l && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Extract the GUID from `powercfg /getactivescheme` output
/// (`Power Scheme GUID: 381b4222-...  (Balanced)`; localised text, so we look for the GUID shape).
#[must_use]
pub fn parse_active_scheme(output: &str) -> Option<String> {
    output
        .split(|c: char| c.is_whitespace() || c == '(' || c == ')')
        .find(|tok| is_guid(tok))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUID: &str = "381b4222-f694-41f0-9685-ff5bb260df2e";

    #[test]
    fn parses_backup() {
        let b = LidBackup::parse(&format!(r#"{{"scheme":"{GUID}","ac":1,"dc":2}}"#)).unwrap();
        assert_eq!(
            b,
            LidBackup {
                scheme: GUID.into(),
                ac: 1,
                dc: 2
            }
        );
    }

    #[test]
    fn rejects_bad_values() {
        assert!(LidBackup::parse(r#"{"scheme":"x; calc","ac":1,"dc":1}"#).is_err());
        assert!(LidBackup::parse(r#"{"scheme":"","ac":9,"dc":1}"#).is_err());
        assert!(LidBackup::parse("not json").is_err());
    }

    #[test]
    fn builds_powercfg_commands() {
        let b = LidBackup {
            scheme: GUID.into(),
            ac: 1,
            dc: 2,
        };
        let cmds = b.powercfg_commands();
        assert_eq!(
            cmds[0],
            ["/setacvalueindex", GUID, "SUB_BUTTONS", "LIDACTION", "1"]
        );
        assert_eq!(
            cmds[1],
            ["/setdcvalueindex", GUID, "SUB_BUTTONS", "LIDACTION", "2"]
        );
        let none = LidBackup {
            scheme: String::new(),
            ac: 0,
            dc: 0,
        };
        assert_eq!(none.scheme_arg(), "SCHEME_CURRENT");
    }

    #[test]
    fn active_scheme_parsing() {
        let out = format!("Power Scheme GUID: {GUID}  (Balanced)");
        assert_eq!(parse_active_scheme(&out).as_deref(), Some(GUID));
        assert_eq!(
            parse_active_scheme(
                "Energieschema-GUID: 381b4222-f694-41f0-9685-ff5bb260df2e  (Ausbalanciert)"
            )
            .as_deref(),
            Some(GUID)
        );
        assert_eq!(parse_active_scheme("nothing"), None);
    }
}
