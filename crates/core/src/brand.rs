//! The one place that knows the product name (the TypeScript counterpart is `app/src/brand.ts`).

pub const PRODUCT_NAME: &str = "Desk Bridge";
pub const CODENAME: &str = "deskbridge";
pub const COMPANY: &str = "softwelop";
pub const COMPANY_URL: &str = "https://softwelop.com";
/// Named pipe between UI and agent.
pub const UI_PIPE_NAME: &str = r"\\.\pipe\deskbridge-ui";
/// Mutex / single-instance identifiers.
pub const AGENT_INSTANCE_ID: &str = "Global\\softwelop-deskbridge-agent";
/// Scheduled task that starts the elevated agent at logon.
pub const AGENT_TASK_NAME: &str = "DeskBridge Agent";
pub const FIREWALL_RULE_NAME: &str = "Desk Bridge (UDP 47820 + mDNS)";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Newest published release (GitHub excludes drafts and pre-releases here).
pub const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/auerth/desk_bridge/releases/latest";
/// Release page shown when no release-specific link is known.
pub const RELEASES_URL: &str = "https://github.com/auerth/desk_bridge/releases";
