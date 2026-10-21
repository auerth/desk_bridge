//! Audio features: laptop sound → headset (F2) and headset microphone → laptop (F3).
//!
//! Roles (plan 4): the device with the headset (`headset_sink`) plays the stream and decides
//! whether it should flow (`AudioRoute`); the device that is the stream source sends loopback
//! audio. The microphone goes the other way and only flows while the virtual mic is in use.

use super::Engine;
use crate::ipc::{AudioDeviceDto, AudioFeatureDto, FeaturesDto, Live, UiEvent};
use bytes::Bytes;
use deskbridge_audio::{
    AudioReceiver, AudioSender, CodecPolicy, OpusApp, ReceiverConfig, SenderConfig,
};
use deskbridge_platform::audio::{
    AudioDeviceInfo, CaptureSource, DeviceEvent, DeviceKind, MicDefaults, OutputLevel, PullSource,
    StreamHandle, VirtualAudio,
};
use deskbridge_proto::media::{AudioPacket, STREAM_AUDIO_MIC, STREAM_AUDIO_SPEAKER};
use deskbridge_proto::pb;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

/// A running capture → sender → datagram pipeline.
struct SendPipe {
    _capture: Box<dyn StreamHandle>,
    task: tokio::task::JoinHandle<()>,
    peak: Arc<AtomicU32>,
    opus: Arc<AtomicBool>,
}

impl Drop for SendPipe {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl SendPipe {
    fn level(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }
}

struct Playback {
    _handle: Box<dyn StreamHandle>,
    device_id: Option<String>,
    device_name: Option<String>,
}

#[derive(Default)]
pub(crate) struct AudioState {
    // --- headset side (PC)
    speaker_rx: Option<Arc<AudioReceiver>>,
    playback: Option<Playback>,
    out_missing: bool,
    last_packet_opus: Arc<AtomicBool>,
    // --- stream source side (laptop)
    speaker_tx: Option<SendPipe>,
    /// Our own speakers were silenced by us: their state before we touched them.
    silenced_local: Option<Vec<OutputLevel>>,
    route_speaker_wanted: bool,
    // --- microphone: sender on the headset device, sink on the stream source
    mic_tx: Option<SendPipe>,
    mic_in_use: Option<(bool, Option<String>)>,
    mic_missing: bool,
    mic_rx: Option<Arc<AudioReceiver>>,
    mic_writer_stop: Option<Arc<AtomicBool>>,
    /// Virtual microphone stream clocked by the device (preferred over the writer thread).
    mic_playback: Option<Box<dyn StreamHandle>>,
    mic_watch_started: bool,
    /// Laptop: the headset microphone is switched on (route from the headset side).
    mic_route_wanted: bool,
    /// Laptop: the headset side wants the virtual mic managed (on/off + default); `None` until the
    /// first route arrived. Kept across disconnects.
    mic_manage: Option<bool>,
    /// Laptop: last state handed to `set_mic_enabled`: `(enabled, managed)`.
    mic_endpoint: Option<(bool, bool)>,
    /// Laptop: the defaults the virtual mic replaced (restored when the route ends).
    mic_default_prev: Option<MicDefaults>,
    /// PC: last `MicToLaptop` route sent to the laptop: `(enabled, manage_device)`.
    mic_route_sent: Option<(bool, bool)>,
    /// Laptop: packet count at the last microphone stats report.
    mic_stats_packets: u64,
    /// PC: last microphone playout report of the laptop.
    mic_stats: Option<pb::MicStats>,
    tone: Option<Box<dyn StreamHandle>>,
}

const MIC_UNAVAILABLE: &str = "Virtueller Mikrofon-Treiber nicht verfügbar";

fn codec_policy(s: &str) -> CodecPolicy {
    match s {
        "pcm" => CodecPolicy::Pcm,
        "opus" => CodecPolicy::Opus,
        _ => CodecPolicy::Auto,
    }
}

impl Engine {
    pub(super) fn peer_has_cap(&self, cap: &str) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.link.hello.caps.iter().any(|c| c.name == cap))
    }

    // ---------------------------------------------------------------------------------------
    // session lifecycle
    // ---------------------------------------------------------------------------------------

    pub(crate) fn start_audio(&mut self) {
        let Some(s) = &self.session else { return };
        let link = s.link.clone();

        // incoming datagrams: speaker audio (headset side) and microphone audio (stream source)
        let (dtx, mut drx) = unbounded_channel::<Bytes>();
        link.set_datagram_sink(Some(dtx));
        let rxs = Arc::new(parking_lot::RwLock::new((
            None::<Arc<AudioReceiver>>,
            None::<Arc<AudioReceiver>>,
        )));
        self.audio_rx_slots = Some(rxs.clone());
        let opus_flag = self.audio.last_packet_opus.clone();
        tokio::spawn(async move {
            while let Some(d) = drx.recv().await {
                let Ok(pkt) = AudioPacket::decode(d) else {
                    continue;
                };
                let slots = rxs.read();
                match pkt.stream_id {
                    STREAM_AUDIO_SPEAKER => {
                        if let Some(r) = &slots.0 {
                            if !pkt.is_silence() {
                                opus_flag.store(pkt.is_opus(), Ordering::Relaxed);
                            }
                            r.on_packet(pkt);
                        }
                    }
                    STREAM_AUDIO_MIC => {
                        if let Some(r) = &slots.1 {
                            r.on_packet(pkt);
                        }
                    }
                    _ => {}
                }
            }
        });

        if self.cfg.device.headset_sink {
            self.start_speaker_sink();
        }
        if self.cfg.device.stream_source {
            self.start_mic_sink();
        }
        self.sync_mic_sender();
    }

    pub(crate) fn stop_audio(&mut self) {
        if let Some(s) = &self.session {
            s.link.set_datagram_sink(None);
        }
        self.audio_rx_slots = None;
        self.drop_speaker_source();
        self.audio.route_speaker_wanted = false;
        self.audio.playback = None;
        self.audio.speaker_rx = None;
        self.audio.mic_tx = None;
        self.audio.mic_in_use = None;
        if let Some(stop) = self.audio.mic_writer_stop.take() {
            stop.store(true, Ordering::SeqCst);
        }
        self.audio.mic_playback = None;
        self.audio.mic_rx = None;
        self.audio.mic_route_sent = None;
        self.audio.mic_route_wanted = false;
        self.sync_mic_default();
        self.audio.tone = None;
    }

    // ---------------------------------------------------------------------------------------
    // speaker: headset side (PC)
    // ---------------------------------------------------------------------------------------

    fn start_speaker_sink(&mut self) {
        let wired = true;
        let Ok(rx) = AudioReceiver::new(ReceiverConfig {
            channels_in: 2,
            wired,
            min_ms: 5.0,
            max_ms: 60.0,
        }) else {
            tracing::error!("audio receiver could not be created");
            return;
        };
        let rx = Arc::new(rx);
        rx.set_volume(self.cfg.audio.laptop_to_pc.volume);
        rx.set_muted(self.cfg.audio.laptop_to_pc.mute);
        self.audio.speaker_rx = Some(rx.clone());
        if let Some(slots) = &self.audio_rx_slots {
            slots.write().0 = Some(rx);
        }
        self.restart_playback();
    }

    /// (Re)opens the render stream on the configured device; keeps the stream paused if the
    /// device is absent (powered-off headset) and tells the source to stop sending meanwhile.
    pub(crate) fn restart_playback(&mut self) {
        let Some(rx) = self.audio.speaker_rx.clone() else {
            return;
        };
        self.audio.playback = None;
        if !self.cfg.audio.laptop_to_pc.enabled {
            self.send_route(pb::Stream::LaptopToPc, false);
            return;
        }
        let want = self.cfg.audio.laptop_to_pc.output_device.clone();
        let device = if want.is_empty() {
            None
        } else {
            Some(want.clone())
        };
        // A configured device that is currently absent → paused, never silently another device.
        if let Some(id) = &device {
            let present = self
                .platform
                .audio_devices
                .list(DeviceKind::Render)
                .map(|l| l.iter().any(|d| &d.id == id))
                .unwrap_or(false);
            if !present {
                self.audio.out_missing = true;
                self.send_route(pb::Stream::LaptopToPc, false);
                self.dirty = true;
                return;
            }
        }
        rx.reset();
        match self
            .platform
            .audio_playback
            .start(device.as_deref(), rx.source())
        {
            Ok(h) => {
                let name = self.device_name(DeviceKind::Render, device.as_deref());
                self.audio.playback = Some(Playback {
                    _handle: h,
                    device_id: device,
                    device_name: name,
                });
                self.audio.out_missing = false;
                self.send_route(pb::Stream::LaptopToPc, true);
            }
            Err(e) => {
                tracing::warn!("opening render device failed: {e}");
                self.audio.out_missing = true;
                self.send_route(pb::Stream::LaptopToPc, false);
            }
        }
        self.dirty = true;
    }

    fn device_name(&self, kind: DeviceKind, id: Option<&str>) -> Option<String> {
        let list = self.platform.audio_devices.list(kind).ok()?;
        match id {
            Some(id) => list.into_iter().find(|d| d.id == id).map(|d| d.name),
            None => list.into_iter().find(|d| d.is_default).map(|d| d.name),
        }
    }

    pub(crate) fn send_route(&self, stream: pb::Stream, enabled: bool) {
        if let Some(s) = &self.session {
            s.link
                .send_control(pb::control::Msg::AudioRoute(pb::AudioRoute {
                    stream: stream as i32,
                    enabled,
                    manage_device: false,
                }));
        }
    }

    // ---------------------------------------------------------------------------------------
    // speaker: source side (laptop)
    // ---------------------------------------------------------------------------------------

    fn start_speaker_source(&mut self) {
        if self.audio.speaker_tx.is_some() {
            return;
        }
        let Some(s) = &self.session else { return };
        let link = s.link.clone();
        let cfg = &self.cfg.audio.laptop_to_pc;
        let Ok(sender) = AudioSender::new(SenderConfig {
            stream_id: STREAM_AUDIO_SPEAKER,
            channels: 2,
            codec: codec_policy(&cfg.codec),
            opus_bitrate_bps: cfg.opus_bitrate_kbps * 1000,
            opus_app: OpusApp::LowDelay,
        }) else {
            return;
        };
        let (tx, rx) = unbounded_channel();
        let capture = match self
            .platform
            .audio_capture
            .start(CaptureSource::Loopback(None), tx)
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("loopback capture failed: {e}");
                self.warn_once(
                    "audio_capture",
                    &format!("Laptop-Ton konnte nicht erfasst werden: {e}"),
                );
                return;
            }
        };
        let peak = Arc::new(AtomicU32::new(0));
        let opus = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn(run_sender_owned(
            link,
            sender,
            rx,
            peak.clone(),
            opus.clone(),
        ));
        self.audio.speaker_tx = Some(SendPipe {
            _capture: capture,
            task,
            peak,
            opus,
        });
        self.silence_local_speakers();
    }

    /// Stops sending the laptop sound and gives the laptop its own speakers back.
    pub(crate) fn drop_speaker_source(&mut self) {
        self.audio.speaker_tx = None;
        self.restore_local_speakers();
    }

    fn speaker_backup_file(&self) -> std::path::PathBuf {
        self.paths.data_dir.join("speaker-mute.json")
    }

    /// While the sound is forwarded to the other device, the local speakers stay quiet (volume 0,
    /// not muted: the mute would silence the forwarded loopback too). The previous state is
    /// remembered (also on disk, so a crash cannot leave the laptop silent).
    fn silence_local_speakers(&mut self) {
        if !self.cfg.audio.laptop_to_pc.mute_local_output || self.audio.silenced_local.is_some() {
            return;
        }
        match self.platform.local_output.silence_forwarded_outputs() {
            Ok(list) => {
                // `prev` is the mute state (key kept from the versions that only muted).
                let devices: Vec<_> = list
                    .iter()
                    .map(|o| serde_json::json!({ "id": o.id, "prev": o.muted, "volume": o.volume }))
                    .collect();
                let _ = std::fs::write(
                    self.speaker_backup_file(),
                    serde_json::json!({ "devices": devices }).to_string(),
                );
                self.audio.silenced_local = Some(list);
            }
            Err(e) => tracing::warn!("could not silence the local speakers: {e}"),
        }
    }

    fn restore_local_speakers(&mut self) {
        if let Some(list) = self.audio.silenced_local.take() {
            for prev in &list {
                if let Err(e) = self.platform.local_output.restore_output(prev) {
                    tracing::warn!("could not restore the local speakers: {e}");
                }
            }
            let _ = std::fs::remove_file(self.speaker_backup_file());
        }
    }

    // ---------------------------------------------------------------------------------------
    // microphone
    // ---------------------------------------------------------------------------------------

    /// Stream source side: set up the sink into the virtual microphone, if a driver exists.
    fn start_mic_sink(&mut self) {
        let va = self.platform.virtual_audio.clone();
        if !va.available() || !self.peer_has_cap(deskbridge_proto::caps::AUDIO_HEADSET) {
            return;
        }
        // The laptop is often on Wi-Fi, where downlink packets can arrive in bursts: allow the
        // buffer to grow further than towards the PC instead of running dry again and again
        // (it only grows when the measured jitter demands it and shrinks back slowly).
        let Ok(rx) = AudioReceiver::new(ReceiverConfig {
            channels_in: 1,
            wired: true,
            min_ms: 5.0,
            max_ms: 150.0,
        }) else {
            return;
        };
        let rx = Arc::new(rx);
        if let Some(slots) = &self.audio_rx_slots {
            slots.write().1 = Some(rx.clone());
        }
        self.audio.mic_rx = Some(rx.clone());
        // Preferred: the virtual microphone's own render stream pulls from the jitter buffer.
        let src: Arc<dyn PullSource> = rx.source();
        match va.start_mic_playback(src.clone()) {
            Ok(h) => self.audio.mic_playback = Some(h),
            Err(e) => {
                tracing::debug!("device-clocked virtual mic not available ({e}); writer thread");
                self.spawn_mic_writer(va.clone(), src);
            }
        }
        // tell the headset side whenever an app opens/closes the virtual microphone
        if !self.audio.mic_watch_started {
            self.audio.mic_watch_started = true;
            let (utx, mut urx) = unbounded_channel();
            if va.watch_mic_in_use(utx).is_ok() {
                let m = self.msg_tx.clone();
                tokio::spawn(async move {
                    while let Some(u) = urx.recv().await {
                        if m.send(super::Msg::MicUsage(u)).is_err() {
                            break;
                        }
                    }
                });
            }
        }
        if let Some(s) = &self.session {
            s.link
                .send_control(pb::control::Msg::MicInUse(pb::MicInUse {
                    in_use: va.mic_in_use(),
                    process: String::new(),
                }));
        }
    }

    /// Fallback without a device-clocked stream: 10 ms blocks into the virtual microphone. A late
    /// wake-up (timer granularity) is caught up immediately instead of dropping blocks.
    fn spawn_mic_writer(&mut self, va: Arc<dyn VirtualAudio>, src: Arc<dyn PullSource>) {
        let stop = Arc::new(AtomicBool::new(false));
        self.audio.mic_writer_stop = Some(stop.clone());
        std::thread::Builder::new()
            .name("virtual-mic-writer".into())
            .spawn(move || {
                let mut stereo = vec![0.0f32; 480 * 2];
                let mut mono = vec![0.0f32; 480];
                let mut next = std::time::Instant::now();
                while !stop.load(Ordering::SeqCst) {
                    src.fill(&mut stereo);
                    for (i, m) in mono.iter_mut().enumerate() {
                        *m = 0.5 * (stereo[i * 2] + stereo[i * 2 + 1]);
                    }
                    va.write_mic(&mono);
                    next += Duration::from_millis(10);
                    let now = std::time::Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    } else if now - next > Duration::from_millis(100) {
                        next = now;
                    }
                }
            })
            .ok();
    }

    fn mic_default_backup_file(&self) -> std::path::PathBuf {
        self.paths.data_dir.join("mic-default.json")
    }

    /// Laptop: while the headset microphone is switched on, "CABLE Output" is enabled and the
    /// default recording device, so Teams & co. use it. Otherwise (mic switched off, disconnected)
    /// the previous defaults come back and the endpoint is disabled, so apps fall back to the
    /// laptop's own microphone; while the agent is not running the service keeps it disabled.
    /// Only with "set as default" on the headset side (`manage_device`); without it the endpoint
    /// is left enabled and the defaults untouched. The previous defaults are also kept on disk,
    /// so a crash cannot leave the laptop on the virtual microphone.
    pub(crate) fn sync_mic_default(&mut self) {
        if !self.cfg.device.stream_source {
            return;
        }
        let Some(manage) = self.audio.mic_manage else {
            return;
        };
        let va = self.platform.virtual_audio.clone();
        if !va.available() {
            return;
        }
        let on = self.session.is_some() && self.audio.mic_route_wanted;
        let claim = manage && on;
        let endpoint = (!manage || on, manage);
        if !claim && let Some(prev) = self.audio.mic_default_prev.take() {
            if let Err(e) = va.release_default_mic(&prev) {
                tracing::warn!("could not restore the default microphone: {e}");
            }
            let _ = std::fs::remove_file(self.mic_default_backup_file());
        }
        if self.audio.mic_endpoint != Some(endpoint) {
            match va.set_mic_enabled(endpoint.0, endpoint.1) {
                Ok(()) => self.audio.mic_endpoint = Some(endpoint),
                Err(e) => tracing::warn!("could not switch the virtual microphone: {e}"),
            }
        }
        if claim && self.audio.mic_default_prev.is_none() {
            match va.claim_default_mic() {
                Ok(prev) => {
                    let _ = std::fs::write(
                        self.mic_default_backup_file(),
                        serde_json::json!({
                            "console": prev.console,
                            "communications": prev.communications,
                        })
                        .to_string(),
                    );
                    self.audio.mic_default_prev = Some(prev);
                }
                Err(e) => tracing::warn!("could not make the virtual mic the default: {e}"),
            }
        }
    }

    /// Laptop: every 2 s while the microphone stream flows, report its playout health to the
    /// headset side (logged there, so problems on the laptop are visible on the PC).
    pub(crate) fn report_mic_stats(&mut self) {
        let (Some(rx), Some(s)) = (&self.audio.mic_rx, &self.session) else {
            return;
        };
        let st = rx.stats();
        if st.packets == self.audio.mic_stats_packets {
            return;
        }
        self.audio.mic_stats_packets = st.packets;
        s.link
            .send_control(pb::control::Msg::MicStats(pb::MicStats {
                fill_ms: st.fill_ms,
                target_ms: st.target_ms,
                jitter_ms: st.jitter_ms,
                loss_pct: st.loss_pct,
                underruns: st.underruns,
                overruns: st.overruns,
                plc: st.plc_count,
                drift_ppm: st.drift_ppm,
                output: self
                    .platform
                    .virtual_audio
                    .mic_output_info()
                    .unwrap_or_default(),
            }));
    }

    pub(crate) fn on_mic_usage(&mut self, u: deskbridge_platform::audio::MicUsage) {
        if let Some(s) = &self.session {
            s.link
                .send_control(pb::control::Msg::MicInUse(pb::MicInUse {
                    in_use: u.in_use,
                    process: u.process.unwrap_or_default(),
                }));
        }
    }

    /// Headset side: start/stop the microphone sender according to usage, config and device.
    pub(crate) fn sync_mic_sender(&mut self) {
        if !self.cfg.device.headset_sink || self.session.is_none() {
            return;
        }
        let supported = self.peer_has_cap(deskbridge_proto::caps::AUDIO_MIC_SINK);
        let cfg = self.cfg.audio.mic_to_laptop.clone();
        let in_use = self.audio.mic_in_use.as_ref().is_some_and(|m| m.0);
        let should = supported
            && cfg.enabled
            && !cfg.mute
            && !self.audio.mic_missing
            && (in_use || !cfg.only_when_used);
        if should && self.audio.mic_tx.is_none() {
            self.start_mic_sender(&cfg.input_device);
        } else if !should && self.audio.mic_tx.is_some() {
            self.audio.mic_tx = None;
        }
        // The laptop offers its virtual mic only while the headset mic is switched on and present.
        let route = (
            supported && cfg.enabled && !self.audio.mic_missing,
            cfg.set_as_default,
        );
        if self.audio.mic_route_sent != Some(route)
            && let Some(s) = &self.session
        {
            self.audio.mic_route_sent = Some(route);
            s.link
                .send_control(pb::control::Msg::AudioRoute(pb::AudioRoute {
                    stream: pb::Stream::MicToLaptop as i32,
                    enabled: route.0,
                    manage_device: route.1,
                }));
        }
        self.dirty = true;
    }

    fn start_mic_sender(&mut self, device: &str) {
        let Some(s) = &self.session else { return };
        let link = s.link.clone();
        let Ok(sender) = AudioSender::new(SenderConfig {
            stream_id: STREAM_AUDIO_MIC,
            channels: 1,
            codec: CodecPolicy::Auto,
            opus_bitrate_bps: 64_000,
            opus_app: OpusApp::LowDelay,
        }) else {
            return;
        };
        let (tx, rx) = unbounded_channel();
        let dev = if device.is_empty() {
            None
        } else {
            Some(device.to_string())
        };
        // A configured microphone that is absent must NOT be replaced by another one.
        if let Some(id) = &dev {
            let present = self
                .platform
                .audio_devices
                .list(DeviceKind::Capture)
                .map(|l| l.iter().any(|d| &d.id == id))
                .unwrap_or(false);
            if !present {
                self.audio.mic_missing = true;
                return;
            }
        }
        let capture = match self
            .platform
            .audio_capture
            .start(CaptureSource::Microphone(dev), tx)
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("microphone capture failed: {e}");
                self.audio.mic_missing = true;
                return;
            }
        };
        let peak = Arc::new(AtomicU32::new(0));
        let opus = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn(run_sender_owned(
            link,
            sender,
            rx,
            peak.clone(),
            opus.clone(),
        ));
        self.audio.mic_tx = Some(SendPipe {
            _capture: capture,
            task,
            peak,
            opus,
        });
    }

    // ---------------------------------------------------------------------------------------
    // control messages & events
    // ---------------------------------------------------------------------------------------

    pub(crate) fn on_audio_control(&mut self, msg: &pb::control::Msg) -> bool {
        use pb::control::Msg as M;
        match msg {
            M::AudioRoute(r) => {
                if r.stream == pb::Stream::LaptopToPc as i32 && self.cfg.device.stream_source {
                    self.audio.route_speaker_wanted = r.enabled;
                    if r.enabled && self.cfg.audio.laptop_to_pc.enabled {
                        self.start_speaker_source();
                    } else {
                        self.drop_speaker_source();
                    }
                    self.dirty = true;
                }
                if r.stream == pb::Stream::MicToLaptop as i32 && self.cfg.device.stream_source {
                    self.audio.mic_route_wanted = r.enabled;
                    self.audio.mic_manage = Some(r.manage_device);
                    self.sync_mic_default();
                }
                true
            }
            M::MicInUse(m) => {
                self.audio.mic_in_use =
                    Some((m.in_use, (!m.process.is_empty()).then(|| m.process.clone())));
                self.sync_mic_sender();
                true
            }
            M::MicStats(m) => {
                let glitches = |x: &pb::MicStats| x.underruns + x.overruns + x.plc;
                let new = self
                    .audio
                    .mic_stats
                    .as_ref()
                    .map_or(u64::MAX, |p| glitches(m).saturating_sub(glitches(p)));
                if new > 0 {
                    tracing::info!(
                        fill_ms = m.fill_ms,
                        target_ms = m.target_ms,
                        jitter_ms = m.jitter_ms,
                        loss_pct = m.loss_pct,
                        underruns = m.underruns,
                        overruns = m.overruns,
                        plc = m.plc,
                        drift_ppm = m.drift_ppm,
                        output = %m.output,
                        "laptop microphone playout"
                    );
                }
                self.audio.mic_stats = Some(m.clone());
                self.dirty = true;
                true
            }
            M::AudioCodec(c) => {
                let _ = c;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn on_audio_device_event(&mut self, ev: DeviceEvent) {
        if self.cfg.device.stream_source && self.audio.speaker_tx.is_some() {
            self.on_forwarded_output_event(&ev);
        }
        if !self.cfg.device.headset_sink {
            return;
        }
        let want_out = self.cfg.audio.laptop_to_pc.output_device.clone();
        let want_mic = self.cfg.audio.mic_to_laptop.input_device.clone();
        match ev {
            DeviceEvent::Removed { id } => {
                if !want_out.is_empty() && id == want_out && self.audio.playback.is_some() {
                    tracing::info!("headset output removed, pausing audio");
                    self.audio.playback = None;
                    self.audio.out_missing = true;
                    self.send_route(pb::Stream::LaptopToPc, false);
                }
                if !want_mic.is_empty() && id == want_mic && self.audio.mic_tx.is_some() {
                    self.audio.mic_tx = None;
                    self.audio.mic_missing = true;
                }
            }
            DeviceEvent::Added(d) => {
                if self.audio.out_missing
                    && d.kind == DeviceKind::Render
                    && (want_out.is_empty() || d.id == want_out)
                {
                    self.restart_playback();
                }
                if self.audio.mic_missing
                    && d.kind == DeviceKind::Capture
                    && (want_mic.is_empty() || d.id == want_mic)
                {
                    self.audio.mic_missing = false;
                    self.sync_mic_sender();
                }
            }
            DeviceEvent::DefaultChanged { kind, .. } => {
                // Following the system default: reopen on the new default.
                if kind == DeviceKind::Render
                    && want_out.is_empty()
                    && self.audio.speaker_rx.is_some()
                {
                    self.restart_playback();
                }
            }
        }
        self.dirty = true;
    }

    /// Source side: the loopback mixes every real output of this device (Teams & co. often play
    /// on another one than the default). A new output, a vanished one or a new default changes
    /// that set, so the capture is reopened.
    fn on_forwarded_output_event(&mut self, ev: &DeviceEvent) {
        let render = match ev {
            DeviceEvent::Added(d) => d.kind == DeviceKind::Render,
            // Render endpoint ids start with "{0.0.0.", capture ones with "{0.0.1." (the
            // virtual microphone being switched off must not interrupt the laptop sound).
            DeviceEvent::Removed { id } => !id.starts_with("{0.0.1."),
            DeviceEvent::DefaultChanged { kind, .. } => *kind == DeviceKind::Render,
        };
        if !render {
            return;
        }
        tracing::info!(
            ?ev,
            "audio outputs changed, reopening the laptop sound capture"
        );
        self.drop_speaker_source();
        self.start_speaker_source();
    }

    // ---------------------------------------------------------------------------------------
    // hotkeys, config, UI
    // ---------------------------------------------------------------------------------------

    pub(crate) fn toggle_laptop_mute(&mut self) {
        self.cfg.audio.laptop_to_pc.mute = !self.cfg.audio.laptop_to_pc.mute;
        let (icon, text) = if self.cfg.audio.laptop_to_pc.mute {
            ("speaker-off", "Laptop-Ton im Headset stumm")
        } else {
            ("speaker", "Laptop-Ton im Headset an")
        };
        self.emit(UiEvent::Osd {
            icon: icon.into(),
            text: text.into(),
        });
        self.persist_config();
        self.apply_audio_config();
    }

    pub(crate) fn toggle_mic_mute(&mut self) {
        self.cfg.audio.mic_to_laptop.mute = !self.cfg.audio.mic_to_laptop.mute;
        let (icon, text) = if self.cfg.audio.mic_to_laptop.mute {
            ("mic-off", "Mikro stumm")
        } else {
            ("mic", "Mikro aktiv")
        };
        self.emit(UiEvent::Osd {
            icon: icon.into(),
            text: text.into(),
        });
        self.persist_config();
        self.apply_audio_config();
    }

    pub(crate) fn apply_audio_config(&mut self) {
        let c = self.cfg.audio.laptop_to_pc.clone();
        if let Some(rx) = &self.audio.speaker_rx {
            rx.set_volume(c.volume);
            rx.set_muted(c.mute);
        }
        if self.cfg.device.headset_sink && self.session.is_some() {
            let current = self
                .audio
                .playback
                .as_ref()
                .map(|p| p.device_id.clone().unwrap_or_default());
            let device_changed = current.is_some_and(|d| d != c.output_device);
            if device_changed
                || (c.enabled && self.audio.playback.is_none())
                || (!c.enabled && self.audio.playback.is_some())
            {
                self.audio.out_missing = false;
                self.restart_playback();
            }
            self.audio.mic_missing = false;
            // device switch for the mic: restart sender
            self.audio.mic_tx = None;
            self.sync_mic_sender();
        }
        if self.cfg.device.stream_source && self.audio.speaker_tx.is_some() && !c.enabled {
            self.drop_speaker_source();
        } else if self.audio.speaker_tx.is_some() {
            if c.mute_local_output {
                self.silence_local_speakers();
            } else {
                self.restore_local_speakers();
            }
        }
    }

    pub(crate) fn list_audio_devices(&self) -> Result<serde_json::Value, String> {
        let conv = |l: Vec<AudioDeviceInfo>| -> Vec<AudioDeviceDto> {
            l.into_iter()
                .map(|d| AudioDeviceDto {
                    id: d.id,
                    name: d.name,
                    is_default: d.is_default,
                    is_default_communications: d.is_default_communications,
                    is_virtual: d.is_virtual,
                })
                .collect()
        };
        let render = conv(
            self.platform
                .audio_devices
                .list(DeviceKind::Render)
                .map_err(|e| e.to_string())?,
        );
        let capture = conv(
            self.platform
                .audio_devices
                .list(DeviceKind::Capture)
                .map_err(|e| e.to_string())?,
        );
        self.emit(UiEvent::AudioDevices { render, capture });
        Ok(serde_json::json!({ "ok": true }))
    }

    /// 1 s test tone on the configured output ("so klingt der Laptop im Headset").
    pub(crate) fn play_test_tone(&mut self) -> Result<serde_json::Value, String> {
        struct Tone(std::sync::atomic::AtomicU64);
        impl PullSource for Tone {
            fn fill(&self, out: &mut [f32]) {
                let mut n = self.0.load(Ordering::Relaxed);
                for f in out.chunks_exact_mut(2) {
                    let t = n as f32 / 48_000.0;
                    // 440 Hz with a short fade in/out, quiet
                    let env = (t.min(1.0 - t).clamp(0.0, 0.05)) / 0.05;
                    let v = 0.08 * env * (2.0 * std::f32::consts::PI * 440.0 * t).sin();
                    f[0] = v;
                    f[1] = v;
                    n += 1;
                }
                self.0.store(n, Ordering::Relaxed);
            }
        }
        let want = self.cfg.audio.laptop_to_pc.output_device.clone();
        let dev = if want.is_empty() { None } else { Some(want) };
        let h = self
            .platform
            .audio_playback
            .start(
                dev.as_deref(),
                Arc::new(Tone(std::sync::atomic::AtomicU64::new(0))),
            )
            .map_err(|e| format!("Testton konnte nicht abgespielt werden: {e}"))?;
        self.audio.tone = Some(h);
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1100)).await;
            let _ = tx.send(super::Msg::ToneDone);
        });
        Ok(serde_json::json!({ "ok": true }))
    }

    pub(crate) fn fill_audio_state(&self, f: &mut FeaturesDto) {
        let cfg = &self.cfg.audio;
        let connected = self.session.is_some();

        // --- laptop → headset
        let mut a = AudioFeatureDto::default();
        a.base.enabled = cfg.laptop_to_pc.enabled;
        a.muted = cfg.laptop_to_pc.mute;
        a.base.state = if !cfg.laptop_to_pc.enabled {
            Live::Off
        } else {
            Live::Ready
        };
        if let Some(rx) = &self.audio.speaker_rx {
            let st = rx.stats();
            a.level = rx.peak_level();
            a.buffer_ms = Some(st.fill_ms);
            a.latency_ms = Some(st.fill_ms + 10.0);
            a.codec = if st.packets == 0 {
                String::new()
            } else if self.audio.last_packet_opus.load(Ordering::Relaxed) {
                "opus".into()
            } else {
                "pcm".into()
            };
            a.device_name = self
                .audio
                .playback
                .as_ref()
                .and_then(|p| p.device_name.clone());
            a.device_missing = self.audio.out_missing;
            if cfg.laptop_to_pc.enabled
                && st.packets > 0
                && matches!(st.state, deskbridge_audio::PlayState::Playing)
            {
                a.base.state = Live::Live;
            }
            if self.audio.out_missing {
                a.base.detail = Some("Headset nicht verfügbar – Wiedergabe pausiert".into());
            } else if cfg.laptop_to_pc.mute && a.base.state == Live::Live {
                // The meter shows the incoming signal; say plainly that nothing reaches the ear.
                a.base.detail = Some("Stummgeschaltet – im Headset ist nichts zu hören".into());
            }
        } else if let Some(p) = &self.audio.speaker_tx {
            a.level = p.level();
            a.codec = if p.opus.load(Ordering::Relaxed) {
                "opus".into()
            } else {
                "pcm".into()
            };
            if cfg.laptop_to_pc.enabled {
                a.base.state = Live::Live;
            }
        } else if !connected {
            a.base.state = if cfg.laptop_to_pc.enabled {
                Live::Ready
            } else {
                Live::Off
            };
        }
        f.laptop_audio = a;

        // --- headset microphone → laptop
        let mut m = AudioFeatureDto::default();
        m.base.enabled = cfg.mic_to_laptop.enabled;
        m.muted = cfg.mic_to_laptop.mute;
        m.base.state = if !cfg.mic_to_laptop.enabled {
            Live::Off
        } else {
            Live::Ready
        };
        if self.cfg.device.headset_sink {
            if connected && !self.peer_has_cap(deskbridge_proto::caps::AUDIO_MIC_SINK) {
                m.base.state = Live::Unavailable;
                m.base.detail = Some(MIC_UNAVAILABLE.into());
            }
            if let Some(p) = &self.audio.mic_tx {
                m.level = p.level();
                m.base.state = Live::Live;
                m.codec = if p.opus.load(Ordering::Relaxed) {
                    "opus".into()
                } else {
                    "pcm".into()
                };
                if let Some((true, Some(name))) = &self.audio.mic_in_use {
                    m.base.detail = Some(format!("Laptop nutzt dein Mikro ({name})"));
                } else {
                    m.base.detail = Some("Laptop nutzt dein Mikro".into());
                }
                if let (Some(d), Some(st)) = (&mut m.base.detail, &self.audio.mic_stats) {
                    d.push_str(&format!(
                        " · Laptop-Puffer {:.0} ms, {} Aussetzer",
                        st.fill_ms,
                        st.underruns + st.overruns
                    ));
                }
            }
            m.device_missing = self.audio.mic_missing;
        } else if !self.platform.virtual_audio.available() {
            m.base.state = Live::Unavailable;
            m.base.detail = Some(MIC_UNAVAILABLE.into());
        } else if self.platform.virtual_audio.mic_in_use() {
            m.base.state = Live::Live;
        }
        f.mic = m;
    }

    pub(crate) fn audio_metrics(&self) -> (Option<f32>, Option<f32>, Option<f32>) {
        match &self.audio.speaker_rx {
            Some(rx) => {
                let s = rx.stats();
                (Some(s.fill_ms), Some(s.drift_ppm), Some(s.loss_pct))
            }
            None => (None, None, None),
        }
    }

    pub(crate) fn audio_is_active(&self) -> bool {
        self.audio.speaker_rx.is_some()
            || self.audio.speaker_tx.is_some()
            || self.audio.mic_tx.is_some()
    }

    pub(crate) fn on_tone_done(&mut self) {
        self.audio.tone = None;
    }
}

// --- sender helper tasks -----------------------------------------------------------------------

async fn run_sender_owned(
    link: super::session::PeerLink,
    mut sender: AudioSender,
    mut rx: UnboundedReceiver<deskbridge_platform::audio::AudioFrame>,
    peak: Arc<AtomicU32>,
    opus: Arc<AtomicBool>,
) {
    let mut link_tick = tokio::time::interval(Duration::from_millis(500));
    let (mut last_sent, mut last_lost) = (0u64, 0u64);
    loop {
        tokio::select! {
            f = rx.recv() => {
                let Some(f) = f else { break };
                for d in sender.push(&f.samples, f.ts_us) {
                    let _ = link.send_datagram(d);
                }
                let p = sender.peak_level();
                let prev = f32::from_bits(peak.load(Ordering::Relaxed));
                peak.store(p.max(prev).to_bits(), Ordering::Relaxed);
                opus.store(sender.current_codec_is_opus(), Ordering::Relaxed);
            }
            _ = link_tick.tick() => {
                let st = link.conn.stats();
                let sent = st.path.sent_packets;
                let lost = st.path.lost_packets;
                let (ds, dl) = (sent.saturating_sub(last_sent), lost.saturating_sub(last_lost));
                (last_sent, last_lost) = (sent, lost);
                let loss = if ds > 0 { dl as f32 * 100.0 / ds as f32 } else { 0.0 };
                sender.set_link(loss, link.conn.rtt().as_micros().min(u128::from(u32::MAX)) as u32);
            }
        }
    }
}
