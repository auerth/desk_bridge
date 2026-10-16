//! Event-driven WASAPI capture (microphone and render-endpoint loopback).

use super::stream::{EndReason, StreamEvent, StreamThread, spawn_stream};
use super::util::{SilenceFiller, downmix_to_mono, frames_to_us, qpc_100ns_to_us};
use super::wasapi::{Event, float_format, is_device_lost, open_device, os_ctx, os_err, qpc_now_us};
use deskbridge_platform::audio::{
    AudioCapture, AudioFrame, CaptureSource, DeviceKind, RATE, StreamHandle,
};
use deskbridge_platform::{Result, UnboundedSender};
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
        move |st, stop| run_capture(st, stop, &sink),
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

fn open_capture(source: &CaptureSource) -> Result<CaptureState> {
    let en = super::wasapi::enumerator().map_err(os_err)?;
    match source {
        CaptureSource::Loopback(id) => {
            let dev = open_device(&en, DeviceKind::Render, id.as_deref(), eConsole)?;
            init_client(&dev, 2, 2, AUDCLNT_STREAMFLAGS_LOOPBACK)
                .map_err(|e| os_ctx("initialise loopback capture", e))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::WasapiDevices;
    use deskbridge_platform::audio::AudioDevices;
    use std::time::{Duration, Instant};

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
