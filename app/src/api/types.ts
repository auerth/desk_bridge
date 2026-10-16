// TypeScript mirror of `crates/core/src/ipc.rs` and `crates/core/src/config.rs`.
// serde rules: enums tagged with `type` (requests, pushes) or `kind` (UiEvent) in snake_case;
// `AudioFeatureDto.base` and `FocusDto.base` are #[serde(flatten)]ed, i.e. their fields sit at the top level.
// Keep in sync with the Rust side.

// ---------------------------------------------------------------------------------------------
// Client -> agent
// ---------------------------------------------------------------------------------------------

export type Feature = 'input' | 'laptop_audio' | 'mic' | 'clipboard' | 'focus' | 'mute_laptop_audio' | 'mute_mic';

export interface PlacementDto {
  device_id: string;
  screen_id: number;
  x: number;
  y: number;
}

export type UiRequest =
  | { type: 'get_state' }
  | { type: 'get_config' }
  | { type: 'set_config'; config: Config }
  | { type: 'complete_onboarding'; name: string; is_pc: boolean }
  | { type: 'start_pairing' }
  | { type: 'cancel_pairing' }
  | { type: 'pair_with'; device_id: string | null; address: string | null; code: string }
  | { type: 'unpair'; device_id: string }
  | { type: 'rename_peer'; device_id: string; name: string }
  | { type: 'add_manual_host'; host: string }
  | { type: 'set_feature'; feature: Feature; enabled: boolean }
  | { type: 'set_layout'; placements: PlacementDto[] }
  | { type: 'set_focus'; mode: 'off' | 'full' | 'split' | 'window' | 'cycle' }
  | { type: 'jump_to_other_device' }
  | { type: 'set_cursor_lock'; locked: boolean }
  | { type: 'list_audio_devices' }
  | { type: 'play_test_tone' }
  | { type: 'send_files'; paths: string[] }
  | { type: 'open_incoming_folder' }
  | { type: 'show_transfer_in_folder'; transfer_id: number }
  | { type: 'export_diagnostics'; target: string | null }
  | { type: 'check_for_update' }
  | { type: 'offer_partner_update'; path: string; version: string }
  | { type: 'answer_partner_update'; accept: boolean }
  | { type: 'make_network_private' }
  | { type: 'set_paused'; paused: boolean }
  | { type: 'restart_agent' }
  | { type: 'shutdown_agent' }
  | { type: 'ping' };

// ---------------------------------------------------------------------------------------------
// Agent -> client
// ---------------------------------------------------------------------------------------------

export interface AudioDeviceDto {
  id: string;
  name: string;
  is_default: boolean;
  is_default_communications: boolean;
  is_virtual: boolean;
}

export type UiEvent =
  | { kind: 'osd'; icon: string; text: string }
  | { kind: 'pairing_result'; ok: boolean; message: string; peer_name: string | null }
  | { kind: 'notification'; level: string; text: string }
  | { kind: 'audio_devices'; render: AudioDeviceDto[]; capture: AudioDeviceDto[] }
  | { kind: 'update_available'; version: string; notes: string }
  | { kind: 'partner_update_offered'; version: string; size: number; from: string }
  | { kind: 'partner_update_ready'; path: string; version: string }
  | { kind: 'diagnostics_exported'; path: string };

export interface DeviceDto {
  id: string;
  name: string;
  version: string;
  os: string;
  input_source: boolean;
  headset_sink: boolean;
  stream_source: boolean;
  onboarding_done: boolean;
  /** `ip:port` endpoints the other device can use to reach this one. */
  addresses: string[];
}

export type ConnState =
  | 'idle'
  | 'discovering'
  | 'connecting'
  | 'handshaking'
  | 'connected'
  | 'degraded'
  | 'reconnecting'
  | 'paused';

export interface PeerDto {
  id: string;
  name: string;
  online: boolean;
  fingerprint_short: string;
  last_ip: string | null;
}

export interface ConnectionDto {
  state: ConnState;
  peer: PeerDto | null;
  rtt_us: number | null;
  clock_offset_us: number | null;
  /** "wired" | "wireless" | "unknown" */
  link: string;
  since_unix_ms: number | null;
  error: string | null;
  version_mismatch: boolean;
  peer_version: string | null;
}

export interface DiscoveredDto {
  id: string;
  name: string;
  address: string;
  version: string;
  paired: boolean;
}

export interface PairingDto {
  code: string;
  seconds_left: number;
}

export type Live = 'off' | 'ready' | 'live' | 'unavailable';

export interface FeatureDto {
  enabled: boolean;
  state: Live;
  detail: string | null;
}

export interface AudioFeatureDto extends FeatureDto {
  muted: boolean;
  /** 0..1 peak level for the meter. */
  level: number;
  latency_ms: number | null;
  /** "pcm" | "opus" | "" */
  codec: string;
  buffer_ms: number | null;
  device_name: string | null;
  device_missing: boolean;
}

export interface StreamStatsDto {
  fps: number;
  bitrate_kbps: number;
  loss_pct: number;
  encode_ms: number;
  network_ms: number;
  decode_ms: number;
  total_ms: number;
}

export interface FocusDto extends FeatureDto {
  /** "off" | "preparing" | "streaming" | "stopping" */
  phase: string;
  /** "full" | "split" | "window" */
  mode: string;
  width: number;
  height: number;
  fps: number;
  bitrate_kbps: number;
  codec: string;
  encoder: string;
  stats: StreamStatsDto | null;
  unavailable_reason: string | null;
}

export interface FeaturesDto {
  input: FeatureDto;
  laptop_audio: AudioFeatureDto;
  mic: AudioFeatureDto;
  clipboard: FeatureDto;
  focus: FocusDto;
}

export interface ScreenDto {
  device_id: string;
  id: number;
  x: number;
  y: number;
  w: number;
  h: number;
  dpi: number;
  primary: boolean;
  name: string;
}

export interface ScreensDto {
  screens: ScreenDto[];
  placements: PlacementDto[];
  revision: number;
}

export interface WarningDto {
  /** "network_public" | "no_encoder" | "no_virtual_display" | "no_virtual_audio" | "firewall" | "version_mismatch" | "elevation" | "headset_missing" ... */
  code: string;
  message: string;
}

export interface TransferDto {
  id: number;
  /** "send" | "receive" */
  direction: string;
  name: string;
  files: number;
  total_bytes: number;
  done_bytes: number;
  /** "running" | "done" | "failed" | "cancelled" */
  status: string;
  path: string | null;
  started_unix_ms: number;
}

export interface UpdateDto {
  current: string;
  available: string | null;
  checking: boolean;
}

export interface MetricsDto {
  /** Last ~60 RTT samples in microseconds. */
  rtt_history_us: number[];
  /** One-way input latency (peer timestamp to injection) in microseconds. */
  input_latency_p50_us: number | null;
  input_latency_p95_us: number | null;
  input_latency_max_us: number | null;
  audio_buffer_fill_ms: number | null;
  audio_drift_ppm: number | null;
  audio_loss_pct: number | null;
  agent_cpu_pct: number | null;
  agent_rss_mb: number | null;
}

export interface AppState {
  device: DeviceDto;
  connection: ConnectionDto;
  peers: PeerDto[];
  discovered: DiscoveredDto[];
  pairing: PairingDto | null;
  features: FeaturesDto;
  screens: ScreensDto;
  /** "local" | "remote": where the mouse and keyboard currently are. */
  location: string;
  cursor_locked: boolean;
  warnings: WarningDto[];
  transfers: TransferDto[];
  update: UpdateDto;
  metrics: MetricsDto;
  paused: boolean;
}

// ---------------------------------------------------------------------------------------------
// Config (TOML <-> JSON)
// ---------------------------------------------------------------------------------------------

export interface DeviceConfig {
  name: string;
  input_source: boolean;
  headset_sink: boolean;
  stream_source: boolean;
  onboarding_done: boolean;
}

export interface GeneralConfig {
  autostart: boolean;
  language: 'auto' | 'de' | 'en';
  theme: 'system' | 'light' | 'dark';
  log_level: 'error' | 'warn' | 'info' | 'debug' | 'trace';
  paused: boolean;
}

export interface NetworkConfig {
  port: number;
  manual_hosts: string[];
  /** Local address to listen on; empty = all interfaces. */
  bind_address: string;
  /** Addresses announced to the other device; empty = all interfaces. */
  advertise_addresses: string[];
}

export interface InputConfig {
  enabled: boolean;
  switch_delay_ms: number;
  require_modifier: 'none' | 'ctrl' | 'alt' | 'shift';
  corner_margin_px: number;
  double_bump: boolean;
  raw_one_to_one: boolean;
  hide_cursor_on_remote: boolean;
}

export interface ClipboardConfig {
  send: boolean;
  receive: boolean;
  max_size_mb: number;
  eager_limit_kb: number;
  sync_images: boolean;
  sync_rich_text: boolean;
  sync_files: boolean;
  files_on_arrival: boolean;
  respect_sensitive: boolean;
}

export interface FilesConfig {
  target_dir: string;
  parallel_streams: number;
}

export interface LaptopAudioConfig {
  enabled: boolean;
  output_device: string;
  volume: number;
  mute: boolean;
  codec: 'auto' | 'pcm' | 'opus';
  opus_bitrate_kbps: number;
  buffer_ms: number;
  mute_local_output: boolean;
}

export interface MicAudioConfig {
  enabled: boolean;
  input_device: string;
  only_when_used: boolean;
  mute: boolean;
  set_as_default: boolean;
}

export interface AudioConfig {
  laptop_to_pc: LaptopAudioConfig;
  mic_to_laptop: MicAudioConfig;
}

export interface FocusConfig {
  mode: 'full' | 'split' | 'window';
  split_side: 'left' | 'right';
  split_percent: number;
  target_monitor: number;
  fps: number;
  codec: 'auto' | 'hevc' | 'av1' | 'h264';
  bitrate_auto: boolean;
  bitrate_kbps: number;
  yuv444: boolean;
  disable_internal_display: boolean;
  lid_auto: boolean;
  lid_do_nothing: boolean;
}

export interface LayoutConfig {
  placement: PlacementDto[];
  revision: number;
  default_side: string;
}

export interface UpdateConfig {
  check_automatically: boolean;
  offer_partner_update: boolean;
}

export interface Config {
  schema_version: number;
  device: DeviceConfig;
  general: GeneralConfig;
  network: NetworkConfig;
  input: InputConfig;
  /** action key -> "Ctrl+Alt+Shift+F"; missing = default binding. */
  hotkeys: Record<string, string>;
  clipboard: ClipboardConfig;
  files: FilesConfig;
  audio: AudioConfig;
  focus: FocusConfig;
  layout: LayoutConfig;
  update: UpdateConfig;
}

/** Hotkey action keys (see `crates/input/src/hotkeys.rs`). */
export const HOTKEY_ACTIONS = [
  'focus_cycle',
  'focus_split',
  'lock_cursor',
  'jump_other',
  'mic_mute',
  'laptop_audio_mute',
  'stats_overlay',
  'send_sas',
  'emergency',
] as const;
export type HotkeyAction = (typeof HOTKEY_ACTIONS)[number];

export const DEFAULT_HOTKEYS: Record<HotkeyAction, string> = {
  focus_cycle: 'Ctrl+Alt+Shift+F',
  focus_split: 'Ctrl+Alt+Shift+D',
  lock_cursor: 'ScrollLock',
  jump_other: 'Ctrl+Alt+Shift+Space',
  mic_mute: 'Ctrl+Alt+Shift+M',
  laptop_audio_mute: 'Ctrl+Alt+Shift+S',
  stats_overlay: 'Ctrl+Alt+Shift+I',
  send_sas: 'Ctrl+Alt+End',
  emergency: 'Ctrl+Alt+Shift+Backspace',
};

/** Mirrors `Config::default()`; used by the mock and as a fallback while the real config loads. */
export function defaultConfig(): Config {
  return {
    schema_version: 1,
    device: { name: '', input_source: true, headset_sink: true, stream_source: false, onboarding_done: false },
    general: { autostart: true, language: 'auto', theme: 'system', log_level: 'info', paused: false },
    network: { port: 47820, manual_hosts: [], bind_address: '', advertise_addresses: [] },
    input: {
      enabled: true,
      switch_delay_ms: 0,
      require_modifier: 'none',
      corner_margin_px: 8,
      double_bump: false,
      raw_one_to_one: false,
      hide_cursor_on_remote: false,
    },
    hotkeys: {},
    clipboard: {
      send: true,
      receive: true,
      max_size_mb: 512,
      eager_limit_kb: 1024,
      sync_images: true,
      sync_rich_text: true,
      sync_files: true,
      files_on_arrival: true,
      respect_sensitive: true,
    },
    files: { target_dir: '', parallel_streams: 4 },
    audio: {
      laptop_to_pc: {
        enabled: true,
        output_device: '',
        volume: 1.0,
        mute: false,
        codec: 'auto',
        opus_bitrate_kbps: 160,
        buffer_ms: 0,
        mute_local_output: true,
      },
      mic_to_laptop: { enabled: true, input_device: '', only_when_used: true, mute: false, set_as_default: true },
    },
    focus: {
      mode: 'full',
      split_side: 'right',
      split_percent: 50,
      target_monitor: -1,
      fps: 0,
      codec: 'auto',
      bitrate_auto: true,
      bitrate_kbps: 120_000,
      yuv444: false,
      disable_internal_display: true,
      lid_auto: false,
      lid_do_nothing: false,
    },
    layout: { placement: [], revision: 0, default_side: 'right' },
    update: { check_automatically: true, offer_partner_update: true },
  };
}
