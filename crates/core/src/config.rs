//! `config.toml`: schema-versioned settings, owned by the agent. The UI changes them via IPC.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub schema_version: u32,
    pub device: DeviceConfig,
    pub general: GeneralConfig,
    pub network: NetworkConfig,
    pub input: InputConfig,
    pub hotkeys: BTreeMap<String, String>,
    pub clipboard: ClipboardConfig,
    pub files: FilesConfig,
    pub audio: AudioConfig,
    pub focus: FocusConfig,
    pub layout: LayoutConfig,
    pub update: UpdateConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            device: DeviceConfig::default(),
            general: GeneralConfig::default(),
            network: NetworkConfig::default(),
            input: InputConfig::default(),
            hotkeys: BTreeMap::new(),
            clipboard: ClipboardConfig::default(),
            files: FilesConfig::default(),
            audio: AudioConfig::default(),
            focus: FocusConfig::default(),
            layout: LayoutConfig::default(),
            update: UpdateConfig::default(),
        }
    }
}

/// Which roles this device plays (plan section 4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceConfig {
    pub name: String,
    /// Mouse and keyboard are physically attached here (PC).
    pub input_source: bool,
    /// The headset is attached here (PC).
    pub headset_sink: bool,
    /// This device is shown in focus mode (laptop).
    pub stream_source: bool,
    pub onboarding_done: bool,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            input_source: true,
            headset_sink: true,
            stream_source: false,
            onboarding_done: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub autostart: bool,
    /// "auto" | "de" | "en"
    pub language: String,
    /// "system" | "light" | "dark"
    pub theme: String,
    /// "error" | "warn" | "info" | "debug" | "trace"
    pub log_level: String,
    pub paused: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            autostart: true,
            language: "auto".into(),
            theme: "system".into(),
            log_level: "info".into(),
            paused: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    pub port: u16,
    /// Manually configured peers (IP or host name), used when mDNS is blocked.
    pub manual_hosts: Vec<String>,
    /// Local address to listen on; empty = all interfaces (`0.0.0.0`).
    pub bind_address: String,
    /// Addresses announced via mDNS and shown to the other device; empty = all interfaces.
    /// Set this if the PC has virtual adapters (Hyper-V, WSL, VPN) that peers cannot reach.
    pub advertise_addresses: Vec<String>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            port: deskbridge_proto::DEFAULT_PORT,
            manual_hosts: Vec::new(),
            bind_address: String::new(),
            advertise_addresses: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputConfig {
    pub enabled: bool,
    pub switch_delay_ms: u32,
    /// "none" | "ctrl" | "alt" | "shift"
    pub require_modifier: String,
    pub corner_margin_px: i32,
    pub double_bump: bool,
    /// Inject mouse without pointer acceleration on the target.
    pub raw_one_to_one: bool,
    pub hide_cursor_on_remote: bool,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            switch_delay_ms: 0,
            require_modifier: "none".into(),
            corner_margin_px: 8,
            double_bump: false,
            raw_one_to_one: false,
            hide_cursor_on_remote: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipboardConfig {
    pub send: bool,
    pub receive: bool,
    pub max_size_mb: u32,
    /// Text below this size is sent immediately.
    pub eager_limit_kb: u32,
    pub sync_images: bool,
    pub sync_rich_text: bool,
    pub sync_files: bool,
    /// Files copied on the other device only replace this clipboard once the cursor arrives here
    /// (needs mouse sharing; otherwise they are applied immediately).
    pub files_on_arrival: bool,
    /// Never sync items marked as sensitive by password managers.
    pub respect_sensitive: bool,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            send: true,
            receive: true,
            max_size_mb: 512,
            eager_limit_kb: 1024,
            sync_images: true,
            sync_rich_text: true,
            sync_files: true,
            files_on_arrival: true,
            respect_sensitive: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilesConfig {
    /// Where files from "Send to" / the drop zone land. Empty = `Downloads\Desk Bridge`.
    pub target_dir: String,
    pub parallel_streams: u32,
}

impl Default for FilesConfig {
    fn default() -> Self {
        Self {
            target_dir: String::new(),
            parallel_streams: 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    pub laptop_to_pc: LaptopAudio,
    pub mic_to_laptop: MicAudio,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LaptopAudio {
    pub enabled: bool,
    /// Render device id on the headset device; empty = system default.
    pub output_device: String,
    pub volume: f32,
    pub mute: bool,
    /// "auto" | "pcm" | "opus"
    pub codec: String,
    pub opus_bitrate_kbps: u32,
    /// Jitter buffer start target in milliseconds (0 = automatic).
    pub buffer_ms: u32,
    /// Mute this device's own speakers while its sound is forwarded to the other device.
    pub mute_local_output: bool,
}

impl Default for LaptopAudio {
    fn default() -> Self {
        Self {
            enabled: true,
            output_device: String::new(),
            volume: 1.0,
            mute: false,
            codec: "auto".into(),
            opus_bitrate_kbps: 160,
            buffer_ms: 0,
            mute_local_output: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MicAudio {
    pub enabled: bool,
    /// Capture device id on the headset device; empty = default communications device.
    pub input_device: String,
    /// Only transmit while an application on the laptop has the virtual mic open.
    pub only_when_used: bool,
    pub mute: bool,
    pub set_as_default: bool,
}

impl Default for MicAudio {
    fn default() -> Self {
        Self {
            enabled: true,
            input_device: String::new(),
            only_when_used: true,
            mute: false,
            set_as_default: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FocusConfig {
    /// "full" | "split" | "window"
    pub mode: String,
    /// "left" | "right": which half the laptop gets in split mode.
    pub split_side: String,
    /// Share of the monitor the laptop gets in split mode in percent (50, 67, 33).
    pub split_percent: u32,
    /// Monitor index on the PC; -1 = the one with the cursor.
    pub target_monitor: i32,
    /// 0 = auto (= monitor refresh rate).
    pub fps: u32,
    /// "auto" | "hevc" | "av1" | "h264"
    pub codec: String,
    pub bitrate_auto: bool,
    pub bitrate_kbps: u32,
    pub yuv444: bool,
    pub disable_internal_display: bool,
    /// Start focus mode when the laptop lid is closed.
    pub lid_auto: bool,
    /// Set the lid action to "do nothing" (only with consent).
    pub lid_do_nothing: bool,
}

impl Default for FocusConfig {
    fn default() -> Self {
        Self {
            mode: "full".into(),
            split_side: "right".into(),
            split_percent: 50,
            target_monitor: -1,
            fps: 0,
            codec: "auto".into(),
            bitrate_auto: true,
            bitrate_kbps: 120_000,
            yuv444: false,
            disable_internal_display: true,
            lid_auto: false,
            lid_do_nothing: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutConfig {
    pub placement: Vec<PlacementConfig>,
    pub revision: u64,
    /// Side of the PC's primary screen where the laptop is placed by default.
    pub default_side: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlacementConfig {
    pub device_id: String,
    pub screen_id: u32,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateConfig {
    pub check_automatically: bool,
    pub offer_partner_update: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            check_automatically: true,
            offer_partner_update: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    Parse(String),
    #[error("config was written by a newer version (schema {found}, supported {SCHEMA_VERSION})")]
    TooNew { found: u32 },
}

impl Config {
    /// Loads the file, migrating old schemas. A missing file yields defaults.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        let mut value: toml::Value =
            toml::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        let found = value
            .get("schema_version")
            .and_then(toml::Value::as_integer)
            .map_or(0, |v| u32::try_from(v).unwrap_or(0));
        if found > SCHEMA_VERSION {
            return Err(ConfigError::TooNew { found });
        }
        migrate(&mut value, found);
        value
            .try_into::<Self>()
            .map_err(|e| ConfigError::Parse(e.to_string()))
    }

    /// Atomically writes the file.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| ConfigError::Parse(e.to_string()))?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    /// Fixes values the UI could have sent out of range.
    pub fn sanitize(&mut self) {
        self.input.switch_delay_ms = self.input.switch_delay_ms.min(500);
        self.input.corner_margin_px = self.input.corner_margin_px.clamp(0, 200);
        self.audio.laptop_to_pc.volume = self.audio.laptop_to_pc.volume.clamp(0.0, 1.5);
        self.audio.laptop_to_pc.opus_bitrate_kbps =
            self.audio.laptop_to_pc.opus_bitrate_kbps.clamp(32, 510);
        self.focus.split_percent = self.focus.split_percent.clamp(20, 80);
        self.focus.bitrate_kbps = self.focus.bitrate_kbps.clamp(5_000, 400_000);
        self.files.parallel_streams = self.files.parallel_streams.clamp(1, 8);
        if self.network.port < 1024 {
            self.network.port = deskbridge_proto::DEFAULT_PORT;
        }
        self.clipboard.max_size_mb = self.clipboard.max_size_mb.clamp(1, 16_384);
        self.schema_version = SCHEMA_VERSION;
    }
}

/// Upgrades an older config document in place. Add a step per schema bump.
fn migrate(value: &mut toml::Value, from: u32) {
    if from == 0 {
        // v0 → v1: the file predates versioning; all keys are compatible.
        if let Some(t) = value.as_table_mut() {
            t.insert("schema_version".into(), toml::Value::Integer(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let c = Config::default();
        c.save(&p).unwrap();
        assert_eq!(Config::load(&p).unwrap(), c);
    }

    #[test]
    fn missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Config::load(&dir.path().join("nope.toml")).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn partial_file_gets_defaults_for_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "schema_version = 1\n[input]\nswitch_delay_ms = 120\n").unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.input.switch_delay_ms, 120);
        assert!(c.clipboard.send);
    }

    #[test]
    fn unversioned_file_is_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "[general]\ntheme = \"dark\"\n").unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.general.theme, "dark");
        assert_eq!(c.schema_version, 1);
    }

    #[test]
    fn newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "schema_version = 99\n").unwrap();
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::TooNew { found: 99 })
        ));
    }

    #[test]
    fn sanitize_clamps() {
        let mut c = Config::default();
        c.input.switch_delay_ms = 99_999;
        c.network.port = 80;
        c.sanitize();
        assert_eq!(c.input.switch_delay_ms, 500);
        assert_eq!(c.network.port, deskbridge_proto::DEFAULT_PORT);
    }
}
