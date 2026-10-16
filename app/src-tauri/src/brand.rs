//! Product constants of the UI process.
//!
//! This is a deliberate copy of the few values from `crates/core/src/brand.rs` that the UI needs; the
//! UI must not depend on `deskbridge-core` (that would pull the whole agent stack into the Tauri build).
//! Keep in sync with `crates/core/src/brand.rs` and `app/src/brand.ts`.

pub const PRODUCT_NAME: &str = "Desk Bridge";
/// Named pipe between UI and agent (`deskbridge_core::brand::UI_PIPE_NAME`).
pub const UI_PIPE_NAME: &str = r"\.\pipe\deskbridge-ui";
/// File name of the agent executable, expected next to the UI executable.
pub const AGENT_EXE: &str = "deskbridge-agent.exe";
/// Directory names below `%LOCALAPPDATA%` (plan section 10).
pub const COMPANY_DIR: &str = "softwelop";
pub const APP_DIR: &str = "DeskBridge";
