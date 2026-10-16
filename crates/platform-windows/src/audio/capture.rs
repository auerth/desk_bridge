//! Event-driven WASAPI capture (microphone and render-endpoint loopback).

use super::devices::forwarded_render_endpoints;
use super::stream::{EndReason, StreamEvent, StreamThread, spawn_stream};
use super::util::{SilenceFiller, downmix_to_mono, frames_to_us, qpc_100ns_to_us};
use super::wasapi::{Event, float_format, is_device_lost, open_device, os_ctx, os_err, qpc_now_us};
use deskbridge_platform::audio::{
    AudioCapture, AudioFrame, CaptureSource, DeviceKind, RATE, StreamHandle,
};
use deskbridge_platform::{Result, UnboundedSender};
use std::collections::VecDeque;
use windows::Win32::Foundation::{WAIT_FAILED, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioCaptureClient,
    IAudioClient, IMMDevice, WAVEFORMATEX, eCommunications, eConsole,
};
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::Win32::System::Threading::WaitForMultipleObjects;

/// Largest frame handed to the sink (10 ms at 48 kHz); bigger WASAPI packets are split.
const MAX_CHUNK_FRAMES: usize = (RATE / 100) as usize;
/// Wake-up interval when no event arrives (also the silence-fill cadence and the device-loss
/// probe interval).
const WAIT_MS: u32 = 10;
/// Loopback produces nothing while the render endpoint is idle; fill after this much silence.
const SILENCE_AFTER_US: u64 = 20_000;
/// Mixed loopback: an output joins the mix once this much of it is buffered (absorbs packet
/// jitter between the endpoints).
const MIX_PREBUF_FRAMES: usize = 720;
/// Mixed loopback: an output whose backlog grows beyond this (clock drift) is trimmed back.
const MIX_MAX_BACKLOG_FRAMES: usize = 2_400;
/// Mixed loopback: smallest chunk emitted per wake-up (5 ms).
const MIX_MIN_EMIT_FRAMES: usize = 240;

/// Dropping the handle stops the capture thread and joins it.
pub struct CaptureHandle {
    _thread: StreamThread,
}

impl StreamHandle for CaptureHandle {}

/// WASAPI [`AudioCapture`]. Stateless; every `start` spawns its own worker thread.
#[derive(Debug, Default, Clone, Copy)]
pub struct WasapiCapture;

impl WasapiCapture {
    pub fn new() -> Self {
        Self
    }

    /// Like [`AudioCapture::start`], additionally reporting [`StreamEvent`]s (started, device
    /// lost, stopped) on `lifecycle_tx`.
    pub fn start_with_events(
        &self,
        source: CaptureSource,
        sink: UnboundedSender<AudioFrame>,
        lifecycle_tx: UnboundedSender<StreamEvent>,
    ) -> Result<Box<dyn StreamHandle>> {
        start_inner(source, sink, Some(lifecycle_tx))
    }
}

impl AudioCapture for WasapiCapture {
    fn start(
        &self,
        source: CaptureSource,
        sink: UnboundedSender<AudioFrame>,
    ) -> Result<Box<dyn StreamHandle>> {
        start_inner(source, sink, None)
    }
}

fn start_inner(
    source: CaptureSource,
    sink: UnboundedSender<AudioFrame>,
    lifecycle: Option<UnboundedSender<StreamEvent>>,
) -> Result<Box<dyn StreamHandle>> {
    let name = match &source {
        CaptureSource::Loopback(_) => "db-audio-loopback",
        CaptureSource::Microphone(_) => "db-audio-mic",
    };
    let (thread, ()) = spawn_stream(
        name,
        lifecycle,
        move || open_capture(&source).map(|st| (st, ())),
        move |st, stop| match st {
            Opened::Single(st) => run_capture(st, stop, &sink),
            Opened::Mix(sources) => run_mix(sources, stop, &sink),
        },
    )?;
    Ok(Box::new(CaptureHandle { _thread: thread }))
}

/// Everything the capture loop needs; created and dropped on the worker thread.
struct CaptureState {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: Event,
    /// Channels delivered by the device in the autoconverted format.
    device_channels: usize,
    /// Channels handed to the sink (1 for microphone, 2 for loopback).
    out_channels: usize,
}

impl Drop for CaptureState {
    fn drop(&mut self) {
        // SAFETY: the client is valid; Stop on an already stopped / invalidated client is harmless.
        let _ = unsafe { self.client.Stop() };
    }
}

/// One loopback stream inside a mixed loopback.
struct MixSource {
    name: String,
    st: CaptureState,
    /// Interleaved stereo samples not yet mixed.
    fifo: VecDeque<f32>,
    /// Contributing to the mix (enough buffered); reset on underrun.
    primed: bool,
}

impl MixSource {
    /// Reads all pending packets into the FIFO.
    fn drain(&mut self) -> windows::core::Result<()> {
        loop {
            // SAFETY: capture client is valid; calls follow the GetBuffer/ReleaseBuffer protocol.
            if unsafe { self.st.capture.GetNextPacketSize() }? == 0 {
                return Ok(());
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            // SAFETY: valid out pointers; the buffer stays valid until `ReleaseBuffer`.
            unsafe {
                self.st
                    .capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
            }
            let n = frames as usize * self.st.device_channels;
            if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                self.fifo.extend(std::iter::repeat_n(0.0, n));
            } else {
                // SAFETY: WASAPI guarantees `frames * block_align` readable, 4-byte aligned bytes.
                let raw = unsafe { std::slice::from_raw_parts(data.cast::<f32>(), n) };
                self.fifo.extend(raw.iter().copied());
            }
            // SAFETY: matches the GetBuffer above.
            unsafe { self.st.capture.ReleaseBuffer(frames) }?;
        }
    }

    /// Adds the next `out.len() / 2` frames to `out` (if this output is primed).
    fn mix_into(&mut self, out: &mut [f32]) {
        let need = out.len() / 2;
        let avail = self.fifo.len() / 2;
        if !self.primed {
            if avail < need + MIX_PREBUF_FRAMES {
                return;
            }
            self.primed = true;
        }
        if avail > need + MIX_MAX_BACKLOG_FRAMES {
            self.fifo.drain(..(avail - need - MIX_PREBUF_FRAMES) * 2);
        }
        let take = need.min(self.fifo.len() / 2) * 2;
        for (o, s) in out.iter_mut().zip(self.fifo.drain(..take)) {
            *o += s;
        }
        if take < need * 2 {
            self.primed = false;
        }
    }
}

enum Opened {
    Single(CaptureState),
    /// Loopback of several outputs mixed into one stream; the first one is the default device.
    Mix(Vec<MixSource>),
}

fn open_loopback(dev: &IMMDevice) -> Result<CaptureState> {
    init_client(dev, 2, 2, AUDCLNT_STREAMFLAGS_LOOPBACK)
        .map_err(|e| os_ctx("initialise loopback capture", e))
}

fn open_capture(source: &CaptureSource) -> Result<Opened> {
    let en = super::wasapi::enumerator().map_err(os_err)?;
    match source {
        CaptureSource::Loopback(None) => {
            // Teams & co. often play on the default communications device or on an output
            // picked in their own settings: capture every real output, not just the default.
            let outputs = forwarded_render_endpoints(&en).unwrap_or_else(|e| {
                tracing::debug!("listing outputs failed ({e}); loopback of the default only");
                Vec::new()
            });
            if outputs.len() <= 1 {
                let dev = open_device(&en, DeviceKind::Render, None, eConsole)?;
                return open_loopback(&dev).map(Opened::Single);
            }
            let mut sources = Vec::with_capacity(outputs.len());
            for (i, (id, name)) in outputs.into_iter().enumerate() {
                let st = open_device(&en, DeviceKind::Render, Some(&id), eConsole)
                    .and_then(|dev| open_loopback(&dev));
                match st {
                    Ok(st) => sources.push(MixSource {
                        name,
                        st,
                        fifo: VecDeque::new(),
                        primed: false,
                    }),
                    Err(e) if i == 0 => return Err(e),
                    Err(e) => tracing::debug!(device = %name, "loopback not possible: {e}"),
                }
            }
            if sources.len() == 1 {
                return Ok(Opened::Single(sources.remove(0).st));
            }
            let names: Vec<&str> = sources.iter().map(|s| s.name.as_str()).collect();
            tracing::info!(outputs = ?names, "laptop sound: mixing the loopback of all outputs");
            Ok(Opened::Mix(sources))
        }
        CaptureSource::Loopback(Some(id)) => {
            let dev = open_device(&en, DeviceKind::Render, Some(id), eConsole)?;
            open_loopback(&dev).map(Opened::Single)
        }
        CaptureSource::Microphone(id) => {
            let dev = open_device(&en, DeviceKind::Capture, id.as_deref(), eCommunications)?;
            // Prefer a native mono stream; if the engine refuses, take stereo and downmix.
            match init_client(&dev, 1, 1, 0) {
                Ok(st) => Ok(st),
                Err(e) => {
                    tracing::debug!("mono microphone format refused ({e}); using stereo + downmix");
                    init_client(&dev, 2, 1, 0)
                        .map_err(|e| os_ctx("initialise microphone capture", e))
                }
            }
            .map(Opened::Single)
        }
    }
}

/// Activates a fresh client (a failed `Initialize` poisons the instance) and starts it with a
/// 48 kHz float format of `device_channels` channels, autoconverted by the audio engine.
fn init_client(
    dev: &IMMDevice,
    device_channels: u16,
    out_channels: usize,
    extra_flags: u32,
) -> windows::core::Result<CaptureState> {
    // SAFETY: all pointers passed to WASAPI reference locals that outlive the calls; the format
    // struct is copied by the audio engine during `Initialize`.
    unsafe {
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
        let mut default_period = 0i64;
        client.GetDevicePeriod(Some(&mut default_period), None)?;
        let fmt = float_format(device_channels);
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY
                | extra_flags,
            default_period,
            0,
            std::ptr::from_ref(&fmt).cast::<WAVEFORMATEX>(),
            None,
        )?;
        let event = Event::new(false)?;
        client.SetEventHandle(event.raw())?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        Ok(CaptureState {
            client,
            capture,
            event,
            device_channels: usize::from(device_channels),
            out_channels,
        })
    }
}

fn end_reason(e: &windows::core::Error, what: &str) -> EndReason {
    if is_device_lost(e) {
        EndReason::DeviceLost
    } else {
        EndReason::Error(format!("{what}: {e}"))
    }
}

/// Sends `samples` (interleaved, `channels` wide) as frames of at most 10 ms starting at
/// `ts_us`. Returns false when the sink is closed.
fn emit(
    sink: &UnboundedSender<AudioFrame>,
    samples: &[f32],
    channels: usize,
    mut ts_us: u64,
) -> bool {
    for chunk in samples.chunks(MAX_CHUNK_FRAMES * channels) {
        let frame = AudioFrame {
            samples: chunk.to_vec(),
            channels: channels as u16,
            ts_us,
        };
        if sink.send(frame).is_err() {
            return false;
        }
        ts_us += frames_to_us((chunk.len() / channels) as u64);
    }
    true
}

fn run_capture(st: CaptureState, stop: &Event, sink: &UnboundedSender<AudioFrame>) -> EndReason {
    let handles = [stop.raw(), st.event.raw()];
    let mut filler = SilenceFiller::new(qpc_now_us(), SILENCE_AFTER_US, MAX_CHUNK_FRAMES);
    let mut discontinuities = 0u32;
    let zeros = vec![0.0f32; MAX_CHUNK_FRAMES * st.out_channels];

    loop {
        // SAFETY: both handles are valid for the duration of the loop.
        let r = unsafe { WaitForMultipleObjects(&handles, false, WAIT_MS) };
        if r == WAIT_OBJECT_0 {
            return EndReason::Stopped;
        }
        if r == WAIT_FAILED {
            return EndReason::Error("WaitForMultipleObjects failed".into());
        }
        // Event or timeout: drain all pending packets. On timeout this also acts as a cheap
        // probe that notices a vanished device.
        loop {
            // SAFETY: capture client is valid; calls follow the GetBuffer/ReleaseBuffer protocol.
            let next = match unsafe { st.capture.GetNextPacketSize() } {
                Ok(n) => n,
                Err(e) => return end_reason(&e, "GetNextPacketSize"),
            };
            if next == 0 {
                break;
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            let mut qpc = 0u64;
            // SAFETY: valid out pointers; the buffer stays valid until `ReleaseBuffer`.
            if let Err(e) = unsafe {
                st.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))
            } {
                return end_reason(&e, "GetBuffer");
            }
            let n_frames = frames as usize;
            let now = qpc_now_us();
            let ts_us = if qpc == 0 || flags & (AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32) != 0 {
                now.saturating_sub(frames_to_us(u64::from(frames)))
            } else {
                qpc_100ns_to_us(qpc)
            };
            if flags & (AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32) != 0 {
                discontinuities += 1;
                if discontinuities.is_power_of_two() {
                    tracing::debug!("audio capture discontinuity (#{discontinuities})");
                }
            }

            let ok = if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                let zeros_n = n_frames * st.out_channels;
                if zeros_n <= zeros.len() {
                    emit(sink, &zeros[..zeros_n], st.out_channels, ts_us)
                } else {
                    emit(sink, &vec![0.0; zeros_n], st.out_channels, ts_us)
                }
            } else {
                // SAFETY: WASAPI guarantees `frames * block_align` readable, 4-byte aligned bytes.
                let raw = unsafe {
                    std::slice::from_raw_parts(data.cast::<f32>(), n_frames * st.device_channels)
                };
                if st.device_channels == st.out_channels {
                    emit(sink, raw, st.out_channels, ts_us)
                } else {
                    emit(
                        sink,
                        &downmix_to_mono(raw, st.device_channels),
                        st.out_channels,
                        ts_us,
                    )
                }
            };
            // SAFETY: matches the GetBuffer above.
            if let Err(e) = unsafe { st.capture.ReleaseBuffer(frames) } {
                return end_reason(&e, "ReleaseBuffer");
            }
            if !ok {
                return EndReason::SinkClosed;
            }
            filler.on_data(ts_us, n_frames, now);
        }

        // No data for a while (idle loopback): keep the cadence with synthetic zero frames.
        let now = qpc_now_us();
        while let Some((ts, frames)) = filler.poll(now) {
            if !emit(
                sink,
                &zeros[..frames * st.out_channels],
                st.out_channels,
                ts,
            ) {
                return EndReason::SinkClosed;
            }
        }
    }
}

/// Mixed loopback: every output keeps its own FIFO; the output clock is QPC, so an idle default
/// device (no packets at all) does not stall the outputs that are playing.
fn run_mix(
    mut sources: Vec<MixSource>,
    stop: &Event,
    sink: &UnboundedSender<AudioFrame>,
) -> EndReason {
    let mut handles = vec![stop.raw()];
    handles.extend(sources.iter().map(|s| s.st.event.raw()));
    let start_us = qpc_now_us();
    let mut emitted = 0u64;
    let mut out = Vec::new();

    loop {
        // SAFETY: all handles are valid for the duration of the loop.
        let r = unsafe { WaitForMultipleObjects(&handles, false, WAIT_MS) };
        if r == WAIT_OBJECT_0 {
            return EndReason::Stopped;
        }
        if r == WAIT_FAILED {
            return EndReason::Error("WaitForMultipleObjects failed".into());
        }
        let mut i = 0;
        while i < sources.len() {
            if let Err(e) = sources[i].drain() {
                if i == 0 {
                    return end_reason(&e, "loopback");
                }
                tracing::info!(device = %sources[i].name, "output left the mix: {e}");
                sources.remove(i);
                handles.remove(i + 1);
                continue;
            }
            i += 1;
        }

        let elapsed = qpc_now_us().saturating_sub(start_us) * u64::from(RATE) / 1_000_000;
        let mut due = elapsed.saturating_sub(emitted) as usize;
        // After a stall (system sleep, debugger) do not burst; resynchronise to "now".
        if due > 10 * MAX_CHUNK_FRAMES {
            emitted = elapsed - MAX_CHUNK_FRAMES as u64;
            due = MAX_CHUNK_FRAMES;
        }
        if due < MIX_MIN_EMIT_FRAMES {
            continue;
        }
        out.clear();
        out.resize(due * 2, 0.0);
        for s in &mut sources {
            s.mix_into(&mut out);
        }
        for v in &mut out {
            *v = v.clamp(-1.0, 1.0);
        }
        if !emit(sink, &out, 2, start_us + frames_to_us(emitted)) {
            return EndReason::SinkClosed;
        }
        emitted += due as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::WasapiDevices;
    use deskbridge_platform::audio::AudioDevices;
    use std::time::{Duration, Instant};

    /// Device-dependent and silent: the default loopback mixes every real output and must keep a
    /// steady ~100 frames/s cadence with monotonic timestamps even while all outputs are idle.
    #[test]
    fn default_loopback_mixes_all_outputs() {
        let _com = super::super::wasapi::ComGuard::init();
        let Ok(en) = super::super::wasapi::enumerator() else {
            eprintln!("skipping: no audio enumeration");
            return;
        };
        let outputs = forwarded_render_endpoints(&en).unwrap_or_default();
        eprintln!("forwarded outputs: {outputs:?}");
        let (tx, mut rx) = deskbridge_platform::unbounded_channel();
        let handle = match WasapiCapture::new().start(CaptureSource::Loopback(None), tx) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("skipping: default loopback not possible: {e}");
                return;
            }
        };
        std::thread::sleep(Duration::from_millis(500));
        drop(handle);
        let (mut frames, mut last_ts) = (0usize, 0u64);
        while let Ok(f) = rx.try_recv() {
            assert_eq!(f.channels, 2);
            assert!(f.ts_us >= last_ts, "timestamps must not go backwards");
            last_ts = f.ts_us;
            frames += f.samples.len() / 2;
        }
        // 500 ms are 24 000 frames; allow generous scheduling slack.
        assert!(frames >= 12_000, "only {frames} frames in 500 ms");
    }

    /// Device-dependent and silent: loopback on a *non-default* (normally idle) render endpoint
    /// must still deliver frames at ~100/s thanks to the synthetic silence filler. Skips on
    /// machines without a suitable device.
    #[test]
    fn idle_loopback_keeps_cadence() {
        let Ok(list) = WasapiDevices::new().list(DeviceKind::Render) else {
            eprintln!("skipping: no audio enumeration");
            return;
        };
        let Some(dev) = list.iter().find(|d| !d.is_default) else {
            eprintln!("skipping: no non-default render device");
            return;
        };
        let (tx, mut rx) = deskbridge_platform::unbounded_channel();
        let (ev_tx, mut ev_rx) = deskbridge_platform::unbounded_channel();
        let handle = match WasapiCapture::new().start_with_events(
            CaptureSource::Loopback(Some(dev.id.clone())),
            tx,
            ev_tx,
        ) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("skipping: cannot loop back {}: {e}", dev.name);
                return;
            }
        };
        let t0 = Instant::now();
        std::thread::sleep(Duration::from_millis(500));
        drop(handle);
        let mut n = 0;
        let mut last_ts = 0;
        while let Ok(f) = rx.try_recv() {
            assert_eq!(f.channels, 2);
            assert!(f.ts_us >= last_ts, "timestamps must not go backwards");
            last_ts = f.ts_us;
            n += 1;
        }
        // 500 ms at 10 ms per frame is 50; allow generous scheduling slack.
        assert!(
            n >= 25,
            "only {n} frames in {:?} from {}",
            t0.elapsed(),
            dev.name
        );
        let mut events = Vec::new();
        while let Ok(e) = ev_rx.try_recv() {
            events.push(e);
        }
        assert_eq!(events, [StreamEvent::Started, StreamEvent::Stopped]);
    }
}
