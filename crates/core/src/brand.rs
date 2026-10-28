//! The one place that knows the product name (the TypeScript counterpart is `app/src/brand.ts`).

pub const PRODUCT_NAME: &str = "Desk Bridge";
pub const CODENAME: &str = "deskbridge";
pub const COMPANY: &str = "softwelop";
pub const COMPANY_URL: &str = "https://softwelop.com";
/// Named pipe between UI and agent.
pub const UI_PIPE_NAME: &str = r"\\.\pipe\deskbridge-ui";
/// Mutex / single-instance identifiers.
pub const AGENT_INSTANCE_ID: &str = "Global\\softwelop-deskbridge-agent";
/// Single instance of the agent at the logon screen (ADR 0008). Different from
/// [`AGENT_INSTANCE_ID`]: the user's agent must never think it is already running.
pub const LOGON_AGENT_INSTANCE_ID: &str = "Global\\softwelop-deskbridge-logon-agent";
/// Command line switch the service starts the logon-screen agent with.
pub const LOGON_SCREEN_ARG: &str = "--logon-screen";
/// Scheduled task that starts the elevated agent at logon.
pub const AGENT_TASK_NAME: &str = "DeskBridge Agent";
pub const FIREWALL_RULE_NAME: &str = "Desk Bridge (UDP 47820 + mDNS)";
/// Build version: package version plus git commit (`0.1.8-1a2b3c4-beta`, see `build.rs`). Shown
/// in the UI, sent to the partner and compared with its build.
pub const VERSION: &str = env!("DESKBRIDGE_BUILD_VERSION");
/// Version of the installer package (`0.1.8-beta`): names `DeskBridge-Setup-<ver>.exe`.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Newest published release (GitHub excludes drafts and pre-releases here).
pub const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/auerth/desk_bridge/releases/latest";
/// Release page shown when no release-specific link is known.
pub const RELEASES_URL: &str = "https://github.com/auerth/desk_bridge/releases";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_version_keeps_number_and_pre_release_of_the_package() {
        let (core, pre) = PACKAGE_VERSION
            .split_once('-')
            .map_or((PACKAGE_VERSION, None), |(c, p)| (c, Some(p)));
        assert!(VERSION.starts_with(core), "{VERSION}");
        if let Some(pre) = pre {
            assert!(VERSION.ends_with(&format!("-{pre}")), "{VERSION}");
        }
    }
}
