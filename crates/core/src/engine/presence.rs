//! "Keep Teams online": while the setting is on and Teams runs on this device, the idle timer is
//! reset before Teams would show the user as away. The user works on the partner device, so
//! nothing is typed here and colleagues would see them as away all day.

use super::Engine;
use std::time::Duration;

/// Teams shows "away" after about five minutes without input; stay well below that.
const NUDGE_AFTER_IDLE: Duration = Duration::from_secs(90);
/// How often Teams and the idle time are checked (in 250 ms ticks); the first check is right
/// after start.
const CHECK_EVERY_TICKS: u64 = 20;

impl Engine {
    pub(crate) fn tick_presence(&mut self) {
        // The logon screen has no Teams and no user to keep available.
        if self.logon_screen || self.tick_count % CHECK_EVERY_TICKS != 1 {
            return;
        }
        let running = self.platform.presence.teams_running();
        if running != self.teams_running {
            tracing::debug!(running, "Teams process state changed");
            self.teams_running = running;
            self.dirty = true;
        }
        if !running || !self.cfg.general.keep_teams_online {
            return;
        }
        if self
            .platform
            .presence
            .idle_for()
            .is_some_and(|idle| idle >= NUDGE_AFTER_IDLE)
            && let Err(e) = self.platform.presence.nudge()
        {
            tracing::debug!("could not keep Teams online: {e}");
        }
    }
}
