//! UI ⇄ agent protocol.
//!
//! Transport: named pipe `\\.\pipe\deskbridge-ui`, newline-delimited JSON.
//!
//! * client → agent: `{"id": 7, "req": {"type": "get_state"}}`
//! * agent → client reply: `{"id": 7, "ok": true, "data": ...}` or `{"id": 7, "ok": false, "error": "..."}`
//! * agent → client push: `{"push": {"type": "state", "state": {...}}}` (full snapshot on every change,
//!   throttled) and `{"push": {"type": "event", ...}}` for transient things (OSD, pairing result).

use crate::config::Config;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------------------------
// Client → agent
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub id: u64,
    pub req: UiRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiRequest {
    /// Returns the current [`AppState`].
    GetState,
    GetConfig,
    /// Replaces the config (validated and clamped by the agent); returns the stored config.
    SetConfig {
        config: Box<Config>,
    },
    /// Complete onboarding with chosen name/roles.
    CompleteOnboarding {
        name: String,
        is_pc: bool,
    },
    /// Show a pairing code on this device (this device is the pairing server).
    StartPairing,
    CancelPairing,
    /// Enter a code to pair with a device found via discovery or a manual address.
    PairWith {
        device_id: Option<String>,
        address: Option<String>,
        code: String,
    },
    Unpair {
        device_id: String,
    },
    RenamePeer {
        device_id: String,
        name: String,
    },
    /// Connect to a manually entered host/IP (does not pair).
    AddManualHost {
        host: String,
    },
    /// Toggle one of the dashboard chips.
    SetFeature {
        feature: Feature,
        enabled: bool,
    },
    SetLayout {
        placements: Vec<PlacementDto>,
    },
    /// Focus mode: "off" | "full" | "split" | "window" | "cycle".
    SetFocus {
        mode: String,
    },
    /// Switch the cursor between devices (same as the jump hotkey).
    JumpToOtherDevice,
    SetCursorLock {
        locked: bool,
    },
    ListAudioDevices,
    /// Play a test tone "from the laptop" on the headset.
    PlayTestTone,
    SendFiles {
        paths: Vec<String>,
    },
    OpenIncomingFolder,
    ShowTransferInFolder {
        transfer_id: u64,
    },
    ExportDiagnostics {
        target: Option<String>,
    },
    CheckForUpdate,
    /// Offer the installer at `path` to the partner device (it asks its user first).
    OfferPartnerUpdate {
        path: String,
        version: String,
    },
    /// Answer a partner's update offer.
    AnswerPartnerUpdate {
        accept: bool,
    },
    /// Mark the public network profile hint as handled ("make private").
    MakeNetworkPrivate,
    /// Disconnect everything / resume.
    SetPaused {
        paused: bool,
    },
    RestartAgent,
    ShutdownAgent,
    Ping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    Input,
    LaptopAudio,
    Mic,
    Clipboard,
    Focus,
    MuteLaptopAudio,
    MuteMic,
}

// ---------------------------------------------------------------------------------------------
// Agent → client
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ServerMessage {
    Reply(ReplyEnvelope),
    Push { push: Push },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyEnvelope {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Push {
    State { state: Box<AppState> },
    Event { event: UiEvent },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiEvent {
    /// Short pill on screen (also drawn by the agent as native OSD).
    Osd {
        icon: String,
        text: String,
    },
    PairingResult {
        ok: bool,
        message: String,
        peer_name: Option<String>,
    },
    Notification {
        level: String,
        text: String,
    },
    /// Audio device list answer for `ListAudioDevices`.
    AudioDevices {
        render: Vec<AudioDeviceDto>,
        capture: Vec<AudioDeviceDto>,
    },
    UpdateAvailable {
        version: String,
        notes: String,
    },
    /// The partner offers an update package (ask the user, then `AnswerPartnerUpdate`).
    PartnerUpdateOffered {
        version: String,
        size: u64,
        from: String,
    },
    /// A partner update arrived, verified (SHA-256). The UI offers to start it.
    PartnerUpdateReady {
        path: String,
        version: String,
    },
    DiagnosticsExported {
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioDeviceDto {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub is_default_communications: bool,
    pub is_virtual: bool,
}

// ---------------------------------------------------------------------------------------------
// State snapshot
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppState {
    pub device: DeviceDto,
    pub connection: ConnectionDto,
    pub peers: Vec<PeerDto>,
    pub discovered: Vec<DiscoveredDto>,
    pub pairing: Option<PairingDto>,
    pub features: FeaturesDto,
    pub screens: ScreensDto,
    /// "local" | "remote": where the mouse and keyboard currently are.
    pub location: String,
    pub cursor_locked: bool,
    pub warnings: Vec<WarningDto>,
    pub transfers: Vec<TransferDto>,
    pub update: UpdateDto,
    pub metrics: MetricsDto,
    pub paused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceDto {
    pub id: String,
    pub name: String,
    pub version: String,
    pub os: String,
    pub input_source: bool,
    pub headset_sink: bool,
    pub stream_source: bool,
    pub onboarding_done: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnState {
    #[default]
    Idle,
    Discovering,
    Connecting,
    Handshaking,
    Connected,
    Degraded,
    Reconnecting,
    Paused,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectionDto {
    pub state: ConnState,
    pub peer: Option<PeerDto>,
    pub rtt_us: Option<u32>,
    pub clock_offset_us: Option<i64>,
    /// "wired" | "wireless" | "unknown"
    pub link: String,
    pub since_unix_ms: Option<u64>,
    pub error: Option<String>,
    /// Peer runs an incompatible major protocol version.
    pub version_mismatch: bool,
    /// Peer runs a different app version (hint in the UI).
    pub peer_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PeerDto {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub fingerprint_short: String,
    pub last_ip: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiscoveredDto {
    pub id: String,
    pub name: String,
    pub address: String,
    pub version: String,
    pub paired: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PairingDto {
    pub code: String,
    pub seconds_left: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Live {
    /// Switched off by the user.
    #[default]
    Off,
    /// Enabled but not doing anything right now / no peer.
    Ready,
    /// Actively in use (mic open, focus streaming, mouse on the other device ...).
    Live,
    /// Not possible on this system (no driver, no encoder ...).
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FeatureDto {
    pub enabled: bool,
    pub state: Live,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AudioFeatureDto {
    #[serde(flatten)]
    pub base: FeatureDto,
    pub muted: bool,
    /// 0..1 peak level for the meter.
    pub level: f32,
    pub latency_ms: Option<f32>,
    /// "pcm" | "opus" | ""
    pub codec: String,
    pub buffer_ms: Option<f32>,
    pub device_name: Option<String>,
    /// The headset device is currently absent (powered off); stream is paused.
    pub device_missing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FocusDto {
    #[serde(flatten)]
    pub base: FeatureDto,
    /// "off" | "preparing" | "streaming" | "stopping"
    pub phase: String,
    /// "full" | "split" | "window"
    pub mode: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub codec: String,
    pub encoder: String,
    pub stats: Option<StreamStatsDto>,
    /// Why focus mode cannot be used (no encoder, no virtual display driver ...).
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StreamStatsDto {
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub loss_pct: f32,
    pub encode_ms: f32,
    pub network_ms: f32,
    pub decode_ms: f32,
    pub total_ms: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FeaturesDto {
    pub input: FeatureDto,
    pub laptop_audio: AudioFeatureDto,
    pub mic: AudioFeatureDto,
    pub clipboard: FeatureDto,
    pub focus: FocusDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ScreenDto {
    pub device_id: String,
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub dpi: u32,
    pub primary: bool,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlacementDto {
    pub device_id: String,
    pub screen_id: u32,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScreensDto {
    /// Every screen of both devices (device_id tells which).
    pub screens: Vec<ScreenDto>,
    /// Current placement in layout space (96-dpi normalised units, see `input::layout`).
    pub placements: Vec<PlacementDto>,
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarningDto {
    /// Stable code: "network_public", "no_encoder", "no_virtual_display", "no_virtual_audio", "firewall",
    /// "version_mismatch", "elevation", "headset_missing" ...
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferDto {
    pub id: u64,
    /// "send" | "receive"
    pub direction: String,
    pub name: String,
    pub files: u32,
    pub total_bytes: u64,
    pub done_bytes: u64,
    /// "running" | "done" | "failed" | "cancelled"
    pub status: String,
    pub path: Option<String>,
    pub started_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateDto {
    pub current: String,
    pub available: Option<String>,
    pub checking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MetricsDto {
    /// Last ~60 RTT samples in microseconds (sparkline).
    pub rtt_history_us: Vec<u32>,
    /// One-way input latency (peer timestamp → injection), microseconds, over the last ~2000 events.
    pub input_latency_p50_us: Option<u32>,
    pub input_latency_p95_us: Option<u32>,
    pub input_latency_max_us: Option<u32>,
    pub audio_buffer_fill_ms: Option<f32>,
    pub audio_drift_ppm: Option<f32>,
    pub audio_loss_pct: Option<f32>,
    pub agent_cpu_pct: Option<f32>,
    pub agent_rss_mb: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_json_shape() {
        let r: RequestEnvelope =
            serde_json::from_str(r#"{"id":3,"req":{"type":"set_focus","mode":"split"}}"#).unwrap();
        assert!(matches!(r.req, UiRequest::SetFocus { ref mode } if mode == "split"));
        let r: RequestEnvelope =
            serde_json::from_str(r#"{"id":1,"req":{"type":"get_state"}}"#).unwrap();
        assert!(matches!(r.req, UiRequest::GetState));
    }

    #[test]
    fn state_serialises() {
        let s = AppState::default();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["connection"]["state"], "idle");
        assert_eq!(v["features"]["laptop_audio"]["state"], "off");
    }

    #[test]
    fn push_serialises() {
        let m = ServerMessage::Push {
            push: Push::Event {
                event: UiEvent::Osd {
                    icon: "laptop".into(),
                    text: "Laptop".into(),
                },
            },
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["push"]["type"], "event");
        assert_eq!(v["push"]["event"]["kind"], "osd");
    }
}
