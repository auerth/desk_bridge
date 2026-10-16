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
        if self.on_clipboard_control(msg.clone()) || self.on_audio_control(&msg) {
            return;
        }
        match msg {
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
        self.stop_audio();
        self.stop_clipboard();
        self.stop_transfers();
    }

    pub(crate) fn on_feature_hotkey(&mut self, h: HotkeyAction) {
        match h {
            HotkeyAction::MicMute => self.toggle_mic_mute(),
            HotkeyAction::LaptopAudioMute => self.toggle_laptop_mute(),
            other => tracing::debug!(?other, "hotkey without handler"),
        }
    }

    pub(crate) fn on_feature_emergency(&mut self) {}

    pub(crate) fn on_clipboard(&mut self, ev: ClipboardEvent) {
        self.on_clipboard_event(ev);
    }

    pub(crate) fn on_lid(&mut self, _closed: bool) {}

    /// UI requests that belong to feature modules.
    pub(crate) fn handle_ui_feature(&mut self, req: crate::ipc::UiRequest) -> Result<serde_json::Value, String> {
        use crate::ipc::UiRequest as R;
        match req {
            R::SendFiles { paths } => self.send_files(paths),
            R::OpenIncomingFolder => self.open_incoming_folder(),
            R::ShowTransferInFolder { transfer_id } => self.show_transfer(transfer_id),
            R::ListAudioDevices => self.list_audio_devices(),
            R::PlayTestTone => self.play_test_tone(),
            R::ExportDiagnostics { .. }
            | R::CheckForUpdate
            | R::MakeNetworkPrivate
            | R::RestartAgent => Err("Diese Funktion ist noch nicht verfügbar".into()),
            _ => Err("Unbekannte Anfrage".into()),
        }
    }

    pub(crate) fn set_focus(&mut self, _mode: &str) -> Result<serde_json::Value, String> {
        Err("Der Fokusmodus ist noch nicht verfügbar".into())
    }

    /// Re-applies feature related settings after a config change.
    pub(crate) fn apply_config_to_features(&mut self) {
        self.apply_clip_config();
        self.apply_audio_config();
    }
}
