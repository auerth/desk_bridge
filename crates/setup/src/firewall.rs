//! Inbound firewall rule (plan 7.5): UDP 47820 (QUIC) + UDP 5353 (mDNS), **Private** profile
//! (plus **Domain** with `include_domain`, which the installer always passes), never Public,
//! scoped to `deskbridge-agent.exe` and the local subnet.
//!
//! Primary path is the `INetFwPolicy2` COM API; `netsh advfirewall` is the fallback.

use std::path::Path;

use crate::brand::{FIREWALL_RULE_NAME, MDNS_PORT, QUIC_PORT};

/// Remote scope of the rule: LAN only (the product has no internet mode).
pub const REMOTE_SCOPE: &str = "LocalSubnet";

#[must_use]
pub fn local_ports() -> String {
    format!("{QUIC_PORT},{MDNS_PORT}")
}

/// Arguments for `netsh` that delete the rule (all rules with that name).
#[must_use]
pub fn netsh_delete_args() -> Vec<String> {
    ["advfirewall", "firewall", "delete", "rule"]
        .iter()
        .map(|s| (*s).to_owned())
        .chain([format!("name={FIREWALL_RULE_NAME}")])
        .collect()
}

/// Arguments for `netsh` that add the rule. Passed as separate argv entries (no shell), so the
/// program path may contain spaces.
#[must_use]
pub fn netsh_add_args(agent_exe: &Path, include_domain: bool) -> Vec<String> {
    let mut v: Vec<String> = ["advfirewall", "firewall", "add", "rule"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    v.extend([
        format!("name={FIREWALL_RULE_NAME}"),
        "dir=in".into(),
        "action=allow".into(),
        "protocol=UDP".into(),
        format!("localport={}", local_ports()),
        (if include_domain {
            "profile=private,domain"
        } else {
            "profile=private"
        })
        .into(),
        format!("remoteip={REMOTE_SCOPE}"),
        format!("program={}", agent_exe.display()),
        "enable=yes".into(),
    ]);
    v
}

#[cfg(windows)]
pub use sys::{add_rule, is_present, remove_rule};

#[cfg(windows)]
mod sys {
    use std::path::Path;
    use std::process::Command;

    use anyhow::{Context, Result, bail};
    use windows::Win32::Foundation::VARIANT_TRUE;
    use windows::Win32::NetworkManagement::WindowsFirewall::{
        INetFwPolicy2, INetFwRule3, NET_FW_ACTION_ALLOW, NET_FW_IP_PROTOCOL_UDP,
        NET_FW_PROFILE2_DOMAIN, NET_FW_PROFILE2_PRIVATE, NET_FW_RULE_DIR_IN, NetFwPolicy2,
        NetFwRule,
    };
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::core::BSTR;

    use super::{REMOTE_SCOPE, local_ports, netsh_add_args, netsh_delete_args};
    use crate::brand::{FIREWALL_RULE_NAME, PRODUCT_NAME};

    fn policy() -> Result<INetFwPolicy2> {
        // SAFETY: COM is initialised by `main` on this thread.
        unsafe { CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER) }
            .context("CoCreateInstance(NetFwPolicy2)")
    }

    /// Read-only: is there a rule with our name?
    pub fn is_present() -> Result<bool> {
        let rules = unsafe { policy()?.Rules() }.context("INetFwPolicy2::Rules")?;
        Ok(unsafe { rules.Item(&BSTR::from(FIREWALL_RULE_NAME)) }.is_ok())
    }

    /// Create (or replace) the rule. Falls back to `netsh` if COM fails.
    pub fn add_rule(agent_exe: &Path, include_domain: bool) -> Result<()> {
        match add_rule_com(agent_exe, include_domain) {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!("firewall via COM failed ({e:#}); falling back to netsh");
                add_rule_netsh(agent_exe, include_domain)
            }
        }
    }

    fn add_rule_com(agent_exe: &Path, include_domain: bool) -> Result<()> {
        let policy = policy()?;
        let rules = unsafe { policy.Rules() }?;
        // Idempotent: drop every rule with our name first.
        unsafe { rules.Remove(&BSTR::from(FIREWALL_RULE_NAME)) }.ok();

        let rule: INetFwRule3 = unsafe { CoCreateInstance(&NetFwRule, None, CLSCTX_INPROC_SERVER) }
            .context("CoCreateInstance(NetFwRule)")?;
        unsafe {
            rule.SetName(&BSTR::from(FIREWALL_RULE_NAME))?;
            rule.SetDescription(&BSTR::from(format!(
                "{PRODUCT_NAME}: peer protocol (QUIC) and mDNS discovery on the local network."
            )))?;
            rule.SetGrouping(&BSTR::from(PRODUCT_NAME))?;
            rule.SetApplicationName(&BSTR::from(agent_exe.display().to_string()))?;
            rule.SetProtocol(NET_FW_IP_PROTOCOL_UDP.0)?;
            rule.SetLocalPorts(&BSTR::from(local_ports()))?;
            rule.SetRemoteAddresses(&BSTR::from(REMOTE_SCOPE))?;
            rule.SetDirection(NET_FW_RULE_DIR_IN)?;
            rule.SetAction(NET_FW_ACTION_ALLOW)?;
            let mut profiles = NET_FW_PROFILE2_PRIVATE.0;
            if include_domain {
                profiles |= NET_FW_PROFILE2_DOMAIN.0;
            }
            rule.SetProfiles(profiles)?;
            rule.SetEnabled(VARIANT_TRUE)?;
            rules.Add(&rule).context("INetFwRules::Add")?;
        }
        Ok(())
    }

    fn run_netsh(args: &[String]) -> Result<()> {
        let out = Command::new("netsh")
            .args(args)
            .output()
            .context("starting netsh")?;
        if !out.status.success() {
            bail!(
                "netsh {} failed ({}): {}",
                args.join(" "),
                out.status,
                String::from_utf8_lossy(&out.stdout).trim()
            );
        }
        Ok(())
    }

    fn add_rule_netsh(agent_exe: &Path, include_domain: bool) -> Result<()> {
        // "No rules match" on delete is fine.
        let _ = Command::new("netsh").args(netsh_delete_args()).output();
        run_netsh(&netsh_add_args(agent_exe, include_domain))
    }

    /// Remove every rule with our name. Missing rule is not an error.
    pub fn remove_rule() -> Result<()> {
        if let Ok(p) = policy()
            && let Ok(rules) = unsafe { p.Rules() }
            && unsafe { rules.Remove(&BSTR::from(FIREWALL_RULE_NAME)) }.is_ok()
        {
            return Ok(());
        }
        // COM unavailable or failed: netsh reports non-zero when nothing matched, so only
        // surface an error if the rule is still present afterwards.
        let _ = Command::new("netsh").args(netsh_delete_args()).output();
        match is_present() {
            Ok(true) => bail!("firewall rule still present after removal attempt"),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn netsh_add_is_private_udp_program_scoped() {
        let exe = PathBuf::from(r"C:\Program Files\softwelop\Desk Bridge\deskbridge-agent.exe");
        let args = netsh_add_args(&exe, false);
        assert!(args.contains(&"profile=private".to_owned()));
        assert!(args.contains(&"protocol=UDP".to_owned()));
        assert!(args.contains(&"dir=in".to_owned()));
        assert!(args.contains(&"localport=47820,5353".to_owned()));
        assert!(args.contains(&"remoteip=LocalSubnet".to_owned()));
        assert!(
            args.iter()
                .any(|a| a.starts_with("program=") && a.contains("Program Files"))
        );
        assert!(args.contains(&format!("name={FIREWALL_RULE_NAME}")));
        assert!(
            !args
                .iter()
                .any(|a| a.contains("profile=public") || a.contains("profile=any"))
        );
    }

    #[test]
    fn netsh_add_can_include_domain() {
        let args = netsh_add_args(Path::new(r"C:\x\a.exe"), true);
        assert!(args.contains(&"profile=private,domain".to_owned()));
    }

    #[test]
    fn netsh_delete_targets_rule_name() {
        let args = netsh_delete_args();
        assert_eq!(&args[..4], ["advfirewall", "firewall", "delete", "rule"]);
        assert_eq!(args[4], format!("name={FIREWALL_RULE_NAME}"));
    }
}
