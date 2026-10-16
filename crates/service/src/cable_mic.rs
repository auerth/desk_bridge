//! Owns the on/off state of the virtual cable microphone ("CABLE Output", ADR 0003) on behalf of
//! the agent.
//!
//! The agent asks for it to be enabled while the headset microphone is switched on and disabled
//! otherwise; a disabled endpoint vanishes from Teams & co., which fall back to the laptop's own
//! microphone. The service runs as LocalSystem, so the switch works for standard users too, and it
//! also covers the time the agent is not running (crashed, quit, before logon): then the endpoint
//! is disabled. All of this only once the agent asked for management (`managed`), persisted in
//! `%ProgramData%\softwelop\DeskBridge\cable-mic.json`; a user who never opted in keeps the
//! cable exactly as it is. Stopping the service (update, uninstall) leaves the endpoint enabled.

use deskbridge_platform_windows::audio::set_cable_mic_enabled;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;

pub struct CableMic {
    managed: Mutex<bool>,
    file: PathBuf,
}

impl CableMic {
    /// Loads the persisted `managed` flag.
    #[must_use]
    pub fn load() -> Arc<Self> {
        let file = crate::winutil::program_data_dir().join("cable-mic.json");
        let managed = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v["managed"].as_bool())
            .unwrap_or(false);
        Arc::new(Self {
            managed: Mutex::new(managed),
            file,
        })
    }

    /// Request from the agent.
    pub fn request(&self, enabled: bool, managed: bool) -> Result<(), String> {
        {
            let mut m = self.managed.lock();
            if *m != managed {
                *m = managed;
                if let Some(dir) = self.file.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(
                    &self.file,
                    serde_json::json!({ "managed": managed }).to_string(),
                );
            }
        }
        // Unmanaged means "hand it back": always enabled.
        let enabled = enabled || !managed;
        set_cable_mic_enabled(enabled).map_err(|e| e.to_string())?;
        tracing::info!(
            enabled,
            managed,
            "cable microphone set on request of the agent"
        );
        Ok(())
    }

    /// No agent runs (any more): disable the endpoint if Desk Bridge manages it.
    pub fn on_agent_down(&self) {
        if !*self.managed.lock() {
            return;
        }
        match set_cable_mic_enabled(false) {
            Ok(()) => tracing::info!("no agent running: cable microphone disabled"),
            // Not installed is the normal case on most machines.
            Err(e) => tracing::debug!("cable microphone not disabled: {e}"),
        }
    }

    /// Service stops (update, uninstall): leave the system as found.
    pub fn on_service_stop(&self) {
        if *self.managed.lock()
            && let Err(e) = set_cable_mic_enabled(true)
        {
            tracing::debug!("cable microphone not re-enabled: {e}");
        }
    }
}
