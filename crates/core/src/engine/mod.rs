//! The engine: one actor task that owns all mutable state. Everything else (network I/O tasks,
//! capture forwarders, IPC connections, timers) only sends [`Msg`]s to it.

mod audio;
mod clip;
mod connect;
mod dto;
mod focus;
mod features;
mod input;
mod pairing;
pub mod session;
mod transfers;
mod update;
mod ui;

use crate::backoff::Backoff;
use crate::clock::{ClockSync, now_us};
use crate::config::Config;
use crate::ipc::{AppState, ConnState, Push, UiEvent, UiRequest, WarningDto};
use crate::paths::AppPaths;
use crate::platform::Platform;
use deskbridge_input::layout::Layout;
use deskbridge_input::receiver::Receiver;
use deskbridge_input::router::{Router, RouterConfig};
use deskbridge_net::discovery::{Discovered, Discovery, DiscoveryEvent};
use deskbridge_net::identity::Identity;
use deskbridge_net::pairing::PairingWindow;
use deskbridge_net::peers::PeerStore;
use deskbridge_net::transport::{Accepted, Transport};
use deskbridge_platform::clipboard::ClipboardEvent;
use deskbridge_platform::input::CaptureEvent;
use deskbridge_platform::power::PowerEvent;
use deskbridge_platform::screen::ScreenInfo;
use deskbridge_proto::pb;
use serde_json::Value;
use session::PeerLink;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, oneshot};
use uuid::Uuid;

pub use session::PeerLink as Link;

/// Receivers the datagram router feeds: (speaker, microphone).
pub(crate) type AudioSlots = Arc<parking_lot::RwLock<(Option<Arc<deskbridge_audio::AudioReceiver>>, Option<Arc<deskbridge_audio::AudioReceiver>>)>>;

/// Messages processed by the actor.
pub(crate) enum Msg {
    Ui { req: UiRequest, reply: oneshot::Sender<Result<Value, String>> },
    Discovery(DiscoveryEvent),
    Accepted(Accepted),
    SessionReady(PeerLink),
    ConnectFailed { peer: Uuid, error: String },
    SessionClosed { session_id: u64, reason: String },
    PeerControl { session_id: u64, msg: pb::control::Msg },
    PeerInput { session_id: u64, ev: pb::InputEvent },
    Capture(CaptureEvent),
    Clipboard(ClipboardEvent),
    Power(PowerEvent),
    ScreensChanged,
    PairingFinished { ok: bool, message: String, peer: Option<deskbridge_net::pairing::PairedDevice>, addr: Option<SocketAddr> },
    Warn { code: String, message: String },
    MicUsage(deskbridge_platform::audio::MicUsage),
    ToneDone,
    FocusSourceUp {
        screen_id: u32,
        capturer: Box<dyn deskbridge_platform::video::ScreenCapturer>,
        encoder: Box<dyn deskbridge_platform::video::VideoEncoder>,
        cfg: deskbridge_platform::video::EncoderConfig,
        link: PeerLink,
    },
    FocusSourceFailed { reason: String },
    AudioDevice(deskbridge_platform::audio::DeviceEvent),
    Tick,
    Shutdown,
}

pub struct EngineParams {
    pub paths: AppPaths,
    pub config: Config,
    pub platform: Platform,
    /// UDP bind address; port 0 = ephemeral (tests).
    pub bind: SocketAddr,
    pub enable_discovery: bool,
    /// Reuse an identity (tests); `None` loads or creates `identity.bin`.
    pub identity: Option<Identity>,
}

/// Handle used by the IPC server and tests.
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::UnboundedSender<Msg>,
    pushes: broadcast::Sender<Push>,
    local_addr: SocketAddr,
    device_id: Uuid,
    shutdown: Arc<tokio::sync::Notify>,
}

impl EngineHandle {
    pub async fn request(&self, req: UiRequest) -> Result<Value, String> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Msg::Ui { req, reply }).map_err(|_| "engine stopped".to_string())?;
        rx.await.map_err(|_| "engine stopped".to_string())?
    }

    /// Convenience: current state snapshot.
    pub async fn state(&self) -> AppState {
        match self.request(UiRequest::GetState).await {
            Ok(v) => serde_json::from_value(v).unwrap_or_default(),
            Err(_) => AppState::default(),
        }
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Push> {
        self.pushes.subscribe()
    }

    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    #[must_use]
    pub fn device_id(&self) -> Uuid {
        self.device_id
    }

    /// Resolves when the engine asked to shut the process down (`ShutdownAgent`).
    pub async fn wait_shutdown(&self) {
        self.shutdown.notified().await;
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Msg::Shutdown);
    }

    /// Shows a persistent warning banner in the UI.
    pub fn warn(&self, code: &str, message: &str) {
        let _ = self.tx.send(Msg::Warn { code: code.into(), message: message.into() });
    }
}

pub(crate) struct ActiveSession {
    pub link: PeerLink,
    pub since_unix_ms: u64,
    pub remote_screens: Vec<ScreenInfo>,
    pub last_pong: Instant,
    pub initiated_by_preferred: bool,
}

pub(crate) struct DiscoveredEntry {
    pub d: Discovered,
    pub last_seen: Instant,
}

pub(crate) struct Engine {
    pub cfg: Config,
    pub paths: AppPaths,
    pub platform: Platform,
    pub identity: Arc<Identity>,
    pub transport: Arc<Transport>,
    pub peers: PeerStore,
    pub discovery: Option<Discovery>,
    pub discovered: HashMap<Uuid, DiscoveredEntry>,
    pub msg_tx: mpsc::UnboundedSender<Msg>,
    pub pushes: broadcast::Sender<Push>,
    pub shutdown: Arc<tokio::sync::Notify>,
    pub hello: Arc<parking_lot::RwLock<pb::Hello>>,
    pub session_ids: Arc<AtomicU64>,

    pub session: Option<ActiveSession>,
    pub conn_state: ConnState,
    pub connecting: Option<Uuid>,
    pub had_session: bool,
    pub backoff: Backoff,
    pub next_attempt: Instant,
    pub grace_applied: bool,
    pub last_error: Option<String>,
    pub version_mismatch: bool,

    pub clock: ClockSync,
    pub rtt_history: VecDeque<u32>,
    pub input_latency: VecDeque<u32>,
    pub procstats: crate::procstats::ProcStats,
    pub proc_cpu: f32,
    pub proc_rss: f32,
    pub tick_count: u64,

    pub pairing: Option<Arc<PairingWindow>>,
    pub local_screens: Vec<ScreenInfo>,
    pub layout: Layout,
    pub peer_placements: Vec<pb::Placement>,
    pub peer_layout_revision: u64,
    pub router: Router,
    pub receiver: Option<Receiver>,
    pub capture_running: bool,
    pub(crate) clip: Option<clip::ClipRuntime>,
    pub(crate) audio: audio::AudioState,
    pub(crate) focus: focus::FocusRt,
    pub(crate) cursor_shape: Option<deskbridge_platform::video::CursorShapeData>,
    pub(crate) cursor_shape_serial: u32,
    pub(crate) audio_rx_slots: Option<AudioSlots>,
    pub(crate) xfer: transfers::Transfers,
    pub(crate) update_out: Option<update::OutgoingUpdate>,
    pub(crate) update_in: Option<pb::UpdateOffer>,
    pub cursor_locked: bool,
    pub warnings: Vec<WarningDto>,

    pub dirty: bool,
    pub last_publish: Instant,
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Starts the engine. Returns once the QUIC endpoint is bound.
pub async fn start(params: EngineParams) -> anyhow::Result<EngineHandle> {
    let EngineParams { paths, mut config, platform, bind, enable_discovery, identity } = params;
    paths.ensure()?;
    config.sanitize();
    if config.device.name.is_empty() {
        config.device.name = default_device_name();
    }
    let identity = Arc::new(match identity {
        Some(i) => i,
        None => Identity::load_or_create(&paths.identity_file(), platform.secrets.as_ref())?,
    });
    // A crashed focus session on this device must not leave the displays rearranged (plan 8.7).
    {
        let (tp, dm) = (paths.topology_file(), platform.display.clone());
        match tokio::task::spawn_blocking(move || deskbridge_display::recover_on_startup(&tp, &dm)).await {
            Ok(Ok(outcome)) => tracing::info!(?outcome, "display recovery check"),
            Ok(Err(e)) => tracing::warn!("display recovery failed: {e}"),
            Err(e) => tracing::warn!("display recovery task failed: {e}"),
        }
    }
    let peers = PeerStore::load(paths.peers_file())?;
    let transport = Arc::new(Transport::bind(identity.clone(), peers.clone(), bind)?);
    let local_addr = transport.local_addr()?;
    tracing::info!(%local_addr, device = %identity.device_id, "engine bound");

    let (msg_tx, msg_rx) = mpsc::unbounded_channel();
    let (pushes, _) = broadcast::channel(64);
    let shutdown = Arc::new(tokio::sync::Notify::new());

    let own_id = identity.device_id;
    let local_screens = platform.screens.screens();
    let router = Router::new(own_id, router_config(&config));

    let mut discovery = None;
    if enable_discovery {
        match Discovery::new(own_id) {
            Ok(mut d) => {
                if let Err(e) = d.announce(&config.device.name, local_addr.port(), crate::brand::VERSION) {
                    tracing::warn!("mDNS announce failed: {e}");
                }
                let (dtx, mut drx) = mpsc::unbounded_channel();
                if let Err(e) = d.browse(dtx) {
                    tracing::warn!("mDNS browse failed: {e}");
                }
                let tx = msg_tx.clone();
                tokio::spawn(async move {
                    while let Some(ev) = drx.recv().await {
                        if tx.send(Msg::Discovery(ev)).is_err() {
                            break;
                        }
                    }
                });
                discovery = Some(d);
            }
            Err(e) => tracing::warn!("mDNS unavailable: {e}"),
        }
    }

    let paths_data_dir = paths.data_dir.clone();
    let mut engine = Engine {
        cfg: config,
        paths,
        platform: platform.clone(),
        identity: identity.clone(),
        transport: transport.clone(),
        peers,
        discovery,
        discovered: HashMap::new(),
        msg_tx: msg_tx.clone(),
        pushes: pushes.clone(),
        shutdown: shutdown.clone(),
        hello: Arc::new(parking_lot::RwLock::new(pb::Hello::default())),
        session_ids: Arc::new(AtomicU64::new(1)),
        session: None,
        conn_state: ConnState::Idle,
        connecting: None,
        had_session: false,
        backoff: Backoff::default(),
        next_attempt: Instant::now(),
        grace_applied: false,
        last_error: None,
        version_mismatch: false,
        clock: ClockSync::default(),
        rtt_history: VecDeque::new(),
        input_latency: VecDeque::new(),
        procstats: crate::procstats::ProcStats::default(),
        proc_cpu: 0.0,
        proc_rss: 0.0,
        tick_count: 0,
        pairing: None,
        local_screens,
        layout: Layout::default(),
        peer_placements: Vec::new(),
        peer_layout_revision: 0,
        router,
        receiver: None,
        capture_running: false,
        clip: None,
        audio: audio::AudioState::default(),
        focus: focus::FocusRt::default(),
        cursor_shape: None,
        cursor_shape_serial: 0,
        audio_rx_slots: None,
        xfer: transfers::Transfers::new(Some(paths_data_dir)),
        update_out: None,
        update_in: None,
        cursor_locked: false,
        warnings: Vec::new(),
        dirty: true,
        last_publish: Instant::now(),
    };
    engine.refresh_hello();
    engine.router.set_screens(engine.local_screens.clone());
    engine.router.set_config(router_config(&engine.cfg));

    // accept loop
    {
        let t = transport;
        let tx = msg_tx.clone();
        tokio::spawn(async move {
            while let Some(p) = t.accept().await {
                let tx = tx.clone();
                tokio::spawn(async move {
                    match p.finish().await {
                        Ok(a) => {
                            let _ = tx.send(Msg::Accepted(a));
                        }
                        Err(e) => tracing::debug!("incoming connection rejected: {e}"),
                    }
                });
            }
        });
    }
    // platform watchers
    {
        let (tx, mut rx) = mpsc::unbounded_channel();
        if platform.screens.watch(tx).is_ok() {
            let m = msg_tx.clone();
            tokio::spawn(async move {
                while rx.recv().await.is_some() {
                    if m.send(Msg::ScreensChanged).is_err() {
                        break;
                    }
                }
            });
        }
        let (atx, mut arx) = mpsc::unbounded_channel();
        if platform.audio_devices.watch(atx).is_ok() {
            let m = msg_tx.clone();
            tokio::spawn(async move {
                while let Some(ev) = arx.recv().await {
                    if m.send(Msg::AudioDevice(ev)).is_err() {
                        break;
                    }
                }
            });
        }
        let (ptx, mut prx) = mpsc::unbounded_channel();
        if platform.power.watch(ptx).is_ok() {
            let m = msg_tx.clone();
            tokio::spawn(async move {
                while let Some(ev) = prx.recv().await {
                    if m.send(Msg::Power(ev)).is_err() {
                        break;
                    }
                }
            });
        }
    }
    // ticker
    {
        let tx = msg_tx.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(250));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                iv.tick().await;
                if tx.send(Msg::Tick).is_err() {
                    break;
                }
            }
        });
    }

    let handle = EngineHandle { tx: msg_tx, pushes, local_addr, device_id: own_id, shutdown };
    tokio::spawn(engine.run(msg_rx));
    Ok(handle)
}

fn default_device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "Desk Bridge".to_string())
}

pub(crate) fn router_config(cfg: &Config) -> RouterConfig {
    use deskbridge_platform::hotkey::{ALT, CTRL, SHIFT};
    RouterConfig {
        switch_delay: Duration::from_millis(u64::from(cfg.input.switch_delay_ms)),
        require_modifiers: match cfg.input.require_modifier.as_str() {
            "ctrl" => CTRL,
            "alt" => ALT,
            "shift" => SHIFT,
            _ => 0,
        },
        corner_margin_px: cfg.input.corner_margin_px,
        double_bump: cfg.input.double_bump,
    }
}

impl Engine {
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Msg>) {
        while let Some(msg) = rx.recv().await {
            if matches!(msg, Msg::Shutdown) {
                break;
            }
            self.handle(msg);
            // Coalesced state publishing.
            if self.dirty && self.last_publish.elapsed() >= Duration::from_millis(50) {
                self.publish_state();
            }
        }
        self.on_shutdown();
    }

    fn on_shutdown(&mut self) {
        self.close_session("shutdown");
        self.focus_shutdown();
        self.platform.capture.stop();
        self.transport.close();
        if let Some(d) = &self.discovery {
            d.shutdown();
        }
        self.shutdown.notify_waiters();
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Ui { req, reply } => {
                let r = self.handle_ui(req);
                let _ = reply.send(r);
                self.dirty = true;
            }
            Msg::Discovery(ev) => self.on_discovery(ev),
            Msg::Accepted(a) => self.on_accepted(a),
            Msg::SessionReady(link) => self.on_session_ready(link),
            Msg::ConnectFailed { peer, error } => self.on_connect_failed(peer, &error),
            Msg::SessionClosed { session_id, reason } => self.on_session_closed(session_id, &reason),
            Msg::PeerControl { session_id, msg } => self.on_peer_control(session_id, msg),
            Msg::PeerInput { session_id, ev } => self.on_peer_input(session_id, ev),
            Msg::Capture(ev) => self.on_capture(ev),
            Msg::Clipboard(ev) => self.on_clipboard(ev),
            Msg::Power(ev) => self.on_power(ev),
            Msg::ScreensChanged => self.on_screens_changed(),
            Msg::PairingFinished { ok, message, peer, addr } => self.on_pairing_finished(ok, message, peer, addr),
            Msg::Tick => self.on_tick(),
            Msg::Warn { code, message } => self.warn_once(&code, &message),
            Msg::MicUsage(u) => self.on_mic_usage(u),
            Msg::ToneDone => self.on_tone_done(),
            Msg::FocusSourceUp { screen_id, capturer, encoder, cfg, link } => self.on_focus_source_up(screen_id, capturer, encoder, cfg, link),
            Msg::FocusSourceFailed { reason } => self.on_focus_source_failed(&reason),
            Msg::AudioDevice(ev) => self.on_audio_device_event(ev),
            Msg::Shutdown => {}
        }
    }

    // ---------------------------------------------------------------------------------------
    // helpers
    // ---------------------------------------------------------------------------------------

    pub(crate) fn emit(&self, event: UiEvent) {
        let _ = self.pushes.send(Push::Event { event });
    }

    pub(crate) fn warn_once(&mut self, code: &str, message: &str) {
        if !self.warnings.iter().any(|w| w.code == code) {
            self.warnings.push(WarningDto { code: code.into(), message: message.into() });
            self.dirty = true;
        }
    }

    #[allow(dead_code)]
    pub(crate) fn clear_warning(&mut self, code: &str) {
        let before = self.warnings.len();
        self.warnings.retain(|w| w.code != code);
        if self.warnings.len() != before {
            self.dirty = true;
        }
    }

    pub(crate) fn publish_state(&mut self) {
        let state = self.build_state();
        let _ = self.pushes.send(Push::State { state: Box::new(state) });
        self.dirty = false;
        self.last_publish = Instant::now();
    }

    pub(crate) fn next_session_id(&self) -> u64 {
        self.session_ids.fetch_add(1, Ordering::Relaxed)
    }

    /// Rebuilds the `Hello` we send to peers (screens, roles, capabilities).
    pub(crate) fn refresh_hello(&mut self) {
        use deskbridge_proto::caps;
        let d = &self.cfg.device;
        let mut c = vec![
            caps::INPUT,
            caps::CLIPBOARD_TEXT,
            caps::CLIPBOARD_RICH,
            caps::FILES,
            caps::AUDIO_PCM,
            caps::AUDIO_OPUS,
            caps::UPDATE,
        ];
        if d.input_source {
            c.push("role.input_source");
            c.push(caps::FOCUS_VIEWER);
        }
        if d.headset_sink {
            c.push(caps::AUDIO_HEADSET);
        }
        if d.stream_source {
            c.push(caps::FOCUS_SOURCE);
            if self.platform.display.virtual_available() {
                c.push("focus.vdisplay");
            }
            c.push(caps::AUDIO_SPEAKER_SOURCE);
            if self.platform.virtual_audio.available() {
                c.push(caps::AUDIO_MIC_SINK);
            }
        }
        let hello = pb::Hello {
            device_id: self.identity.device_id.to_string(),
            name: d.name.clone(),
            os: self.platform.os.clone(),
            app_version: crate::brand::VERSION.to_string(),
            proto_version: deskbridge_proto::PROTO_VERSION,
            caps: c.into_iter().map(|n| pb::Capability { name: n.to_string(), version: 1 }).collect(),
            screens: self.local_screens.iter().map(screen_to_pb).collect(),
            video: Some(focus::hello_video_caps(self.platform.video.as_ref(), d.stream_source, d.input_source)),
        };
        *self.hello.write() = hello;
    }

    pub(crate) fn own_id(&self) -> Uuid {
        self.identity.device_id
    }

    pub(crate) fn persist_config(&mut self) {
        if let Err(e) = self.cfg.save(&self.paths.config_file()) {
            tracing::error!("saving config failed: {e}");
        }
    }

    // ---------------------------------------------------------------------------------------
    // events
    // ---------------------------------------------------------------------------------------

    fn on_discovery(&mut self, ev: DiscoveryEvent) {
        match ev {
            DiscoveryEvent::Found(d) => {
                // Remember the freshest address of paired peers.
                if self.peers.by_id(d.device_id).is_some() {
                    if let Some(ip) = d.addrs.iter().find(|a| a.is_ipv4()).or(d.addrs.first()) {
                        let _ = self.peers.update_address(d.device_id, ip.to_string(), d.port);
                    }
                    if !deskbridge_proto::compatible(d.proto, deskbridge_proto::PROTO_VERSION) && d.proto != 0 {
                        self.version_mismatch = true;
                    }
                    // Seen again: try right away.
                    if self.session.is_none() {
                        self.next_attempt = Instant::now();
                    }
                }
                self.discovered.insert(d.device_id, DiscoveredEntry { d, last_seen: Instant::now() });
            }
            DiscoveryEvent::Lost { device_id } => {
                self.discovered.remove(&device_id);
            }
        }
        self.dirty = true;
    }

    fn on_power(&mut self, ev: PowerEvent) {
        tracing::info!(?ev, "power event");
        match ev {
            PowerEvent::Suspend => {
                self.close_session("suspend");
            }
            PowerEvent::Resume | PowerEvent::NetworkChanged => {
                if let Err(e) = self.transport.rebind() {
                    tracing::debug!("rebind: {e}");
                }
                self.backoff.reset();
                self.next_attempt = Instant::now();
                if let Some(d) = self.discovery.as_mut() {
                    let _ = d.announce(&self.cfg.device.name, self.transport.local_addr().map_or(0, |a| a.port()), crate::brand::VERSION);
                }
            }
            PowerEvent::LidClosed | PowerEvent::LidOpened => {
                self.on_lid(matches!(ev, PowerEvent::LidClosed));
            }
        }
        self.dirty = true;
    }

    fn on_screens_changed(&mut self) {
        self.local_screens = self.platform.screens.screens();
        self.router.set_screens(self.local_screens.clone());
        self.refresh_hello();
        if let Some(s) = &self.session {
            s.link.send_control(pb::control::Msg::ScreensUpdate(pb::ScreensUpdate {
                screens: self.local_screens.iter().map(screen_to_pb).collect(),
            }));
        }
        self.rebuild_layout(false);
        self.dirty = true;
    }

    fn on_tick(&mut self) {
        self.tick_count += 1;
        let now = Instant::now();

        // pairing window expiry
        if let Some(w) = &self.pairing {
            if w.seconds_left() == 0 {
                self.pairing = None;
                self.dirty = true;
            } else if self.tick_count % 4 == 0 {
                self.dirty = true; // countdown
            }
        }
        // stale discovery entries
        let before = self.discovered.len();
        self.discovered.retain(|_, e| now.duration_since(e.last_seen) < Duration::from_secs(120));
        if self.discovered.len() != before {
            self.dirty = true;
        }

        if let Some(s) = &mut self.session {
            // heartbeat every 500 ms
            if self.tick_count % 2 == 0 {
                s.link.send_control(pb::control::Msg::Ping(pb::Ping { t0_us: now_us() }));
            }
            let silent = now.duration_since(s.last_pong);
            let new_state = if silent > Duration::from_secs(6) {
                None
            } else if silent > Duration::from_secs(3) {
                Some(ConnState::Degraded)
            } else {
                Some(ConnState::Connected)
            };
            match new_state {
                None => {
                    tracing::warn!("no heartbeat for 6 s, dropping session");
                    self.close_session("heartbeat timeout");
                }
                Some(st) => {
                    if self.conn_state != st {
                        self.conn_state = st;
                        self.dirty = true;
                    }
                }
            }
        } else {
            self.maybe_connect();
        }

        if self.tick_count % 8 == 0 {
            let (cpu, rss) = self.procstats.sample();
            self.proc_cpu = cpu;
            self.proc_rss = rss;
        }
        self.tick_focus();
        self.update_idle_state();
        self.tick_input();
        if self.has_running_transfers() || self.audio_is_active() || self.focus_is_active() {
            self.dirty = true;
        }
        if self.dirty && self.last_publish.elapsed() >= Duration::from_millis(100)
            || self.last_publish.elapsed() >= Duration::from_secs(1)
        {
            self.publish_state();
        }
    }

    /// Chooses the idle-ish connection state while no session exists.
    fn update_idle_state(&mut self) {
        if self.session.is_some() {
            return;
        }
        let st = if self.cfg.general.paused {
            ConnState::Paused
        } else if self.peers.all().is_empty() {
            ConnState::Idle
        } else if self.connecting.is_some() {
            ConnState::Connecting
        } else if self.had_session {
            ConnState::Reconnecting
        } else {
            ConnState::Discovering
        };
        if self.conn_state != st {
            self.conn_state = st;
            self.dirty = true;
        }
    }

    pub(crate) fn on_peer_control(&mut self, session_id: u64, msg: pb::control::Msg) {
        use pb::control::Msg as M;
        let Some(s) = self.session.as_mut() else { return };
        if s.link.session_id != session_id {
            return;
        }
        match msg {
            M::Ping(p) => {
                s.link.send_control(M::Pong(pb::Pong { t0_us: p.t0_us, t1_us: now_us() }));
            }
            M::Pong(p) => {
                s.last_pong = Instant::now();
                self.clock.on_pong(p.t0_us, p.t1_us, now_us());
                if let Some(rtt) = self.clock.rtt_us() {
                    self.rtt_history.push_back(rtt.min(u64::from(u32::MAX)) as u32);
                    while self.rtt_history.len() > 60 {
                        self.rtt_history.pop_front();
                    }
                }
                self.dirty = true;
            }
            M::Goodbye(g) => {
                tracing::info!(reason = %g.reason, "peer said goodbye");
                if g.reason == "unpaired" {
                    let id = s.link.peer.id;
                    let _ = self.peers.remove(id);
                }
                self.close_session("peer goodbye");
            }
            M::ScreensUpdate(u) => {
                s.remote_screens = u.screens.iter().map(screen_from_pb).collect();
                self.rebuild_layout(false);
                self.dirty = true;
            }
            M::Layout(l) => self.on_layout_update(l),
            M::Error(e) => tracing::warn!(code = e.code, msg = %e.message, "peer error"),
            other => self.on_feature_control(other),
        }
    }

}

pub(crate) fn screen_to_pb(s: &ScreenInfo) -> pb::Screen {
    pb::Screen {
        id: s.id,
        x: s.x,
        y: s.y,
        w: s.w,
        h: s.h,
        dpi: s.dpi,
        refresh_mhz: s.refresh_mhz,
        primary: s.primary,
        name: s.name.clone(),
    }
}

pub(crate) fn screen_from_pb(s: &pb::Screen) -> ScreenInfo {
    ScreenInfo {
        id: s.id,
        x: s.x,
        y: s.y,
        w: s.w,
        h: s.h,
        dpi: s.dpi,
        refresh_mhz: s.refresh_mhz,
        primary: s.primary,
        name: s.name.clone(),
    }
}

pub(crate) fn since_unix_ms() -> u64 {
    unix_ms()
}
