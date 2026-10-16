//! Focus mode (F6): the laptop screen on the PC monitor, full, split or in a window.
//!
//! Roles: the *viewer* (PC, input source) negotiates, shows the stream and maps the pointer
//! absolutely onto the laptop; the *source* (laptop) creates the virtual display, captures,
//! encodes and sends. Rollback rules (plan 6.4): every failure and every disconnect restores the
//! display topology; the viewer never keeps a window or a grabbed keyboard without a stream.

use super::Engine;
use super::session::{PeerLink, uni};
use crate::ipc::{FeaturesDto, FocusDto, Live, StreamStatsDto, UiEvent};
use deskbridge_display::{
    FocusDisplaySession, FocusLayout, FocusOptions, PlanOptions, Side, SplitShare, plan_focus,
};
use deskbridge_input::router::AbsRegion;
use deskbridge_platform::display::Mode;
use deskbridge_platform::video::{
    CaptureOutput, CursorState, EncoderConfig, Frame, ScreenCapturer, VideoCodec, VideoDecoder,
    VideoEncoder, VideoPresenter,
};
use deskbridge_proto::media::VideoHeader;
use deskbridge_proto::pb;
use deskbridge_video::abr::{AbrSample, BitrateController, LinkClass};
use deskbridge_video::backend::PresenterMode;
use deskbridge_video::negotiate::{WantedStream, codec_from_pb, codec_to_pb, negotiate};
use deskbridge_video::pacing::{IdrThrottle, KeepAlive};
use deskbridge_video::stream::{
    LateFrameFilter, Verdict, frame_header, keepalive_header, read_frame, write_frame,
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, mpsc as std_mpsc};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::unbounded_channel;

type CaptureSetup = (Box<dyn ScreenCapturer>, Box<dyn VideoEncoder>);

const START_TIMEOUT: Duration = Duration::from_secs(25);
/// The viewer covers the PC monitor and holds the keyboard: without a picture the user is stuck
/// in front of a black screen. No first picture within this time ends focus mode.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(8);
/// After the first picture the source sends at least a keep-alive every 500 ms.
const STREAM_GAP_TIMEOUT: Duration = Duration::from_secs(4);
/// The source gives up (and tells the viewer why) if nothing could be sent by then.
const SOURCE_FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the "how to get out" hint stays on the picture.
const EXIT_HINT_TIME: Duration = Duration::from_secs(6);
/// Frames in flight on the sender before it drops (and requests an IDR later).
const MAX_INFLIGHT: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Phase {
    #[default]
    Off,
    Preparing,
    Streaming,
    /// Reserved: the UI protocol knows a "stopping" phase; teardown is currently instantaneous.
    #[allow(dead_code)]
    Stopping,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Preparing => "preparing",
            Self::Streaming => "streaming",
            Self::Stopping => "stopping",
        }
    }
}

#[derive(Default)]
pub(crate) struct FocusRt {
    phase: Phase,
    mode: String,
    deadline: Option<Instant>,
    // negotiated values for the UI
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
    codec: String,
    encoder: String,
    stats: Option<StreamStatsDto>,
    stats_overlay: bool,
    screen_id: u32,
    // viewer side
    streaming_since: Option<Instant>,
    hint_until: Option<Instant>,
    /// Last logged "size encoder adapter" of the source pipeline.
    source_info_key: Option<String>,
    plan: Option<ViewerPlan>,
    viewer: Option<Viewer>,
    // source side
    display: Option<Arc<FocusDisplaySession>>,
    source: Option<Source>,
}

struct ViewerPlan {
    presenter: PresenterMode,
    cfg: EncoderConfig,
}

struct Viewer {
    cmd: std_mpsc::Sender<ViewerCmd>,
    shared: Arc<ViewerShared>,
    _thread: std::thread::JoinHandle<()>,
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let _ = self.cmd.send(ViewerCmd::Stop);
    }
}

enum ViewerCmd {
    Frame(VideoHeader, Vec<u8>, Instant),
    Cursor(CursorState),
    Overlay(bool),
    /// Text shown on the picture (`None` hides it again, unless the stats overlay is on).
    Hint(Option<String>),
    Stop,
}

#[derive(Default)]
struct ViewerShared {
    presented: AtomicU64,
    /// Frames presented since the start (never reset).
    presented_total: AtomicU64,
    /// `clock::now_us()` of the last frame or keep-alive that arrived; 0 = none yet.
    last_rx_us: AtomicU64,
    bytes: AtomicU64,
    decode_us_sum: AtomicU64,
    decode_n: AtomicU64,
    total_us_sum: AtomicU64,
    network_us_sum: AtomicU64,
    loss_permille: AtomicU32,
    /// peer_clock - our_clock (µs), kept fresh by the actor.
    clock_offset_us: AtomicI64,
    /// Last known presenter rectangle (window mode can be moved).
    rect: Mutex<Option<(i32, i32, u32, u32)>>,
    last_text: Mutex<String>,
}

struct Source {
    stop: Arc<AtomicBool>,
    force_key: Arc<AtomicBool>,
    feedback: Arc<Mutex<Option<pb::StreamStats>>>,
    task: tokio::task::JoinHandle<()>,
    capturer_stop: Arc<Mutex<Option<Box<dyn ScreenCapturer>>>>,
}

impl Drop for Source {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(mut c) = self.capturer_stop.lock().take() {
            c.stop();
        }
        self.task.abort();
    }
}

fn layout_of(cfg: &crate::config::Config) -> (FocusLayout, &'static str) {
    match cfg.focus.mode.as_str() {
        "split" => {
            let side = if cfg.focus.split_side == "left" {
                Side::Left
            } else {
                Side::Right
            };
            let share =
                SplitShare::from_percent(cfg.focus.split_percent).unwrap_or(SplitShare::Half);
            (FocusLayout::Split { side, share }, "split")
        }
        "window" => (FocusLayout::Full, "window"),
        _ => (FocusLayout::Full, "full"),
    }
}

impl Engine {
    // ---------------------------------------------------------------------------------------
    // entry points (UI, hotkeys, lid)
    // ---------------------------------------------------------------------------------------

    /// UI / hotkey: "off" | "full" | "split" | "window" | "cycle".
    pub(crate) fn set_focus(&mut self, mode: &str) -> Result<Value, String> {
        let target = match mode {
            // On, or off again: a hotkey that first switched to split left people in front of
            // a second picture instead of their PC.
            "cycle" => match self.focus.phase {
                Phase::Off => self.cfg.focus.mode.clone().replace("window", "full"),
                _ => "off".to_string(),
            },
            other => other.to_string(),
        };
        if target == "off" {
            self.stop_focus("Fokusmodus beendet", true);
            return Ok(json!({ "ok": true }));
        }
        if !matches!(target.as_str(), "full" | "split" | "window") {
            return Err("Unbekannter Modus".into());
        }
        if self.focus.phase != Phase::Off {
            // switching mode = restart
            self.stop_focus("", false);
        }
        self.cfg.focus.mode = target;
        self.start_focus()
    }

    pub(crate) fn on_focus_hotkey(&mut self, h: deskbridge_input::hotkeys::HotkeyAction) {
        use deskbridge_input::hotkeys::HotkeyAction as H;
        match h {
            H::FocusCycle => {
                let _ = self.set_focus("cycle");
            }
            H::FocusSplit => {
                let m = if self.focus.phase != Phase::Off && self.focus.mode == "split" {
                    "off"
                } else {
                    "split"
                };
                let _ = self.set_focus(m);
            }
            H::StatsOverlay => {
                self.focus.stats_overlay = !self.focus.stats_overlay;
                if let Some(v) = &self.focus.viewer {
                    let _ = v.cmd.send(ViewerCmd::Overlay(self.focus.stats_overlay));
                }
            }
            _ => {}
        }
    }

    pub(crate) fn focus_on_lid(&mut self, closed: bool) {
        if !self.cfg.focus.lid_auto || !self.cfg.device.stream_source {
            return;
        }
        if let Some(s) = &self.session {
            s.link
                .send_control(pb::control::Msg::FocusRequest(pb::FocusRequest {
                    start: closed,
                }));
        }
    }

    fn unavailable_reason(&self) -> Option<String> {
        if self.cfg.device.input_source {
            if self.platform.video.decoder_caps().decode.is_empty() {
                return Some("Dieser PC kann kein Video per Hardware decodieren.".into());
            }
            if let Some(s) = &self.session {
                let caps = &s.link.hello.caps;
                if !caps
                    .iter()
                    .any(|c| c.name == deskbridge_proto::caps::FOCUS_SOURCE)
                {
                    return Some("Das Partnergerät unterstützt den Fokusmodus nicht.".into());
                }
                if s.link
                    .hello
                    .video
                    .as_ref()
                    .is_none_or(|v| v.encode.is_empty())
                {
                    return Some("Der Laptop hat keinen Hardware-Video-Encoder (NVENC, Quick Sync oder AMF).".into());
                }
                if !caps.iter().any(|c| c.name == "focus.vdisplay") {
                    return Some(
                        "Der virtuelle Monitor-Treiber ist auf dem Laptop nicht installiert."
                            .into(),
                    );
                }
            }
        }
        None
    }

    // ---------------------------------------------------------------------------------------
    // viewer (PC)
    // ---------------------------------------------------------------------------------------

    fn start_focus(&mut self) -> Result<Value, String> {
        if !self.cfg.device.input_source {
            return Err("Der Fokusmodus wird am PC mit Maus und Tastatur gestartet.".into());
        }
        let Some(s) = &self.session else {
            return Err("Nicht verbunden".into());
        };
        if let Some(r) = self.unavailable_reason() {
            return Err(r);
        }
        let link = s.link.clone();
        let peer_video = link.hello.video.clone().unwrap_or_default();
        let own_decoder = self.platform.video.decoder_caps();

        // target monitor
        let screens = &self.local_screens;
        let cursor = self.platform.screens.cursor_pos();
        let idx = self.cfg.focus.target_monitor;
        let mon = usize::try_from(idx)
            .ok()
            .and_then(|i| screens.get(i))
            .or_else(|| screens.iter().find(|s| s.contains(cursor)))
            .or_else(|| screens.iter().find(|s| s.primary))
            .or(screens.first())
            .ok_or("Kein Monitor gefunden")?
            .clone();
        let hz = (mon.refresh_mhz / 1000).max(30);
        let (layout, mode_name) = layout_of(&self.cfg);
        let plan = plan_focus(
            Mode {
                width: mon.w,
                height: mon.h,
                hz,
            },
            layout,
            PlanOptions::default(),
        )
        .map_err(|e| format!("Monitormodus nicht nutzbar: {e}"))?;
        let want_fps = if self.cfg.focus.fps == 0 {
            plan.virtual_mode.hz
        } else {
            self.cfg.focus.fps
        };
        let mut want =
            WantedStream::new(plan.virtual_mode.width, plan.virtual_mode.height, want_fps);
        want.prefer = match self.cfg.focus.codec.as_str() {
            "hevc" => Some(VideoCodec::Hevc),
            "av1" => Some(VideoCodec::Av1),
            "h264" => Some(VideoCodec::H264),
            _ => None,
        };
        want.want_yuv444 = self.cfg.focus.yuv444;
        if !self.cfg.focus.bitrate_auto {
            want.bitrate_kbps = Some(self.cfg.focus.bitrate_kbps);
        }
        let neg = negotiate(&peer_video, &own_decoder, &want).map_err(|e| match e {
            deskbridge_video::NegotiateError::NoEncoder => {
                "Der Laptop hat keinen Hardware-Video-Encoder.".to_string()
            }
            deskbridge_video::NegotiateError::NoDecoder => {
                "Dieser PC kann kein Video decodieren.".to_string()
            }
            other => format!("Keine gemeinsame Video-Einstellung: {other}"),
        })?;
        for n in &neg.notes {
            tracing::info!("focus negotiation: {n}");
        }

        let rect = plan.viewer_rect_on(mon.x, mon.y);
        let presenter_mode = match (mode_name, plan.pc_region) {
            ("window", _) => PresenterMode::Window {
                w: (mon.w * 2 / 3).max(640),
                h: (mon.h * 2 / 3).max(360),
            },
            (_, Some(_)) => PresenterMode::Region {
                x: rect.x,
                y: rect.y,
                w: rect.w,
                h: rect.h,
                appbar: true,
            },
            _ => PresenterMode::Fullscreen {
                x: rect.x,
                y: rect.y,
                w: rect.w,
                h: rect.h,
            },
        };
        let cfg = EncoderConfig {
            codec: neg.codec,
            width: neg.width,
            height: neg.height,
            fps: neg.fps,
            bitrate_kbps: neg.bitrate_kbps,
            yuv444: neg.yuv444,
        };
        link.send_control(pb::control::Msg::FocusStart(pb::FocusStart {
            width: neg.width,
            height: neg.height,
            fps: neg.fps,
            codec: codec_to_pb(neg.codec) as i32,
            bitrate_kbps: neg.bitrate_kbps,
            yuv444: neg.yuv444,
            layout: match layout {
                FocusLayout::Full if mode_name == "window" => pb::FocusLayout::Window as i32,
                FocusLayout::Full => pb::FocusLayout::Full as i32,
                FocusLayout::Split {
                    side: Side::Left, ..
                } => pb::FocusLayout::SplitLeft as i32,
                FocusLayout::Split {
                    side: Side::Right, ..
                } => pb::FocusLayout::SplitRight as i32,
            },
            disable_internal_display: self.cfg.focus.disable_internal_display,
        }));
        self.focus.phase = Phase::Preparing;
        self.focus.mode = mode_name.to_string();
        self.focus.deadline = Some(Instant::now() + START_TIMEOUT);
        self.focus.width = neg.width;
        self.focus.height = neg.height;
        self.focus.fps = neg.fps;
        self.focus.bitrate_kbps = neg.bitrate_kbps;
        self.focus.codec = codec_name(neg.codec).into();
        self.focus.plan = Some(ViewerPlan {
            presenter: presenter_mode,
            cfg,
        });
        self.emit(UiEvent::Osd {
            icon: "monitor".into(),
            text: "Fokusmodus startet …".into(),
        });
        self.dirty = true;
        Ok(json!({ "ok": true }))
    }

    pub(crate) fn on_focus_ready(&mut self, r: pb::FocusReady) {
        if self.focus.phase != Phase::Preparing {
            return;
        }
        let Some(plan) = self.focus.plan.take() else {
            return;
        };
        let Some(s) = &self.session else { return };
        let link = s.link.clone();
        self.focus.encoder = r.encoder.clone();
        self.focus.screen_id = r.screen_id;
        let presenter = match self
            .platform
            .video
            .create_presenter(plan.presenter, &plan.cfg)
        {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("presenter failed: {e}");
                self.abort_focus(&format!("Anzeigefenster konnte nicht geöffnet werden: {e}"));
                return;
            }
        };
        let decoder = match self.platform.video.create_decoder(&plan.cfg) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("decoder failed: {e}");
                self.abort_focus(&format!("Video-Decoder nicht verfügbar: {e}"));
                return;
            }
        };
        let shared = Arc::new(ViewerShared::default());
        shared
            .clock_offset_us
            .store(self.clock.offset_us().unwrap_or(0), Ordering::Relaxed);
        let (cmd_tx_plain, cmd_rx_plain) = std_mpsc::channel::<ViewerCmd>();
        let sh = shared.clone();
        let link_t = link.clone();
        let thread = std::thread::Builder::new()
            .name("focus-decode".into())
            .spawn(move || viewer_thread(cmd_rx_plain, decoder, presenter, sh, link_t))
            .ok();
        let Some(thread) = thread else {
            self.abort_focus("Decoder-Thread konnte nicht gestartet werden");
            return;
        };

        // incoming frame streams
        let (utx, mut urx) = unbounded_channel::<quinn::RecvStream>();
        link.set_uni_sink(uni::VIDEO, Some(utx));
        let cmd_for_streams = cmd_tx_plain.clone();
        let sh_rx = shared.clone();
        tokio::spawn(async move {
            while let Some(mut r) = urx.recv().await {
                let cmd = cmd_for_streams.clone();
                let sh = sh_rx.clone();
                tokio::spawn(async move {
                    if let Ok((h, payload)) = read_frame(&mut r).await {
                        sh.last_rx_us
                            .store(crate::clock::now_us().max(1), Ordering::Relaxed);
                        let _ = cmd.send(ViewerCmd::Frame(h, payload, Instant::now()));
                    }
                });
            }
        });

        // pointer mapping: absolute onto the laptop screen
        let (rx, ry, rw, rh) = plan.presenter.rect();
        let (px, py, pw, ph) = if matches!(plan.presenter, PresenterMode::Window { .. }) {
            // window mode follows the window; refreshed on tick
            (rx, ry, rw, rh)
        } else {
            (rx, ry, rw, rh)
        };
        let peer = link.peer.id;
        let actions = self.router.set_region(Some(AbsRegion {
            device: peer,
            screen: r.screen_id,
            x: px,
            y: py,
            w: pw as i32,
            h: ph as i32,
        }));
        self.exec_actions(actions, true);

        let hint = format!("Fokusmodus beenden: {}", self.focus_exit_keys());
        let _ = cmd_tx_plain.send(ViewerCmd::Hint(Some(hint.clone())));
        self.focus.viewer = Some(Viewer {
            cmd: cmd_tx_plain,
            shared,
            _thread: thread,
        });
        tracing::info!(
            screen = r.screen_id,
            encoder = %r.encoder,
            "focus mode: source ready, viewer open"
        );
        self.focus.phase = Phase::Streaming;
        self.focus.deadline = None;
        self.focus.streaming_since = Some(Instant::now());
        self.focus.hint_until = Some(Instant::now() + EXIT_HINT_TIME);
        self.platform.power.set_keep_awake(true);
        self.emit(UiEvent::Osd {
            icon: "monitor".into(),
            text: hint,
        });
        self.dirty = true;
    }

    /// The binding that ends focus mode, in the words of a German keyboard.
    fn focus_exit_keys(&self) -> String {
        use deskbridge_input::hotkeys::HotkeyAction as H;
        let b = self
            .cfg
            .hotkeys
            .get(H::FocusCycle.key())
            .cloned()
            .unwrap_or_else(|| H::FocusCycle.default_binding().to_string());
        b.replace("Ctrl", "Strg").replace("Shift", "Umschalt")
    }

    pub(crate) fn on_focus_failed(&mut self, reason: &str) {
        tracing::warn!(reason, "focus mode failed on the source");
        self.abort_focus(reason);
    }

    /// Cleans up after a failed start and tells the user.
    fn abort_focus(&mut self, reason: &str) {
        let had_peer = self.session.is_some();
        self.stop_focus_inner(had_peer);
        self.emit(UiEvent::Notification {
            level: "error".into(),
            text: format!("Fokusmodus: {reason}"),
        });
    }

    /// What the laptop pipeline does; the first report and any change of size or encoder are
    /// logged at info, the rest at debug.
    fn on_focus_source_info(&mut self, i: &pb::FocusSourceInfo) {
        let key = format!(
            "{}x{} {} {}",
            i.capture_width, i.capture_height, i.encoder, i.adapter
        );
        let first = self.focus.source_info_key.as_deref() != Some(key.as_str());
        if first {
            tracing::info!(
                capture = %format!("{}x{}", i.capture_width, i.capture_height),
                stream = %format!("{}x{}", self.focus.width, self.focus.height),
                encoder = %i.encoder,
                adapter = %i.adapter,
                "focus source pipeline"
            );
            self.focus.source_info_key = Some(key);
        }
        tracing::info!(
            capture_fps = i.capture_fps,
            encoded_fps = i.encoded_fps,
            encode_ms = f64::from(i.encode_us) / 1000.0,
            skipped = i.skipped,
            "focus source rate"
        );
        if i.capture_width != 0
            && (i.capture_width, i.capture_height) != (self.focus.width, self.focus.height)
        {
            self.warn_once(
                "focus_capture_size",
                &format!(
                    "Der virtuelle Monitor des Laptops läuft mit {}x{} statt {}x{}; das Bild wird skaliert.",
                    i.capture_width, i.capture_height, self.focus.width, self.focus.height
                ),
            );
        }
    }

    /// Viewer cursor updates from the source.
    pub(crate) fn on_focus_cursor(&mut self, state: CursorState) {
        if let Some(v) = &self.focus.viewer {
            let _ = v.cmd.send(ViewerCmd::Cursor(state));
        }
    }

    // ---------------------------------------------------------------------------------------
    // source (laptop)
    // ---------------------------------------------------------------------------------------

    pub(crate) fn on_focus_start(&mut self, fs: pb::FocusStart) {
        let Some(s) = &self.session else { return };
        let link = s.link.clone();
        if !self.cfg.device.stream_source {
            link.send_control(pb::control::Msg::FocusFailed(pb::FocusFailed {
                reason: "Dieses Gerät ist keine Stream-Quelle.".into(),
            }));
            return;
        }
        if self.focus.phase != Phase::Off {
            self.stop_focus("", false);
        }
        let Some(codec) = codec_from_pb(fs.codec) else {
            link.send_control(pb::control::Msg::FocusFailed(pb::FocusFailed {
                reason: "Unbekannter Codec".into(),
            }));
            return;
        };
        self.focus.phase = Phase::Preparing;
        self.focus.mode = if fs.layout == pb::FocusLayout::Full as i32 {
            "full".into()
        } else if fs.layout == pb::FocusLayout::Window as i32 {
            "window".into()
        } else {
            "split".into()
        };
        self.focus.width = fs.width;
        self.focus.height = fs.height;
        self.focus.fps = fs.fps;
        self.focus.bitrate_kbps = fs.bitrate_kbps;
        self.focus.codec = codec_name(codec).into();
        self.dirty = true;

        let dm = self.platform.display.clone();
        let session = Arc::new(FocusDisplaySession::new(dm, self.paths.topology_file()));
        self.focus.display = Some(session.clone());
        let video = self.platform.video.clone();
        let tx = self.msg_tx.clone();
        let opts = FocusOptions {
            disable_internal: fs.disable_internal_display,
            render_adapter_hint: None,
        };
        if self.cfg.focus.lid_do_nothing {
            let _ = self.platform.power.set_lid_action_do_nothing(true);
        }
        self.platform.power.set_keep_awake(true);
        let cfg = EncoderConfig {
            codec,
            width: fs.width,
            height: fs.height,
            fps: fs.fps,
            bitrate_kbps: fs.bitrate_kbps,
            yuv444: fs.yuv444,
        };
        tokio::spawn(async move {
            let mode = Mode {
                width: fs.width,
                height: fs.height,
                hz: fs.fps,
            };
            let s2 = session.clone();
            let began = tokio::task::spawn_blocking(move || s2.begin(mode, &opts)).await;
            let screen_id = match began {
                Ok(Ok(id)) => id,
                Ok(Err(e)) => {
                    let _ = tx.send(super::Msg::FocusSourceFailed {
                        reason: format!("Virtueller Monitor nicht verfügbar: {e}"),
                    });
                    return;
                }
                Err(e) => {
                    let _ = tx.send(super::Msg::FocusSourceFailed {
                        reason: format!("Intern: {e}"),
                    });
                    return;
                }
            };
            let setup = (|| -> Result<CaptureSetup, String> {
                let capturer = video
                    .create_capturer()
                    .map_err(|e| format!("Bildschirmaufnahme nicht möglich: {e}"))?;
                let encoder = video
                    .create_encoder(&cfg, &capturer.adapter_name())
                    .map_err(|e| format!("Video-Encoder nicht verfügbar: {e}"))?;
                Ok((capturer, encoder))
            })();
            match setup {
                Ok((capturer, encoder)) => {
                    let _ = tx.send(super::Msg::FocusSourceUp {
                        screen_id,
                        capturer,
                        encoder,
                        cfg,
                        link,
                    });
                }
                Err(reason) => {
                    let _ = tx.send(super::Msg::FocusSourceFailed { reason });
                }
            }
        });
    }

    pub(crate) fn on_focus_source_up(
        &mut self,
        screen_id: u32,
        mut capturer: Box<dyn ScreenCapturer>,
        encoder: Box<dyn VideoEncoder>,
        cfg: EncoderConfig,
        link: PeerLink,
    ) {
        if self.focus.phase != Phase::Preparing {
            // stopped meanwhile
            capturer.stop();
            self.end_display_async();
            return;
        }
        let (tx, rx) = unbounded_channel::<CaptureOutput>();
        if let Err(e) = capturer.start(screen_id, cfg.fps, tx) {
            self.on_focus_source_failed(&format!("Aufnahme konnte nicht starten: {e}"));
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let force_key = Arc::new(AtomicBool::new(true));
        let feedback = Arc::new(Mutex::new(None));
        let capturer_slot = Arc::new(Mutex::new(Some(capturer)));
        let encoder_name = encoder.name();
        let adapter = capturer_slot
            .lock()
            .as_ref()
            .map(|c| c.adapter_name())
            .unwrap_or_default();
        let task = tokio::spawn(run_source(
            link.clone(),
            rx,
            encoder,
            cfg.clone(),
            SourceSignals {
                stop: stop.clone(),
                force_key: force_key.clone(),
                feedback: feedback.clone(),
            },
            adapter,
        ));
        self.focus.source = Some(Source {
            stop,
            force_key,
            feedback,
            task,
            capturer_stop: capturer_slot,
        });
        self.focus.encoder = encoder_name.clone();
        self.focus.phase = Phase::Streaming;
        link.send_control(pb::control::Msg::FocusReady(pb::FocusReady {
            width: cfg.width,
            height: cfg.height,
            codec: codec_to_pb(cfg.codec) as i32,
            fps: cfg.fps,
            screen_id,
            encoder: encoder_name,
            yuv444: cfg.yuv444,
        }));
        self.dirty = true;
    }

    pub(crate) fn on_focus_source_failed(&mut self, reason: &str) {
        if let Some(s) = &self.session {
            s.link
                .send_control(pb::control::Msg::FocusFailed(pb::FocusFailed {
                    reason: reason.to_string(),
                }));
        }
        self.stop_focus_inner(false);
    }

    fn end_display_async(&mut self) {
        if let Some(d) = self.focus.display.take() {
            let tx = self.msg_tx.clone();
            if self.cfg.focus.lid_do_nothing {
                let _ = self.platform.power.set_lid_action_do_nothing(false);
            }
            tokio::spawn(async move {
                let r = tokio::task::spawn_blocking(move || d.end()).await;
                if let Ok(Err(e)) = r {
                    tracing::error!("restoring the display failed: {e}");
                    let _ = tx.send(super::Msg::Warn {
                        code: "display_restore".into(),
                        message: format!(
                            "Die Bildschirmanordnung konnte nicht wiederhergestellt werden: {e}"
                        ),
                    });
                }
            });
        }
    }

    // ---------------------------------------------------------------------------------------
    // common
    // ---------------------------------------------------------------------------------------

    /// Stops focus mode on both sides. `notify_peer`: send `FocusStop`.
    pub(crate) fn stop_focus(&mut self, osd: &str, notify_peer: bool) {
        if self.focus.phase == Phase::Off && self.focus.display.is_none() {
            return;
        }
        self.stop_focus_inner(notify_peer);
        if !osd.is_empty() {
            self.emit(UiEvent::Osd {
                icon: "monitor".into(),
                text: osd.into(),
            });
        }
    }

    fn stop_focus_inner(&mut self, notify_peer: bool) {
        if notify_peer && let Some(s) = &self.session {
            s.link
                .send_control(pb::control::Msg::FocusStop(pb::FocusStop {}));
        }
        // viewer
        if self.focus.viewer.is_some() || self.focus.plan.is_some() {
            let actions = self.router.set_region(None);
            self.exec_actions(actions, self.session.is_some());
        }
        if let Some(s) = &self.session {
            s.link.set_uni_sink(uni::VIDEO, None);
        }
        self.focus.viewer = None;
        self.focus.plan = None;
        self.focus.streaming_since = None;
        self.focus.hint_until = None;
        self.focus.source_info_key = None;
        // source
        self.focus.source = None;
        self.end_display_async();
        if self.focus.phase != Phase::Off {
            tracing::info!(mode = %self.focus.mode, "focus mode stopped");
            self.platform.power.set_keep_awake(false);
        }
        self.focus.phase = Phase::Off;
        self.focus.deadline = None;
        self.focus.stats = None;
        self.dirty = true;
    }

    pub(crate) fn on_focus_stop_msg(&mut self) {
        self.stop_focus("Fokusmodus beendet", false);
    }

    pub(crate) fn on_focus_request(&mut self, r: pb::FocusRequest) {
        if !self.cfg.device.input_source {
            return;
        }
        if r.start && self.focus.phase == Phase::Off {
            if let Err(e) = self.set_focus(&self.cfg.focus.mode.clone()) {
                tracing::warn!("focus request from lid automation failed: {e}");
            }
        } else if !r.start && self.focus.phase != Phase::Off {
            self.stop_focus("Fokusmodus beendet", true);
        }
    }

    pub(crate) fn focus_control(&mut self, msg: &pb::control::Msg) -> bool {
        use pb::control::Msg as M;
        match msg {
            M::FocusStart(fs) => self.on_focus_start(*fs),
            M::FocusReady(r) => self.on_focus_ready(r.clone()),
            M::FocusFailed(f) => self.on_focus_failed(&f.reason),
            M::FocusStop(_) => self.on_focus_stop_msg(),
            M::FocusRequest(r) => self.on_focus_request(*r),
            M::KeyframeRequest(_) => {
                if let Some(s) = &self.focus.source {
                    s.force_key.store(true, Ordering::SeqCst);
                }
            }
            M::StreamStats(st) => {
                if let Some(s) = &self.focus.source {
                    *s.feedback.lock() = Some(*st);
                }
            }
            M::CursorShape(c) => {
                self.cursor_shape = Some(deskbridge_platform::video::CursorShapeData {
                    width: c.width,
                    height: c.height,
                    hot_x: c.hot_x,
                    hot_y: c.hot_y,
                    rgba: c.rgba.clone(),
                });
                self.cursor_shape_serial = c.serial;
            }
            M::CursorPos(p) => {
                let state = CursorState {
                    x: p.x,
                    y: p.y,
                    visible: p.visible,
                    shape_serial: p.serial,
                    shape: if p.serial == self.cursor_shape_serial {
                        self.cursor_shape.clone()
                    } else {
                        None
                    },
                };
                self.on_focus_cursor(state);
            }
            M::BitrateHint(_) => {}
            M::FocusSourceInfo(i) => self.on_focus_source_info(i),
            _ => return false,
        }
        true
    }

    /// 250 ms tick: start timeout, window-mode pointer region, stats.
    pub(crate) fn tick_focus(&mut self) {
        if let Some(d) = self.focus.deadline
            && Instant::now() > d
        {
            tracing::warn!("focus mode start timed out");
            self.abort_focus("Zeitüberschreitung beim Start");
            return;
        }
        let Some(v) = &self.focus.viewer else { return };
        if let Some(reason) = self.viewer_stalled(&v.shared) {
            tracing::warn!(reason, "focus mode: no picture from the source, stopping");
            self.abort_focus(reason);
            return;
        }
        if self.focus.hint_until.is_some_and(|t| Instant::now() > t) {
            self.focus.hint_until = None;
            let _ = v.cmd.send(ViewerCmd::Hint(None));
        }
        v.shared
            .clock_offset_us
            .store(self.clock.offset_us().unwrap_or(0), Ordering::Relaxed);
        if !self.tick_count.is_multiple_of(4) {
            return;
        }
        // 1 s statistics
        let sh = v.shared.clone();
        let n = sh.presented.swap(0, Ordering::Relaxed);
        let bytes = sh.bytes.swap(0, Ordering::Relaxed);
        let dn = sh.decode_n.swap(0, Ordering::Relaxed).max(1);
        let dus = sh.decode_us_sum.swap(0, Ordering::Relaxed) / dn;
        let total = sh.total_us_sum.swap(0, Ordering::Relaxed) / n.max(1);
        let net = sh.network_us_sum.swap(0, Ordering::Relaxed) / n.max(1);
        let loss = sh.loss_permille.load(Ordering::Relaxed) as f32 / 10.0;
        let st = pb::StreamStats {
            fps: n as u32,
            bitrate_kbps: (bytes * 8 / 1000) as u32,
            loss_pct: loss,
            encode_us: 0,
            network_us: net as u32,
            decode_us: dus as u32,
            total_us: total as u32,
        };
        self.focus.stats = Some(StreamStatsDto {
            fps: st.fps,
            bitrate_kbps: st.bitrate_kbps,
            loss_pct: st.loss_pct,
            encode_ms: 0.0,
            network_ms: st.network_us as f32 / 1000.0,
            decode_ms: st.decode_us as f32 / 1000.0,
            total_ms: st.total_us as f32 / 1000.0,
        });
        *sh.last_text.lock() = format!(
            "{} fps  {:.0} Mbit/s  Verlust {:.1}%\nNetz {:.1} ms  Decode {:.1} ms  Gesamt {:.1} ms",
            st.fps,
            f64::from(st.bitrate_kbps) / 1000.0,
            st.loss_pct,
            st.network_us as f32 / 1000.0,
            st.decode_us as f32 / 1000.0,
            st.total_us as f32 / 1000.0
        );
        if let Some(s) = &self.session {
            s.link.send_control(pb::control::Msg::StreamStats(st));
        }
        // window mode: the window can be moved
        if self.focus.mode == "window"
            && let (Some(rect), Some(s)) = (*sh.rect.lock(), &self.session)
        {
            let actions = self.router.set_region(Some(AbsRegion {
                device: s.link.peer.id,
                screen: self.focus.screen_id,
                x: rect.0,
                y: rect.1,
                w: rect.2 as i32,
                h: rect.3 as i32,
            }));
            let _ = actions; // region update only; no enter/leave side effects while unchanged
        }
    }

    /// Why the viewer should give up: no first picture, or the stream stopped arriving.
    fn viewer_stalled(&self, sh: &ViewerShared) -> Option<&'static str> {
        let since = self.focus.streaming_since?;
        if sh.presented_total.load(Ordering::Relaxed) == 0 {
            return (since.elapsed() > FIRST_FRAME_TIMEOUT)
                .then_some("Kein Bild vom Laptop erhalten");
        }
        let last = sh.last_rx_us.load(Ordering::Relaxed);
        let gap = crate::clock::now_us().saturating_sub(last);
        (last != 0 && gap > STREAM_GAP_TIMEOUT.as_micros() as u64)
            .then_some("Die Bildübertragung vom Laptop ist abgerissen")
    }

    /// Session ended: restore everything (viewer window, topology).
    pub(crate) fn focus_session_down(&mut self) {
        self.stop_focus_inner(false);
    }

    /// Blocking variant for process shutdown.
    pub(crate) fn focus_shutdown(&mut self) {
        let display = self.focus.display.take();
        self.focus.source = None;
        self.focus.viewer = None;
        if let Some(d) = display
            && let Err(e) = d.end()
        {
            tracing::error!("restoring the display at shutdown failed: {e}");
        }
        self.focus.phase = Phase::Off;
    }

    pub(crate) fn fill_focus_state(&self, f: &mut FeaturesDto) {
        let reason = self.unavailable_reason();
        let mut d = FocusDto::default();
        d.base.enabled = true;
        d.phase = self.focus.phase.as_str().into();
        d.mode = if self.focus.mode.is_empty() {
            self.cfg.focus.mode.clone()
        } else {
            self.focus.mode.clone()
        };
        d.width = self.focus.width;
        d.height = self.focus.height;
        d.fps = self.focus.fps;
        d.bitrate_kbps = self.focus.bitrate_kbps;
        d.codec = self.focus.codec.clone();
        d.encoder = self.focus.encoder.clone();
        d.stats = self.focus.stats.clone();
        d.unavailable_reason = reason.clone();
        d.base.state = if reason.is_some() {
            Live::Unavailable
        } else {
            match self.focus.phase {
                Phase::Streaming => Live::Live,
                Phase::Preparing | Phase::Stopping => Live::Live,
                Phase::Off => Live::Ready,
            }
        };
        d.base.detail = reason;
        f.focus = d;
    }

    pub(crate) fn focus_is_active(&self) -> bool {
        self.focus.phase != Phase::Off
    }
}

fn codec_name(c: VideoCodec) -> &'static str {
    match c {
        VideoCodec::H264 => "h264",
        VideoCodec::Hevc => "hevc",
        VideoCodec::Av1 => "av1",
    }
}

// --- viewer decode thread -----------------------------------------------------------------------

fn viewer_thread(
    rx: std_mpsc::Receiver<ViewerCmd>,
    mut decoder: Box<dyn VideoDecoder>,
    mut presenter: Box<dyn VideoPresenter>,
    sh: Arc<ViewerShared>,
    link: PeerLink,
) {
    let epoch = Instant::now();
    let mut filter = LateFrameFilter::new();
    let mut interval = Instant::now();
    let mut overlay_on = false;
    // Every frame must be decoded (P-frames depend on each other) but only the newest one of a
    // backlog is presented, so a stall never turns into seconds of latency.
    'outer: while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        batch.extend(rx.try_iter());
        let last_frame = batch
            .iter()
            .rposition(|c| matches!(c, ViewerCmd::Frame(..)));
        for (idx, cmd) in batch.into_iter().enumerate() {
            let present_now = Some(idx) == last_frame;
            match cmd {
                ViewerCmd::Stop => break 'outer,
                ViewerCmd::Cursor(c) => presenter.set_cursor(&c),
                ViewerCmd::Overlay(on) => {
                    overlay_on = on;
                    let text = sh.last_text.lock().clone();
                    presenter.set_stats_overlay(on, &text);
                }
                ViewerCmd::Hint(Some(text)) => presenter.set_stats_overlay(true, &text),
                ViewerCmd::Hint(None) => {
                    let text = sh.last_text.lock().clone();
                    presenter.set_stats_overlay(overlay_on, &text);
                }
                ViewerCmd::Frame(h, payload, arrived) => {
                    let now_ms = epoch.elapsed().as_millis() as u64;
                    let offset = sh.clock_offset_us.load(Ordering::Relaxed);
                    let local_ts = (h.ts_us as i64 - offset).max(0) as u64;
                    let now_us = crate::clock::now_us();
                    let age = now_us.saturating_sub(local_ts);
                    let outcome = filter.on_frame(&h, now_ms, Some(age));
                    if outcome.request_keyframe {
                        link.send_control(pb::control::Msg::KeyframeRequest(
                            pb::KeyframeRequest {},
                        ));
                    }
                    if outcome.verdict != Verdict::Accept || h.is_keepalive() {
                        continue;
                    }
                    let t0 = Instant::now();
                    match decoder.decode(&payload, h.is_key()) {
                        Ok(Some(frame)) => {
                            let dec_us = t0.elapsed().as_micros() as u64;
                            let frame: Frame = frame;
                            let shown = !present_now || presenter.present(&frame).is_ok();
                            if shown {
                                if sh.presented_total.fetch_add(1, Ordering::Relaxed) == 0 {
                                    tracing::info!("focus mode: first picture shown");
                                }
                                sh.presented.fetch_add(1, Ordering::Relaxed);
                                sh.bytes.fetch_add(payload.len() as u64, Ordering::Relaxed);
                                sh.decode_us_sum.fetch_add(dec_us, Ordering::Relaxed);
                                sh.decode_n.fetch_add(1, Ordering::Relaxed);
                                let total = crate::clock::now_us().saturating_sub(local_ts);
                                sh.total_us_sum.fetch_add(total, Ordering::Relaxed);
                                sh.network_us_sum.fetch_add(
                                    age.saturating_sub(arrived.elapsed().as_micros() as u64),
                                    Ordering::Relaxed,
                                );
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::debug!("decode error: {e}");
                            if filter.on_decode_error(now_ms) {
                                link.send_control(pb::control::Msg::KeyframeRequest(
                                    pb::KeyframeRequest {},
                                ));
                            }
                        }
                    }
                }
            }
        }
        if interval.elapsed() > Duration::from_millis(500) {
            interval = Instant::now();
            sh.loss_permille.store(
                (filter.take_interval_loss_pct() * 10.0) as u32,
                Ordering::Relaxed,
            );
            *sh.rect.lock() = presenter.window_rect();
            if filter.poll_retry(epoch.elapsed().as_millis() as u64) {
                link.send_control(pb::control::Msg::KeyframeRequest(pb::KeyframeRequest {}));
            }
        }
    }
    presenter.close();
}

// --- source pipeline ----------------------------------------------------------------------------

async fn run_source(
    link: PeerLink,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<CaptureOutput>,
    mut encoder: Box<dyn VideoEncoder>,
    cfg: EncoderConfig,
    signals: SourceSignals,
    adapter: String,
) {
    let SourceSignals {
        stop,
        force_key,
        feedback,
    } = signals;
    let epoch = Instant::now();
    let inflight = Arc::new(tokio::sync::Semaphore::new(MAX_INFLIGHT));
    let mut frame_id = 0u32;
    let mut idr = IdrThrottle::new(250);
    let mut keepalive = KeepAlive::new(500);
    let mut abr = BitrateController::for_link(LinkClass::Wired);
    abr.set_bitrate_kbps(cfg.bitrate_kbps);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut last_shape_serial = 0u32;
    let (mut enc_us_sum, mut enc_n, mut fps_n) = (0u64, 0u64, 0u32);
    let mut last_abr = Instant::now();
    // Why no picture went out yet: reported to the viewer instead of a silent black screen.
    let (mut captured, mut sent_any, mut last_err) = (0u64, false, None::<String>);
    // Diagnostics for the viewer (FocusSourceInfo every 2 s).
    let mut info = SourceInfoAcc::default();
    let mut info_at = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        tokio::select! {
            out = rx.recv() => {
                let Some(first) = out else { break };
                // Encode only the newest picture of a backlog: when encoding is slower than the
                // capture rate, every queued picture added latency (seconds after a while).
                let mut batch = vec![first];
                while let Ok(more) = rx.try_recv() {
                    batch.push(more);
                }
                let newest = batch
                    .iter()
                    .rposition(|o| matches!(o, CaptureOutput::Frame(_)));
                for (i, out) in batch.into_iter().enumerate() {
                match out {
                    CaptureOutput::Frame(_) if Some(i) != newest => {
                        info.captured += 1;
                        info.skipped += 1;
                    }
                    CaptureOutput::Frame(frame) => {
                        info.captured += 1;
                        info.size = (frame.width, frame.height);
                        captured += 1;
                        if captured == 1 {
                            tracing::info!(
                                w = frame.width,
                                h = frame.height,
                                "focus source: first captured frame"
                            );
                        }
                        let now_ms = epoch.elapsed().as_millis() as u64;
                        let want_key = force_key.swap(false, Ordering::SeqCst);
                        let key = want_key && idr.request(now_ms);
                        let t0 = Instant::now();
                        let enc = &mut encoder;
                        let packets = tokio::task::block_in_place(|| enc.encode(&frame, key));
                        let packets = match packets {
                            Ok(p) => p,
                            Err(e) => {
                                tracing::warn!("encode failed: {e}");
                                last_err = Some(format!("Video-Encoder ({}): {e}", encoder.name()));
                                continue;
                            }
                        };
                        if !sent_any && !packets.is_empty() {
                            sent_any = true;
                            tracing::info!(encoder = %encoder.name(), "focus source: first frame encoded");
                        }
                        enc_us_sum += t0.elapsed().as_micros() as u64;
                        enc_n += 1;
                        info.encoded += 1;
                        info.encode_us += t0.elapsed().as_micros() as u64;
                        for p in packets {
                            // Capture timestamps are on the capture clock; the header carries ours.
                            let ts = crate::clock::now_us().saturating_sub(t0.elapsed().as_micros() as u64);
                            let h = frame_header(frame_id, p.key, cfg.codec, cfg.width, cfg.height, ts);
                            frame_id = frame_id.wrapping_add(1);
                            if p.key {
                                idr.note_idr(now_ms);
                            }
                            keepalive.note_sent(now_ms);
                            fps_n += 1;
                            let Ok(permit) = inflight.clone().try_acquire_owned() else {
                                // The network can't keep up: drop and resync with an IDR.
                                force_key.store(true, Ordering::SeqCst);
                                continue;
                            };
                            let link = link.clone();
                            tokio::spawn(async move {
                                if let Ok(mut s) = link.open_uni(uni::VIDEO).await
                                    && write_frame(&mut s, &h, &p.data).await.is_ok() {
                                        let _ = s.finish();
                                    }
                                drop(permit);
                            });
                        }
                    }
                    CaptureOutput::Cursor(c) => {
                        if let Some(shape) = &c.shape
                            && c.shape_serial != last_shape_serial {
                                last_shape_serial = c.shape_serial;
                                link.send_control(pb::control::Msg::CursorShape(pb::CursorShape {
                                    serial: c.shape_serial,
                                    width: shape.width,
                                    height: shape.height,
                                    hot_x: shape.hot_x,
                                    hot_y: shape.hot_y,
                                    rgba: shape.rgba.clone(),
                                }));
                            }
                        link.send_control(pb::control::Msg::CursorPos(pb::CursorPos { x: c.x, y: c.y, visible: c.visible, serial: c.shape_serial }));
                    }
                }
                }
            }
            _ = tick.tick() => {
                let now_ms = epoch.elapsed().as_millis() as u64;
                if !sent_any && epoch.elapsed() > SOURCE_FIRST_FRAME_TIMEOUT {
                    let reason = last_err.take().unwrap_or_else(|| {
                        if captured == 0 {
                            "Die Bildschirmaufnahme am Laptop liefert keine Bilder".to_string()
                        } else {
                            format!("Der Video-Encoder liefert keine Daten ({captured} Bilder aufgenommen)")
                        }
                    });
                    tracing::warn!(reason, "focus source: nothing sent, giving up");
                    link.send_control(pb::control::Msg::FocusFailed(pb::FocusFailed { reason }));
                    break;
                }
                if keepalive.keepalive_due(now_ms) {
                    keepalive.note_sent(now_ms);
                    let h = keepalive_header(frame_id, cfg.codec, cfg.width, cfg.height, crate::clock::now_us());
                    frame_id = frame_id.wrapping_add(1);
                    let link = link.clone();
                    tokio::spawn(async move {
                        if let Ok(mut s) = link.open_uni(uni::VIDEO).await
                            && write_frame(&mut s, &h, &[]).await.is_ok() {
                                let _ = s.finish();
                            }
                    });
                }
                if idr.poll(now_ms) {
                    force_key.store(true, Ordering::SeqCst);
                }
                if sent_any && info_at.elapsed() >= Duration::from_secs(2) {
                    let msg = info.take(info_at.elapsed(), encoder.describe(), &adapter);
                    info_at = Instant::now();
                    link.send_control(pb::control::Msg::FocusSourceInfo(msg));
                }
                if last_abr.elapsed() >= Duration::from_millis(500) {
                    let secs = last_abr.elapsed().as_secs_f32().max(0.001);
                    last_abr = Instant::now();
                    let fb = *feedback.lock();
                    let sample = AbrSample {
                        loss_pct: fb.as_ref().map_or(0.0, |f| f.loss_pct),
                        rtt_ms: link.conn.rtt().as_secs_f32() * 1000.0,
                        encode_ms: if enc_n > 0 { enc_us_sum as f32 / enc_n as f32 / 1000.0 } else { 0.0 },
                        decode_ms: fb.as_ref().map_or(0.0, |f| f.decode_us as f32 / 1000.0),
                        fps: (fps_n as f32 / secs) as u32,
                    };
                    (enc_us_sum, enc_n, fps_n) = (0, 0, 0);
                    let d = abr.update(now_ms, &sample);
                    if d.changed {
                        let _ = encoder.set_bitrate(d.bitrate_kbps);
                    }
                }
            }
        }
    }
}

/// Shared flags between the actor and the source task.
struct SourceSignals {
    stop: Arc<AtomicBool>,
    force_key: Arc<AtomicBool>,
    feedback: Arc<Mutex<Option<pb::StreamStats>>>,
}

/// Counters behind `FocusSourceInfo`.
#[derive(Default)]
struct SourceInfoAcc {
    size: (u32, u32),
    captured: u64,
    encoded: u64,
    encode_us: u64,
    skipped: u64,
}

impl SourceInfoAcc {
    fn take(&mut self, over: Duration, encoder: String, adapter: &str) -> pb::FocusSourceInfo {
        let secs = over.as_secs_f64().max(0.001);
        let msg = pb::FocusSourceInfo {
            capture_width: self.size.0,
            capture_height: self.size.1,
            encoder,
            adapter: adapter.to_string(),
            capture_fps: (self.captured as f64 / secs).round() as u32,
            encoded_fps: (self.encoded as f64 / secs).round() as u32,
            encode_us: (self.encode_us / self.encoded.max(1)) as u32,
            skipped: self.skipped as u32,
        };
        let size = self.size;
        *self = Self::default();
        self.size = size;
        msg
    }
}

/// Needed by the actor for `Hello`: the pair of video capabilities.
pub(crate) fn hello_video_caps(
    backend: &dyn deskbridge_video::backend::VideoBackend,
    encoder_side: bool,
    viewer_side: bool,
) -> pb::VideoCaps {
    let e = if encoder_side {
        backend.encoder_caps()
    } else {
        pb::VideoCaps::default()
    };
    let d = if viewer_side {
        backend.decoder_caps()
    } else {
        pb::VideoCaps::default()
    };
    pb::VideoCaps {
        encode: e.encode,
        decode: d.decode,
        encode_yuv444: e.encode_yuv444,
        decode_yuv444: d.decode_yuv444,
        max_width: e.max_width.max(d.max_width),
        max_height: e.max_height.max(d.max_height),
        max_fps: e.max_fps.max(d.max_fps),
        encoder_name: e.encoder_name,
    }
}
