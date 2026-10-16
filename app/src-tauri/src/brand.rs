//! Product constants of the UI process.
//!
//! This is a deliberate copy of the few values from `crates/core/src/brand.rs` that the UI needs; the
//! UI must not depend on `deskbridge-core` (that would pull the whole agent stack into the Tauri build).
//! Keep in sync with `crates/core/src/brand.rs` and `app/src/brand.ts`.

pub const PRODUCT_NAME: &str = "Desk Bridge";
/// Named pipe between UI and agent (`deskbridge_core::brand::UI_PIPE_NAME`).
pub const UI_PIPE_NAME: &str = r"\\.\pipe\deskbridge-ui";
/// File name of the agent executable, expected next to the UI executable.
pub const AGENT_EXE: &str = "deskbridge-agent.exe";
/// Scheduled task that starts the elevated agent at logon (`crates/core/src/brand.rs`).
pub const AGENT_TASK_NAME: &str = "DeskBridge Agent";
/// Directory names below `%LOCALAPPDATA%` (plan section 10).
pub const COMPANY_DIR: &str = "softwelop";
pub const APP_DIR: &str = "DeskBridge";

#[cfg(test)]
mod tests {
    use super::*;

    /// The UI must not depend on `deskbridge-core`, so the constants are copied. This test keeps
    /// the copies honest (a wrong pipe name once made the UI report "agent not reachable").
    #[test]
    fn constants_match_the_core_brand() {
        let core = include_str!("../../../crates/core/src/brand.rs");
        let want = |name: &str, value: &str| {
            assert!(
                core.contains(&format!("pub const {name}: &str = r\"{value}\";"))
                    || core.contains(&format!("pub const {name}: &str = \"{value}\";")),
                "{name} differs from crates/core/src/brand.rs"
            );
        };
        want("UI_PIPE_NAME", UI_PIPE_NAME);
        want("AGENT_TASK_NAME", AGENT_TASK_NAME);
        assert!(UI_PIPE_NAME.starts_with(r"\\.\pipe\"));
    }
}
