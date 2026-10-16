//! Names shared with the rest of the product.
//!
//! The canonical copies live in `crates/core/src/brand.rs`. This crate deliberately does not
//! depend on `deskbridge-core` (it would drag the whole audio/video/net stack into a tiny
//! installer helper); the unit test `matches_core_brand` keeps the two in sync.

pub const PRODUCT_NAME: &str = "Desk Bridge";
pub const COMPANY: &str = "softwelop";
/// Scheduled task that starts the elevated agent at logon.
pub const AGENT_TASK_NAME: &str = "DeskBridge Agent";
pub const FIREWALL_RULE_NAME: &str = "Desk Bridge (UDP 47820 + mDNS)";

/// Peer protocol port (UDP, QUIC).
pub const QUIC_PORT: u16 = 47820;
/// mDNS / DNS-SD port (UDP).
pub const MDNS_PORT: u16 = 5353;

pub const AGENT_EXE: &str = "deskbridge-agent.exe";
pub const SERVICE_EXE: &str = "deskbridge-service.exe";
pub const SETUP_EXE: &str = "deskbridge-setup.exe";

/// Windows service name / display name (service is built in M6).
pub const SERVICE_NAME: &str = "DeskBridgeService";
pub const SERVICE_DISPLAY_NAME: &str = "Desk Bridge Service";

/// File name of the SendTo shortcut (without the `.lnk` extension).
pub const SENDTO_SHORTCUT_NAME: &str = "Desk Bridge \u{2013} Laptop senden";

/// Written by `install`, read by `uninstall` (lives in the install dir).
pub const MANIFEST_FILE: &str = "installed.json";
/// Written by the power module of the agent before it changes the lid action.
pub const LID_BACKUP_FILE: &str = "lid-backup.json";
/// Optional catalogue of driver packages shipped with the installer
/// (`<install dir>\drivers\drivers.json`).
pub const DRIVER_CATALOGUE: &str = "drivers/drivers.json";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_core_brand() {
        let core = include_str!("../../core/src/brand.rs");
        for (name, value) in [
            ("AGENT_TASK_NAME", AGENT_TASK_NAME),
            ("FIREWALL_RULE_NAME", FIREWALL_RULE_NAME),
            ("PRODUCT_NAME", PRODUCT_NAME),
            ("COMPANY", COMPANY),
        ] {
            let line = core
                .lines()
                .find(|l| l.trim_start().starts_with(&format!("pub const {name}:")))
                .unwrap_or_else(|| panic!("{name} missing in core brand.rs"));
            assert!(
                line.contains(&format!("\"{value}\"")),
                "{name} drifted from core: {line}"
            );
        }
    }

    #[test]
    fn firewall_rule_mentions_ports() {
        assert!(FIREWALL_RULE_NAME.contains(&QUIC_PORT.to_string()));
    }
}
