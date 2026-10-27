// Fully scripted fake agent. Used automatically outside Tauri (plain browser) or with `?mock` in the URL.
//
// Scenario switches (query string):
//   ?mock                 connected PC <-> laptop, everything running (default)
//   &onboarding           first start: onboarding flow, one laptop to discover (pairing code to enter: 123456)
//   &role=laptop          this device is the laptop
//   &state=disconnected   paired but peer offline
//   &offline              agent pipe not reachable
//   &focus=unavailable | focus=live
//   &mic=live             mic forwarding active (default: "not yet available")
//   &update               an update is available
//   &theme=dark|light  &lang=de|en
//   &p95=high  &mismatch  &partner=offer  &warn=elevation|sas_unavailable|capture_failed|audio_capture  &nopublic
//   &quiet                no ticking (stable screenshots)

import type { AppInfo, Backend, ConnectionInfo, FileDropEvent, UpdateInfo, Unlisten } from './backend';
import {
  defaultConfig,
  type AppState,
  type AudioDeviceDto,
  type Config,
  type PeerDto,
  type PlacementDto,
  type ScreenDto,
  type TransferDto,
  type UiEvent,
  type UiRequest,
} from './types';

const params = new URLSearchParams(typeof location === 'undefined' ? '' : location.search);
const has = (k: string) => params.has(k);

const PC_ID = 'pc-0001';
const LAPTOP_ID = 'laptop-0001';

function sleep(ms: number) {
  return new Promise<void>((r) => setTimeout(r, ms));
}

function clone<T>(v: T): T {
  return structuredClone(v);
}

function emptyAudio(): AppState['features']['laptop_audio'] {
  return {
    enabled: true,
    state: 'ready',
    detail: null,
    muted: false,
    level: 0,
    latency_ms: null,
    codec: '',
    buffer_ms: null,
    device_name: null,
    device_missing: false,
  };
}

const RENDER: AudioDeviceDto[] = [
  { id: 'r1', name: 'Headset Game (USB-Dongle)', is_default: true, is_default_communications: false, is_virtual: false },
  { id: 'r2', name: 'Headset Chat (USB-Dongle)', is_default: false, is_default_communications: true, is_virtual: false },
  { id: 'r3', name: 'Lautsprecher (Realtek Audio)', is_default: false, is_default_communications: false, is_virtual: false },
  { id: 'r4', name: 'Monitor-Lautsprecher (NVIDIA HD Audio)', is_default: false, is_default_communications: false, is_virtual: false },
];
const CAPTURE: AudioDeviceDto[] = [
  { id: 'c1', name: 'Headset-Mikrofon (USB-Dongle)', is_default: false, is_default_communications: true, is_virtual: false },
  { id: 'c2', name: 'Mikrofon (Realtek Audio)', is_default: true, is_default_communications: false, is_virtual: false },
];

function seedState(): AppState {
  const isLaptop = params.get('role') === 'laptop';
  const onboarding = has('onboarding');
  const disconnected = params.get('state') === 'disconnected';
  const thisId = isLaptop ? LAPTOP_ID : PC_ID;
  const otherId = isLaptop ? PC_ID : LAPTOP_ID;
  const peer: PeerDto = {
    id: otherId,
    name: isLaptop ? 'THORBEN-PC' : 'Thorbens Laptop',
    online: !disconnected,
    fingerprint_short: '7F3A-91C2',
    last_ip: isLaptop ? '192.168.178.20' : '192.168.178.42',
  };
  const screens: ScreenDto[] = [
    { device_id: PC_ID, id: 0, x: 0, y: 0, w: 5120, h: 1440, dpi: 96, primary: true, name: 'Super-Ultrawide 49"' },
    { device_id: LAPTOP_ID, id: 0, x: 0, y: 0, w: 2560, h: 1600, dpi: 144, primary: true, name: 'Internes Display' },
  ];
  const placements: PlacementDto[] = [
    { device_id: PC_ID, screen_id: 0, x: 0, y: 0 },
    { device_id: LAPTOP_ID, screen_id: 0, x: 5120, y: 373 },
  ];
  const connected = !onboarding && !disconnected;
  const focusUnavailable = params.get('focus') === 'unavailable';
  const micLive = params.get('mic') === 'live';

  const state: AppState = {
    device: {
      id: thisId,
      name: isLaptop ? 'THORBEN-LAPTOP' : 'THORBEN-PC',
      version: '0.1.0',
      os: 'Windows 11 Pro',
      input_source: !isLaptop,
      headset_sink: !isLaptop,
      stream_source: isLaptop,
      onboarding_done: !onboarding,
      addresses: isLaptop ? ['192.168.178.42:47820'] : ['192.168.178.20:47820', '172.28.80.1:47820'],
    },
    connection: {
      state: connected ? 'connected' : onboarding ? 'idle' : 'reconnecting',
      peer: onboarding ? null : peer,
      rtt_us: connected ? 1100 : null,
      clock_offset_us: connected ? 42 : null,
      link: 'wired',
      since_unix_ms: connected ? Date.now() - 3 * 3600_000 - 17 * 60_000 : null,
      error: null,
      version_mismatch: false,
      peer_version: connected ? (has('mismatch') ? '0.0.9' : has('newer') ? '0.2.0' : '0.1.0') : null,
      peer_logon_screen: connected && has('logon'),
    },
    peers: onboarding ? [] : [peer],
    discovered: onboarding
      ? [
          {
            id: otherId,
            name: isLaptop ? 'THORBEN-PC' : 'Thorbens Laptop',
            address: isLaptop ? '192.168.178.20' : '192.168.178.42',
            version: '0.1.0',
            paired: false,
          },
          { id: 'x-9', name: 'Büro-Rechner', address: '192.168.178.77', version: '0.1.0', paired: false },
        ]
      : [],
    pairing: null,
    features: {
      input: { enabled: true, state: connected ? 'ready' : 'off', detail: null },
      laptop_audio: { ...emptyAudio(), state: connected ? 'live' : 'ready', latency_ms: 18.4, codec: 'pcm', buffer_ms: 10, device_name: 'Headset Game (USB-Dongle)' },
      mic: {
        ...emptyAudio(),
        enabled: micLive,
        state: micLive ? 'live' : 'unavailable',
        detail: micLive ? 'Laptop nutzt dein Mikro (Teams)' : 'Es gibt noch keinen signierten virtuellen Audiotreiber.',
        codec: micLive ? 'opus' : '',
        latency_ms: micLive ? 21 : null,
        device_name: 'Headset-Mikrofon (USB-Dongle)',
      },
      clipboard: { enabled: true, state: connected ? 'ready' : 'off', detail: null },
      focus: {
        enabled: true,
        state: focusUnavailable ? 'unavailable' : connected ? 'ready' : 'off',
        detail: null,
        phase: 'off',
        mode: 'full',
        width: 5120,
        height: 1440,
        fps: 120,
        bitrate_kbps: 120_000,
        codec: 'hevc',
        encoder: 'hevc_nvenc',
        stats: null,
        unavailable_reason: focusUnavailable
          ? 'Auf dem Laptop wurde kein Hardware-Encoder gefunden (NVENC, QuickSync oder AMF).'
          : null,
      },
    },
    screens: { screens, placements, revision: 1 },
    location: 'local',
    cursor_locked: false,
    warnings: onboarding
      ? []
      : [
          ...(has('nopublic') ? [] : [{ code: 'network_public', message: 'Dein Netzwerk ist als „Öffentlich“ eingestuft.' }]),
          ...(params.get('warn') ? [{ code: params.get('warn') ?? '', message: '' }] : []),
        ],
    transfers: onboarding
      ? []
      : [
          { id: 3, direction: 'send', name: 'Angebot_Kaiserstuhl.pdf', files: 1, total_bytes: 2_400_000, done_bytes: 2_400_000, status: 'done', path: 'C:\\Users\\thorben\\Documents\\Angebot_Kaiserstuhl.pdf', started_unix_ms: Date.now() - 12 * 60_000 },
          { id: 2, direction: 'receive', name: 'Screenshots (14 Dateien)', files: 14, total_bytes: 48_900_000, done_bytes: 48_900_000, status: 'done', path: 'C:\\Users\\thorben\\Downloads\\Desk Bridge\\Screenshots', started_unix_ms: Date.now() - 95 * 60_000 },
          { id: 1, direction: 'receive', name: 'backup-2026-10.zip', files: 1, total_bytes: 1_380_000_000, done_bytes: 410_000_000, status: 'failed', path: null, started_unix_ms: Date.now() - 26 * 3600_000 },
        ],
    update: {
      current: '0.1.0',
      available: has('update') ? '0.2.0' : null,
      checking: false,
      url: has('update') ? 'https://github.com/auerth/desk_bridge/releases' : null,
      error: null,
      last_checked_unix_ms: null,
      installer_available: !has('noinstaller'),
      partner_remote: true,
      partner_updating: null,
      updating_from_partner: false,
    },
    metrics: { rtt_history_us: connected ? Array.from({ length: 60 }, (_, i) => 900 + Math.round(Math.sin(i / 5) * 160 + ((i * 37) % 11) * 12)) : [], input_latency_p50_us: connected ? 480 : null, input_latency_p95_us: connected ? (params.get('p95') === 'high' ? 5200 : 1350) : null, input_latency_max_us: connected ? 3100 : null, audio_buffer_fill_ms: 9.6, audio_drift_ppm: 12, audio_loss_pct: 0, agent_cpu_pct: 0.4, agent_rss_mb: 42 },
    paused: false,
  };
  return state;
}

function seedConfig(state: AppState): Config {
  const c = defaultConfig();
  c.device = {
    name: state.device.name,
    input_source: state.device.input_source,
    headset_sink: state.device.headset_sink,
    stream_source: state.device.stream_source,
    onboarding_done: state.device.onboarding_done,
  };
  c.layout.placement = clone(state.screens.placements);
  c.layout.revision = 1;
  const lang = params.get('lang');
  if (lang === 'de' || lang === 'en') c.general.language = lang;
  const theme = params.get('theme');
  if (theme === 'dark' || theme === 'light') c.general.theme = theme;
  c.network.manual_hosts = ['192.168.178.42'];
  return c;
}

export function createMockBackend(): Backend {
  const state = seedState();
  const config = seedConfig(state);
  const stateSubs = new Set<(s: AppState) => void>();
  const eventSubs = new Set<(e: UiEvent) => void>();
  const connSubs = new Set<(c: ConnectionInfo) => void>();
  const osdSubs = new Set<(p: { icon: string; text: string }) => void>();
  let agentOnline = !has('offline');
  let nextTransfer = 10;
  let testToneUntil = 0;
  let tick = 0;
  let osdChannel: BroadcastChannel | null = null;
  try {
    osdChannel = new BroadcastChannel('db-mock-osd');
  } catch {
    osdChannel = null;
  }

  const pushState = () => {
    if (!agentOnline) return;
    const snapshot = clone(state);
    stateSubs.forEach((cb) => cb(snapshot));
  };
  const emitEvent = (e: UiEvent) => {
    if (!agentOnline) return;
    eventSubs.forEach((cb) => cb(clone(e)));
    if (e.kind === 'osd') {
      osdSubs.forEach((cb) => cb({ icon: e.icon, text: e.text }));
      osdChannel?.postMessage({ icon: e.icon, text: e.text });
    }
  };
  const setConn = (c: ConnectionInfo) => connSubs.forEach((cb) => cb(c));
  /** "Partner aktualisieren": transfer, the partner installs, comes back with `version`. */
  const simulatePartnerUpdate = (version: string) => {
    const name = state.connection.peer?.name ?? 'Partner';
    state.update.partner_updating = version;
    pushState();
    setTimeout(() => emitEvent({ kind: 'notification', level: 'info', text: `${name} installiert jetzt Version ${version}. Die Verbindung trennt sich kurz und kommt mit der neuen Version zurück.` }), 1500);
    setTimeout(() => {
      state.update.partner_updating = null;
      state.connection.peer_version = version;
      pushState();
      emitEvent({ kind: 'notification', level: 'info', text: `${name} läuft jetzt mit Version ${version}.` });
    }, 4500);
  };

  const connected = () => state.connection.state === 'connected';

  const applyConfigToState = () => {
    state.device.name = config.device.name || state.device.name;
    state.device.input_source = config.device.input_source;
    state.device.headset_sink = config.device.headset_sink;
    state.device.stream_source = config.device.stream_source;
    state.features.laptop_audio.muted = config.audio.laptop_to_pc.mute;
    state.features.mic.muted = config.audio.mic_to_laptop.mute;
    state.features.input.enabled = config.input.enabled;
    state.features.laptop_audio.enabled = config.audio.laptop_to_pc.enabled;
    state.features.clipboard.enabled = config.clipboard.send || config.clipboard.receive;
    const dev = RENDER.find((d) => d.id === config.audio.laptop_to_pc.output_device);
    state.features.laptop_audio.device_name = (dev ?? RENDER[0])?.name ?? null;
    if (state.features.mic.state !== 'unavailable') state.features.mic.enabled = config.audio.mic_to_laptop.enabled;
    state.features.focus.mode = config.focus.mode;
    state.screens.placements = clone(config.layout.placement.length ? config.layout.placement : state.screens.placements);
    syncFeatureStates();
  };

  const syncFeatureStates = () => {
    const f = state.features;
    const on = connected() && !state.paused;
    const live = (feat: { enabled: boolean; state: string }, isLive: boolean) => {
      if (feat.state === 'unavailable') return;
      feat.state = !feat.enabled ? 'off' : on ? (isLive ? 'live' : 'ready') : 'ready';
    };
    live(f.input, state.location === 'remote');
    live(f.laptop_audio, on && !f.laptop_audio.muted && f.laptop_audio.level > 0.02);
    if (f.mic.state !== 'unavailable') live(f.mic, on && !f.mic.muted && params.get('mic') === 'live');
    live(f.clipboard, false);
    if (f.focus.state !== 'unavailable') f.focus.state = !f.focus.enabled ? 'off' : f.focus.phase === 'streaming' ? 'live' : 'ready';
  };

  // ---- ticking simulation -------------------------------------------------------------------
  let smooth = 0.3;
  const timer = has('quiet')
    ? null
    : setInterval(() => {
        if (!agentOnline) return;
        tick++;
        const on = connected() && !state.paused;
        if (on) {
          const base = state.connection.link === 'wired' ? 1000 : 4500;
          const spike = Math.random() < 0.04 ? Math.random() * 1800 : 0;
          const rtt = Math.round(base + Math.sin(tick / 6) * 220 + Math.random() * 260 + spike);
          state.connection.rtt_us = rtt;
          state.metrics.rtt_history_us = [...state.metrics.rtt_history_us.slice(-59), rtt];
          // Audio level: smoothed noise bursts like music.
          const target = state.features.laptop_audio.muted ? 0 : 0.25 + Math.abs(Math.sin(tick / 3)) * 0.5 * Math.random() + (Date.now() < testToneUntil ? 0.35 : 0);
          smooth = smooth * 0.55 + target * 0.45;
          state.features.laptop_audio.level = Math.min(1, smooth);
          state.features.laptop_audio.latency_ms = 17 + Math.sin(tick / 9) * 1.5;
          if (params.get('mic') === 'live' && !state.features.mic.muted) {
            state.features.mic.level = Math.min(1, 0.15 + Math.abs(Math.sin(tick / 2)) * Math.random() * 0.6);
          } else {
            state.features.mic.level = 0;
          }
          if (state.features.focus.phase === 'streaming') {
            const f = state.features.focus;
            f.stats = {
              fps: Math.round(f.fps - Math.random() * 2),
              bitrate_kbps: Math.round(f.bitrate_kbps * (0.7 + Math.random() * 0.25)),
              loss_pct: Math.random() < 0.05 ? 0.1 : 0,
              encode_ms: 3.1 + Math.random() * 0.8,
              network_ms: 1.2 + Math.random() * 0.5,
              decode_ms: 2.4 + Math.random() * 0.6,
              total_ms: 12.5 + Math.random() * 2,
            };
          }
        } else {
          state.features.laptop_audio.level = 0;
          state.features.mic.level = 0;
        }
        // Transfers
        for (const t of state.transfers) {
          if (t.status === 'running') {
            t.done_bytes = Math.min(t.total_bytes, t.done_bytes + Math.max(1_000_000, t.total_bytes / 14));
            if (t.done_bytes >= t.total_bytes) t.status = 'done';
          }
        }
        // Pairing countdown / simulated peer
        if (state.pairing) {
          if (tick % 4 === 0) state.pairing.seconds_left = Math.max(0, state.pairing.seconds_left - 1);
          if (state.pairing.seconds_left <= 0) state.pairing = null;
        }
        syncFeatureStates();
        pushState();
      }, 250);
  void timer;

  // ---- request handling -----------------------------------------------------------------------
  const completePairing = (name: string, id: string) => {
    const peer: PeerDto = { id, name, online: true, fingerprint_short: '7F3A-91C2', last_ip: '192.168.178.42' };
    state.peers = [peer];
    state.pairing = null;
    state.connection = {
      ...state.connection,
      state: 'connected',
      peer,
      rtt_us: 1100,
      since_unix_ms: Date.now(),
      peer_version: '0.1.0',
    };
    state.discovered = state.discovered.map((d) => (d.id === id ? { ...d, paired: true } : d));
    state.metrics.rtt_history_us = Array.from({ length: 30 }, () => 1000 + Math.round(Math.random() * 300));
    state.warnings = state.warnings.filter((w) => w.code !== 'network_public');
    syncFeatureStates();
    emitEvent({ kind: 'pairing_result', ok: true, message: 'Gekoppelt', peer_name: name });
    pushState();
  };

  const setFocusMode = async (mode: string) => {
    const f = state.features.focus;
    if (f.state === 'unavailable') throw new Error(f.unavailable_reason ?? 'Fokusmodus nicht verfügbar');
    if (!connected()) throw new Error('Kein Partnergerät verbunden');
    let target = mode;
    if (mode === 'cycle') target = f.phase === 'off' ? 'full' : f.mode === 'full' ? 'split' : 'off';
    if (target === 'off') {
      if (f.phase === 'off') return;
      f.phase = 'stopping';
      pushState();
      await sleep(500);
      f.phase = 'off';
      f.stats = null;
      f.state = 'ready';
      emitEvent({ kind: 'osd', icon: 'monitor', text: 'Fokusmodus aus' });
      pushState();
      return;
    }
    f.mode = target;
    config.focus.mode = target as Config['focus']['mode'];
    f.phase = 'preparing';
    const pc = state.screens.screens.find((s) => s.device_id === PC_ID);
    const w = pc?.w ?? 5120;
    f.width = target === 'split' ? Math.round((w * config.focus.split_percent) / 100) : target === 'window' ? 1920 : w;
    f.height = target === 'window' ? 1080 : (pc?.h ?? 1440);
    f.fps = config.focus.fps || 120;
    pushState();
    await sleep(900);
    f.phase = 'streaming';
    f.state = 'live';
    emitEvent({ kind: 'osd', icon: 'focus', text: 'Fokusmodus: Laptop' });
    pushState();
  };

  async function handle(req: UiRequest): Promise<unknown> {
    await sleep(25 + Math.random() * 60);
    switch (req.type) {
      case 'get_state':
        return clone(state);
      case 'get_config':
        return clone(config);
      case 'set_config': {
        Object.assign(config, clone(req.config));
        config.layout.revision += 0;
        applyConfigToState();
        pushState();
        return clone(config);
      }
      case 'complete_onboarding':
        state.device.onboarding_done = true;
        state.device.name = req.name;
        config.device.onboarding_done = true;
        config.device.name = req.name;
        pushState();
        return null;
      case 'start_pairing': {
        const code = String(Math.floor(100000 + Math.random() * 900000));
        state.pairing = { code, seconds_left: 120 };
        pushState();
        // The "other device" finds us and enters the code a few seconds later.
        setTimeout(() => {
          if (state.pairing?.code === code && !has('quiet')) {
            completePairing(state.device.stream_source ? 'THORBEN-PC' : 'Thorbens Laptop', state.device.stream_source ? PC_ID : LAPTOP_ID);
          }
        }, 9000);
        return null;
      }
      case 'cancel_pairing':
        state.pairing = null;
        pushState();
        return null;
      case 'pair_with': {
        await sleep(700);
        if (req.code.replace(/\s/g, '') === '123456') {
          const d = state.discovered.find((x) => x.id === req.device_id);
          completePairing(d?.name ?? 'Thorbens Laptop', d?.id ?? LAPTOP_ID);
        } else {
          emitEvent({ kind: 'pairing_result', ok: false, message: 'Der Code stimmt nicht. Noch 2 Versuche.', peer_name: null });
        }
        return null;
      }
      case 'unpair':
        state.peers = state.peers.filter((p) => p.id !== req.device_id);
        state.connection = { ...state.connection, state: 'idle', peer: null, rtt_us: null, since_unix_ms: null };
        syncFeatureStates();
        pushState();
        return null;
      case 'rename_peer':
        state.peers = state.peers.map((p) => (p.id === req.device_id ? { ...p, name: req.name } : p));
        if (state.connection.peer?.id === req.device_id) state.connection.peer = { ...state.connection.peer, name: req.name };
        pushState();
        return null;
      case 'add_manual_host':
        if (!config.network.manual_hosts.includes(req.host)) config.network.manual_hosts.push(req.host);
        return null;
      case 'set_feature': {
        const f = state.features;
        switch (req.feature) {
          case 'input': config.input.enabled = req.enabled; break;
          case 'laptop_audio': config.audio.laptop_to_pc.enabled = req.enabled; break;
          case 'mic': config.audio.mic_to_laptop.enabled = req.enabled; break;
          case 'clipboard': config.clipboard.send = req.enabled; config.clipboard.receive = req.enabled; break;
          case 'focus': f.focus.enabled = req.enabled; break;
          case 'mute_laptop_audio': config.audio.laptop_to_pc.mute = req.enabled; break;
          case 'mute_mic': config.audio.mic_to_laptop.mute = req.enabled; break;
        }
        applyConfigToState();
        if (req.feature === 'mute_mic') emitEvent({ kind: 'osd', icon: req.enabled ? 'mic-off' : 'mic', text: req.enabled ? 'Mikro stumm' : 'Mikro aktiv' });
        if (req.feature === 'mute_laptop_audio') emitEvent({ kind: 'osd', icon: req.enabled ? 'volume-off' : 'volume', text: req.enabled ? 'Laptop-Ton stumm' : 'Laptop-Ton an' });
        pushState();
        return null;
      }
      case 'set_layout':
        state.screens.placements = clone(req.placements);
        state.screens.revision += 1;
        config.layout.placement = clone(req.placements);
        config.layout.revision = state.screens.revision;
        pushState();
        return null;
      case 'set_focus':
        await setFocusMode(req.mode);
        return null;
      case 'jump_to_other_device':
        state.location = state.location === 'local' ? 'remote' : 'local';
        emitEvent({ kind: 'osd', icon: state.location === 'remote' ? 'laptop' : 'monitor', text: state.location === 'remote' ? '→ Laptop' : '→ PC' });
        syncFeatureStates();
        pushState();
        return null;
      case 'set_cursor_lock':
        state.cursor_locked = req.locked;
        emitEvent({ kind: 'osd', icon: 'lock', text: req.locked ? 'Cursor gesperrt' : 'Cursor frei' });
        pushState();
        return null;
      case 'list_audio_devices':
        setTimeout(() => emitEvent({ kind: 'audio_devices', render: clone(RENDER), capture: clone(CAPTURE) }), 90);
        return null;
      case 'play_test_tone':
        testToneUntil = Date.now() + 1400;
        return null;
      case 'send_files': {
        const names = req.paths.map((p) => p.split(/[\\/]/).pop() ?? p);
        const t: TransferDto = {
          id: nextTransfer++,
          direction: 'send',
          name: names.length === 1 ? (names[0] ?? 'Datei') : `${names[0] ?? 'Datei'} und ${names.length - 1} weitere`,
          files: names.length,
          total_bytes: 180_000_000 * names.length,
          done_bytes: 0,
          status: 'running',
          path: req.paths[0] ?? null,
          started_unix_ms: Date.now(),
        };
        state.transfers = [t, ...state.transfers].slice(0, 50);
        emitEvent({ kind: 'osd', icon: 'file', text: `${names.length} ${names.length === 1 ? 'Datei' : 'Dateien'} werden gesendet` });
        pushState();
        return null;
      }
      case 'open_incoming_folder':
      case 'show_transfer_in_folder':
        return null;
      case 'clear_transfer_history':
        state.transfers = state.transfers.filter((t) => t.status === 'running');
        pushState();
        return null;
      case 'fetch_peer_logs':
        await sleep(700);
        emitEvent({
          kind: 'peer_logs',
          peer: 'Laptop',
          folder: String.raw`C:\Users\thorben\AppData\Local\softwelop\DeskBridge\peer-logs\Laptop-1760942100`,
          files: [
            { name: 'system-info.txt', size: 412, truncated: false },
            { name: 'agent.log.2026-10-20', size: 1_204_331, truncated: true },
            { name: 'service/service.log.2026-10-20', size: 8_120, truncated: false },
          ],
          error: null,
        });
        return { started: true };
      case 'get_peer_log': {
        const now = Date.now();
        const content =
          req.name === 'system-info.txt'
            ? ['desk bridge 0.1.2', 'os: Windows', 'device: Laptop', 'connection: Connected'].join('\n')
            : Array.from({ length: 400 }, (_, i) => {
                const lvl = i % 37 === 0 ? 'ERROR' : i % 11 === 0 ? 'WARN' : 'INFO';
                return `${new Date(now - (400 - i) * 1000).toISOString()}  ${lvl} deskbridge_core::engine: mock line ${i}`;
              }).join('\n');
        return { name: req.name, content, truncated: req.name.startsWith('agent'), size: content.length };
      }
      case 'export_diagnostics':
        await sleep(600);
        emitEvent({ kind: 'diagnostics_exported', path: 'C:\\Users\\thorben\\Desktop\\DeskBridge-Diagnose-2026-10-16.zip' });
        return null;
      case 'check_for_update':
        state.update.checking = true;
        pushState();
        await sleep(1200);
        state.update.checking = false;
        state.update.last_checked_unix_ms = Date.now();
        pushState();
        return null;
      case 'offer_partner_update':
        if (req.install) {
          simulatePartnerUpdate(req.version);
          return { offered: true, confirm_on_partner: false };
        }
        emitEvent({ kind: 'notification', level: 'info', text: `Update ${req.version} wird an das Partnergerät gesendet.` });
        return null;
      case 'update_partner':
        if (!state.update.installer_available) throw new Error(`Für Version ${state.device.version} liegt kein Installer vor.`);
        simulatePartnerUpdate(state.device.version);
        return { offered: true, confirm_on_partner: false };
      case 'request_partner_update': {
        const v = state.connection.peer_version ?? '';
        state.update.updating_from_partner = true;
        pushState();
        emitEvent({ kind: 'notification', level: 'info', text: `${state.connection.peer?.name ?? 'Partner'} aktualisiert Desk Bridge auf Version ${v}. Desk Bridge startet danach neu.` });
        setTimeout(() => {
          state.update.updating_from_partner = false;
          state.device.version = v;
          state.update.current = v;
          pushState();
        }, 4000);
        return { requested: true };
      }
      case 'answer_partner_update':
        if (req.accept) {
          setTimeout(() => emitEvent({ kind: 'partner_update_ready', path: 'C:\Users\thorben\AppData\Local\softwelop\DeskBridge\Updates\DeskBridge-Setup-0.2.0.exe', version: '0.2.0' }), 1500);
        }
        return null;
      case 'make_network_private':
        state.warnings = state.warnings.filter((w) => w.code !== 'network_public');
        emitEvent({ kind: 'notification', level: 'info', text: 'Netzwerk ist jetzt als „Privat“ markiert.' });
        pushState();
        return null;
      case 'set_paused':
        state.paused = req.paused;
        state.connection.state = req.paused ? 'paused' : 'connected';
        syncFeatureStates();
        pushState();
        return null;
      case 'restart_agent':
      case 'shutdown_agent':
        agentOnline = false;
        setConn({ connected: false, reason: 'disconnected' });
        if (req.type === 'restart_agent') {
          if (req.elevated) state.warnings = state.warnings.filter((w) => w.code !== 'elevation');
          setTimeout(() => {
            agentOnline = true;
            setConn({ connected: true });
            pushState();
          }, 2500);
        }
        return null;
      case 'ping':
        return null;
      default:
        return null;
    }
  }

  const noop = () => {};

  const fileDropListeners = new Set<(e: FileDropEvent) => void>();
  if (typeof document !== 'undefined') {
    const emit = (e: FileDropEvent) => fileDropListeners.forEach((cb) => cb(e));
    document.addEventListener('dragenter', (e) => { e.preventDefault(); emit({ type: 'enter' }); });
    document.addEventListener('dragover', (e) => { e.preventDefault(); emit({ type: 'over' }); });
    document.addEventListener('dragleave', (e) => { if (!e.relatedTarget) emit({ type: 'leave' }); });
    document.addEventListener('drop', (e) => {
      e.preventDefault();
      const files = Array.from(e.dataTransfer?.files ?? []).map((f) => f.name);
      emit(files.length ? { type: 'drop', paths: files } : { type: 'leave' });
    });
  }

  const sub = <T,>(set: Set<T>, cb: T): Unlisten => {
    set.add(cb);
    return () => set.delete(cb);
  };

  let offerScheduled = false;

  if (!agentOnline) {
    // Report offline shortly after startup, like the real backend would.
    setTimeout(() => setConn({ connected: false, reason: 'not_running', starting: true }), 50);
    setTimeout(() => setConn({ connected: false, reason: 'not_running', starting: false, start_failed: true }), 4000);
  }

  return {
    kind: 'mock',
    request: async <T,>(req: UiRequest) => {
      if (!agentOnline) throw new Error('agent_offline');
      try {
        return (await handle(req)) as T;
      } catch (e) {
        throw e instanceof Error ? e.message : String(e);
      }
    },
    onState: (cb) => sub(stateSubs, cb),
    onEvent: (cb) => {
      // Scenario `?partner=offer`: the partner offers an update shortly after the UI is listening.
      if (params.get('partner') === 'offer' && !offerScheduled) {
        offerScheduled = true;
        setTimeout(() => emitEvent({ kind: 'partner_update_offered', version: '0.2.0', size: 38_400_000, from: 'Thorbens Laptop' }), 1500);
      }
      return sub(eventSubs, cb);
    },
    onConnection: (cb) => {
      const un = sub(connSubs, cb);
      // The real backend only emits on change; give subscribers the initial status.
      setTimeout(() => cb({ connected: agentOnline }), 0);
      return un;
    },
    appInfo: async (): Promise<AppInfo> => ({ version: '0.1.0', mica: false, start_hidden: false }),
    frontendReady: noop,
    window: {
      minimize: noop,
      toggleMaximize: noop,
      close: noop,
      isMaximized: async () => false,
      onMaximizeChange: () => noop,
    },
    pickFolder: async () => 'C:\\Users\\thorben\\Downloads\\Desk Bridge',
    pickFiles: async () => ['C:\\Users\\thorben\\Documents\\Angebot_Kaiserstuhl.pdf', 'C:\\Users\\thorben\\Pictures\\Skizze.png'],
    pickInstaller: async () => 'C:\Users\thorben\Downloads\DeskBridge-Setup-0.2.0.exe',
    launchInstaller: async () => {},
    onFileDrop: (cb) => sub(fileDropListeners, cb),
    openUrl: async (url) => {
      window.open(url, '_blank', 'noopener');
    },
    openLogsFolder: async () => {},
    revealPath: async () => {},
    checkUpdate: async (): Promise<UpdateInfo | null> => {
      await sleep(1200);
      return has('update') ? { version: '0.2.0', notes: 'Bessere Reconnect-Zeiten und ein neues Tray-Flyout.' } : null;
    },
    installUpdate: async () => {
      await sleep(1500);
    },
    setWindowTheme: noop,
    openMain: noop,
    hideFlyout: noop,
    onOsd: (cb) => {
      const un = sub(osdSubs, cb);
      const ch = osdChannel;
      const handler = (m: MessageEvent) => cb(m.data as { icon: string; text: string });
      ch?.addEventListener('message', handler);
      return () => {
        un();
        ch?.removeEventListener('message', handler);
      };
    },
    onFlyoutOpened: () => noop,
  };
}
