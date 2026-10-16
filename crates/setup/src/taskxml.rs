//! Task Scheduler XML for the elevated agent autostart (plan 12.2 / 8.1).
//!
//! Pure string generation so it can be unit-tested without touching the Task Scheduler.

use std::path::Path;

use crate::brand::{AGENT_TASK_NAME, COMPANY, PRODUCT_NAME};

/// Well-known SID of the local `BUILTIN\Users` group.
pub const USERS_GROUP_SID: &str = "S-1-5-32-545";

/// Who the logon trigger and the principal apply to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskPrincipal {
    /// Any interactive user (group principal `BUILTIN\Users`, run level highest available:
    /// administrators get an elevated agent, standard users a limited one).
    AnyUser,
    /// One specific account, `DOMAIN\user`.
    User(String),
}

/// Escape text for XML element content.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Build the task definition for `agent_exe`.
///
/// * logon trigger (no delay; the agent waits for the network itself)
/// * `RunLevel=HighestAvailable` so no UAC prompt appears at logon
/// * no execution time limit, not stopped on battery, restart on failure (1 min, up to 999x)
/// * single instance (`IgnoreNew`)
#[must_use]
pub fn agent_task_xml(agent_exe: &Path, working_dir: &Path, principal: &TaskPrincipal) -> String {
    let (trigger_user, principal_xml) = match principal {
        TaskPrincipal::AnyUser => (
            String::new(),
            format!(
                "<GroupId>{USERS_GROUP_SID}</GroupId>\n      <RunLevel>HighestAvailable</RunLevel>"
            ),
        ),
        TaskPrincipal::User(user) => (
            format!("\n      <UserId>{}</UserId>", esc(user)),
            format!(
                "<UserId>{}</UserId>\n      <LogonType>InteractiveToken</LogonType>\n      <RunLevel>HighestAvailable</RunLevel>",
                esc(user)
            ),
        ),
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Author>{company}</Author>
    <Description>Starts the {product} agent (mouse/keyboard sharing, audio, clipboard) elevated at logon.</Description>
    <URI>\{task}</URI>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>{trigger_user}
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      {principal_xml}
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>999</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <WorkingDirectory>{wd}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#,
        company = esc(COMPANY),
        product = esc(PRODUCT_NAME),
        task = esc(AGENT_TASK_NAME),
        exe = esc(&agent_exe.display().to_string()),
        wd = esc(&working_dir.display().to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn exe() -> PathBuf {
        PathBuf::from(r"C:\Program Files\softwelop\Desk Bridge\deskbridge-agent.exe")
    }

    #[test]
    fn any_user_xml_has_required_settings() {
        let xml = agent_task_xml(&exe(), exe().parent().unwrap(), &TaskPrincipal::AnyUser);
        assert!(xml.contains("<LogonTrigger>"));
        assert!(
            !xml.contains("<UserId>"),
            "any-user trigger must not name a user"
        );
        assert!(xml.contains("<GroupId>S-1-5-32-545</GroupId>"));
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(xml.contains("<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>"));
        assert!(xml.contains("<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>"));
        assert!(xml.contains("<RestartOnFailure>"));
        assert!(xml.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"));
        assert!(xml.contains(
            r"<Command>C:\Program Files\softwelop\Desk Bridge\deskbridge-agent.exe</Command>"
        ));
        assert!(xml.contains(r"<URI>\DeskBridge Agent</URI>"));
    }

    #[test]
    fn user_xml_names_user_in_trigger_and_principal() {
        let xml = agent_task_xml(
            &exe(),
            exe().parent().unwrap(),
            &TaskPrincipal::User(r"PC\Tho&rben".into()),
        );
        assert_eq!(xml.matches(r"<UserId>PC\Tho&amp;rben</UserId>").count(), 2);
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(!xml.contains("<GroupId>"));
    }

    #[test]
    fn paths_are_escaped() {
        let p = PathBuf::from(r"C:\a&b\<x>\deskbridge-agent.exe");
        let xml = agent_task_xml(&p, p.parent().unwrap(), &TaskPrincipal::AnyUser);
        assert!(xml.contains(r"C:\a&amp;b\&lt;x&gt;\deskbridge-agent.exe"));
    }

    #[test]
    fn xml_is_well_formed_enough() {
        let xml = agent_task_xml(&exe(), exe().parent().unwrap(), &TaskPrincipal::AnyUser);
        // Every opening tag we emit has a closing tag (cheap structural sanity check).
        for tag in [
            "Triggers",
            "Principals",
            "Settings",
            "Actions",
            "Exec",
            "RegistrationInfo",
        ] {
            assert_eq!(
                xml.matches(&format!("<{tag}>")).count() + xml.matches(&format!("<{tag} ")).count(),
                xml.matches(&format!("</{tag}>")).count(),
                "{tag}"
            );
        }
        assert!(xml.trim_end().ends_with("</Task>"));
    }
}
