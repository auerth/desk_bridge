use crate::Result;
use std::time::Duration;

/// Keeps the user shown as "available" in chat apps (Teams) on a machine nobody is typing on.
///
/// Teams switches to "away" after about five minutes without keyboard or mouse input on the
/// machine it runs on. A user who works on the other PC of the desk leaves exactly that
/// machine idle, so colleagues see them as away.
pub trait Presence: Send + Sync {
    /// Is a Microsoft Teams process running in the current session?
    fn teams_running(&self) -> bool;
    /// Time since the last real or injected keyboard/mouse input on this machine.
    fn idle_for(&self) -> Option<Duration>;
    /// Registers harmless input (an unmapped key press) so the idle timer starts over.
    fn nudge(&self) -> Result<()>;
}

/// No presence support (logon screen, tests that do not care).
#[derive(Debug, Default, Clone, Copy)]
pub struct NullPresence;

impl Presence for NullPresence {
    fn teams_running(&self) -> bool {
        false
    }

    fn idle_for(&self) -> Option<Duration> {
        None
    }

    fn nudge(&self) -> Result<()> {
        Ok(())
    }
}
