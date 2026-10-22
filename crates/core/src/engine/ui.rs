//! Handlers for UI requests.

use super::Engine;
use crate::ipc::{Feature, UiRequest};
use deskbridge_input::hotkeys::HotkeyAction;
use deskbridge_platform::input::CaptureEvent;
use deskbridge_proto::pb;
use serde_json::{Value, json};
use std::time::Instant;
use uuid::Uuid;

type Reply = Result<Value, String>;

fn ok() -> Reply {
    Ok(json!({ "ok": true }))
}

impl Engine {
    pub(crate) fn handle_ui(&mut self, req: UiRequest) -> Reply {
        match req {
            UiRequest::GetState => {
                serde_json::to_value(self.build_state()).map_err(|e| e.to_string())
            }
            UiRequest::GetConfig => serde_json::to_value(&self.cfg).map_err(|e| e.to_string()),
            UiRequest::SetConfig { config } => self.set_config(*config),
            UiRequest::CompleteOnboarding { name, is_pc } => {
                let name = name.trim();
                if !name.is_empty() {
                    self.cfg.device.name = name.to_string();
                }
                self.cfg.device.input_source = is_pc;
                self.cfg.device.headset_sink = is_pc;
                self.cfg.device.stream_source = !is_pc;
                self.cfg.device.onboarding_done = true;
                self.after_device_change();
                ok()
            }
            UiRequest::StartPairing => self.start_pairing(),
            UiRequest::CancelPairing => {
                self.cancel_pairing();
                ok()
            }
            UiRequest::PairWith {
                device_id,
                address,
                code,
            } => self.pair_with(device_id, address, code),
            UiRequest::Unpair { device_id } => {
                let id = Uuid::parse_str(&device_id).map_err(|e| e.to_string())?;
                if let Some(s) = &self.session
                    && s.link.peer.id == id
                {
                    s.link.send_control(pb::control::Msg::Goodbye(pb::Goodbye {
                        reason: "unpaired".into(),
                    }));
                }
                let connected_to = self.session.as_ref().map(|s| s.link.peer.id);
                self.peers.remove(id).map_err(|e| e.to_string())?;
                if connected_to == Some(id) {
                    self.close_session("unpaired");
                }
                self.had_session = false;
                ok()
            }
            UiRequest::RenamePeer { device_id, name } => {
                let id = Uuid::parse_str(&device_id).map_err(|e| e.to_string())?;
                self.peers.rename(id, name).map_err(|e| e.to_string())?;
                ok()
            }
            UiRequest::AddManualHost { host } => {
                let h = host.trim().to_string();
                if h.is_empty() {
                    return Err("Leere Adresse".into());
                }
                if !self.cfg.network.manual_hosts.contains(&h) {
                    self.cfg.network.manual_hosts.push(h);
                    self.persist_config();
                }
                self.next_attempt = Instant::now();
                ok()
            }
            UiRequest::SetFeature { feature, enabled } => self.set_feature(feature, enabled),
            UiRequest::SetLayout { placements } => {
                self.set_layout_from_ui(placements);
                ok()
            }
            UiRequest::SetFocus { mode } => self.set_focus(&mode),
            UiRequest::JumpToOtherDevice => {
                self.on_capture(CaptureEvent::Hotkey(HotkeyAction::JumpOther.id()));
                ok()
            }
            UiRequest::SetCursorLock { locked } => {
                if self.router.is_locked() != locked {
                    self.on_capture(CaptureEvent::Hotkey(HotkeyAction::LockCursor.id()));
                }
                ok()
            }
            UiRequest::SetPaused { paused } => {
                self.cfg.general.paused = paused;
                self.persist_config();
                if paused {
                    self.close_session("paused");
                } else {
                    self.next_attempt = Instant::now();
                }
                ok()
            }
            UiRequest::ShutdownAgent => {
                self.shutdown.notify_waiters();
                ok()
            }
            UiRequest::RestartAgent { elevated } => {
                tracing::info!(elevated, "agent restart requested");
                self.restart_elevated
                    .store(elevated, std::sync::atomic::Ordering::SeqCst);
                self.restart
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                self.shutdown.notify_waiters();
                ok()
            }
            UiRequest::Ping => Ok(json!({ "pong": true })),
            other => self.handle_ui_feature(other),
        }
    }

    fn set_config(&mut self, mut new: crate::config::Config) -> Reply {
        new.sanitize();
        let name_changed = new.device.name != self.cfg.device.name
            || new.device.input_source != self.cfg.device.input_source
            || new.device.headset_sink != self.cfg.device.headset_sink
            || new.device.stream_source != self.cfg.device.stream_source;
        let hotkeys_changed = new.hotkeys != self.cfg.hotkeys;
        self.cfg = new;
        if self.cfg.device.name.trim().is_empty() {
            self.cfg.device.name = super::default_device_name();
        }
        self.router.set_config(super::router_config(&self.cfg));
        if hotkeys_changed && self.capture_running {
            let (hk, _) = deskbridge_input::hotkeys::build(&self.cfg.hotkeys);
            self.platform.capture.set_hotkeys(hk);
        }
        if name_changed {
            self.after_device_change();
        } else {
            self.persist_config();
        }
        self.apply_config_to_features();
        serde_json::to_value(&self.cfg).map_err(|e| e.to_string())
    }

    fn after_device_change(&mut self) {
        self.persist_config();
        self.refresh_hello();
        self.announce_self();
    }

    fn set_feature(&mut self, feature: Feature, enabled: bool) -> Reply {
        match feature {
            Feature::Input => {
                self.cfg.input.enabled = enabled;
                if enabled && self.session.is_some() && self.cfg.device.input_source {
                    self.start_capture();
                } else if !enabled {
                    let a = self.router.force_local();
                    self.exec_actions(a, true);
                    self.stop_capture();
                }
            }
            Feature::Clipboard => {
                self.cfg.clipboard.send = enabled;
                self.cfg.clipboard.receive = enabled;
            }
            Feature::LaptopAudio => self.cfg.audio.laptop_to_pc.enabled = enabled,
            Feature::Mic => self.cfg.audio.mic_to_laptop.enabled = enabled,
            Feature::MuteLaptopAudio => self.cfg.audio.laptop_to_pc.mute = enabled,
            Feature::MuteMic => self.cfg.audio.mic_to_laptop.mute = enabled,
            Feature::Focus => return self.set_focus(if enabled { "full" } else { "off" }),
        }
        self.persist_config();
        self.apply_config_to_features();
        ok()
    }
}
