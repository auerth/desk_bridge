//! Hook points for the feature modules (clipboard, files, audio, focus). The actor core calls
//! these; each feature lives in its own module.

use super::Engine;
use deskbridge_input::hotkeys::HotkeyAction;
use deskbridge_platform::clipboard::ClipboardEvent;
use deskbridge_proto::pb;

impl Engine {
    /// A control message the core does not handle itself.
    pub(crate) fn on_feature_control(&mut self, msg: pb::control::Msg) {
        use pb::control::Msg as M;
        if self.on_clipboard_control(msg.clone())
            || self.on_audio_control(&msg)
            || self.focus_control(&msg)
        {
            return;
        }
        match msg {
            M::SendSas(_) => {
                if let Err(e) = self.platform.system.send_sas() {
                    tracing::warn!("Ctrl+Alt+Del request failed: {e}");
                    self.warn_once(
                        "sas_unavailable",
                        "Strg+Alt+Entf ist nur mit dem Desk-Bridge-Dienst möglich.",
                    );
                }
            }
            M::UpdateOffer(o) => self.on_update_offer(o),
            M::UpdateAnswer(a) => self.on_update_answer(a),
            M::SendFiles(sf) => self.on_send_files(sf),
            M::FileRequest(r) => self.on_file_request(r),
            M::TransferProgress(p) => self.on_transfer_progress(p),
            other => tracing::trace!(?other, "unhandled control message"),
        }
    }

    pub(crate) fn on_session_up_features(&mut self) {
        self.start_transfers();
        self.start_clipboard();
        self.start_audio();
    }

    pub(crate) fn on_session_down_features(&mut self) {
        self.focus_session_down();
        self.stop_audio();
        self.stop_clipboard();
        self.stop_transfers();
    }

    pub(crate) fn on_feature_hotkey(&mut self, h: HotkeyAction) {
        match h {
            HotkeyAction::MicMute => self.toggle_mic_mute(),
            HotkeyAction::LaptopAudioMute => self.toggle_laptop_mute(),
            HotkeyAction::FocusCycle | HotkeyAction::FocusSplit | HotkeyAction::StatsOverlay => {
                self.on_focus_hotkey(h)
            }
            HotkeyAction::SendSas => {
                if self.router.is_remote()
                    && let Some(s) = &self.session
                {
                    s.link
                        .send_control(pb::control::Msg::SendSas(pb::SendSas {}));
                }
            }
            other => tracing::debug!(?other, "hotkey without handler"),
        }
    }

    pub(crate) fn on_feature_emergency(&mut self) {
        self.stop_focus("Fokusmodus beendet", true);
    }

    pub(crate) fn on_clipboard(&mut self, ev: ClipboardEvent) {
        self.on_clipboard_event(ev);
    }

    pub(crate) fn on_lid(&mut self, closed: bool) {
        self.focus_on_lid(closed);
    }

    /// UI requests that belong to feature modules.
    pub(crate) fn handle_ui_feature(
        &mut self,
        req: crate::ipc::UiRequest,
    ) -> Result<serde_json::Value, String> {
        use crate::ipc::UiRequest as R;
        match req {
            R::SendFiles { paths } => self.send_files(paths),
            R::OpenIncomingFolder => self.open_incoming_folder(),
            R::ShowTransferInFolder { transfer_id } => self.show_transfer(transfer_id),
            R::ListAudioDevices => self.list_audio_devices(),
            R::PlayTestTone => self.play_test_tone(),
            R::ExportDiagnostics { target } => self.export_diagnostics(target),
            R::OfferPartnerUpdate { path, version } => self.offer_partner_update(path, version),
            R::AnswerPartnerUpdate { accept } => self.answer_partner_update(accept),
            R::CheckForUpdate | R::MakeNetworkPrivate | R::RestartAgent => {
                Err("Diese Funktion ist noch nicht verfügbar".into())
            }
            _ => Err("Unbekannte Anfrage".into()),
        }
    }

    /// Re-applies feature related settings after a config change.
    pub(crate) fn apply_config_to_features(&mut self) {
        self.apply_clip_config();
        self.apply_audio_config();
    }
}

impl Engine {
    /// Human readable system block for the diagnostics archive.
    fn system_info(&self) -> String {
        use std::fmt::Write;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "Desk Bridge {} (proto {:#x})",
            crate::brand::VERSION,
            deskbridge_proto::PROTO_VERSION
        );
        let _ = writeln!(s, "os: {}", self.platform.os);
        let _ = writeln!(
            s,
            "device: {} ({})",
            self.cfg.device.name, self.identity.device_id
        );
        let _ = writeln!(
            s,
            "roles: input_source={} headset_sink={} stream_source={}",
            self.cfg.device.input_source,
            self.cfg.device.headset_sink,
            self.cfg.device.stream_source
        );
        let _ = writeln!(
            s,
            "connection: {:?}, last error: {:?}",
            self.conn_state, self.last_error
        );
        if let Some(a) = self.session.as_ref() {
            let _ = writeln!(
                s,
                "peer: {} v{} caps={:?}",
                a.link.peer.name,
                a.link.hello.app_version,
                a.link
                    .hello
                    .caps
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
            );
            let _ = writeln!(
                s,
                "rtt_us: {:?}, clock_offset_us: {:?}",
                self.clock.rtt_us(),
                self.clock.offset_us()
            );
        }
        let _ = writeln!(
            s,
            "process: cpu {:.1}% rss {:.1} MB",
            self.proc_cpu, self.proc_rss
        );
        for sc in &self.local_screens {
            let _ = writeln!(
                s,
                "screen {}: {}x{}+{}+{} dpi={} {} mHz primary={} '{}'",
                sc.id, sc.w, sc.h, sc.x, sc.y, sc.dpi, sc.refresh_mhz, sc.primary, sc.name
            );
        }
        for kind in [
            deskbridge_platform::audio::DeviceKind::Render,
            deskbridge_platform::audio::DeviceKind::Capture,
        ] {
            if let Ok(list) = self.platform.audio_devices.list(kind) {
                for d in list {
                    let _ = writeln!(
                        s,
                        "audio {:?}: {} [{}] default={} virtual={}",
                        kind, d.name, d.id, d.is_default, d.is_virtual
                    );
                }
            }
        }
        for w in &self.warnings {
            let _ = writeln!(s, "warning {}: {}", w.code, w.message);
        }
        s
    }

    pub(crate) fn export_diagnostics(
        &mut self,
        target: Option<String>,
    ) -> Result<serde_json::Value, String> {
        let info = self.system_info();
        let path = crate::diagnostics::export(
            &self.paths,
            target.as_deref().map(std::path::Path::new),
            &info,
        )
        .map_err(|e| format!("Diagnosepaket konnte nicht erstellt werden: {e}"))?;
        let p = path.to_string_lossy().into_owned();
        self.emit(crate::ipc::UiEvent::DiagnosticsExported { path: p.clone() });
        Ok(serde_json::json!({ "path": p }))
    }
}
