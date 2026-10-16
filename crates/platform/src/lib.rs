//! OS abstraction for Desk Bridge.
//!
//! All OS access goes through the traits in this crate. `input`, `audio`, `clipboard`, `video`
//! and `core` never touch Windows types. [`mock`] provides in-memory implementations so the
//! whole stack is testable without hardware.

pub mod audio;
pub mod clipboard;
pub mod display;
pub mod error;
pub mod hid;
pub mod hotkey;
pub mod input;
pub mod mock;
pub mod power;
pub mod screen;
pub mod secret;
pub mod video;

pub use error::{PlatformError, Result};

pub use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
