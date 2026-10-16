//! Platform independent input logic.
//!
//! * [`layout`]: shared screen layout, edge adjacency and relative-position mapping
//! * [`router`]: source side state machine (local ⇄ remote, edge detection, hotkeys)
//! * [`receiver`]: target side (injection, return-edge detection, `release_all` safety)
//! * [`hotkeys`]: default hotkey set and actions

pub mod hotkeys;
pub mod layout;
pub mod receiver;
pub mod router;

pub use layout::{Hit, LScreen, Layout};
