//! Event-driven WASAPI shared-mode render with the smallest engine period the device allows.

use super::devices::friendly_name;
use super::stream::{EndReason, StreamEvent, StreamThread, spawn_stream};
use super::wasapi::{Event, device_id, float_format, is_device_lost, open_device, os_ctx, os_err};
use deskbridge_platform::audio::{AudioPlayback, DeviceKind, PullSource, RATE, StreamHandle};
use deskbridge_platform::{Result, UnboundedSender};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use windows::Win32::Foundation::{WAIT_FAILED, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient,
    IAudioClient3, IAudioRenderClient, IMMDevice, WAVEFORMATEX, eConsole,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoTaskMemFree};
use windows::Win32::System::Threading::WaitForMultipleObjects;
use windows::core::Interface;

const CHANNELS: usize = 2;
/// Wake-up timeout; the engine normally signals every period (2.5-10 ms).
const WAIT_MS: u32 = 200;

/// What was negotiated with the audio engine.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackInfo {
    pub device_id: String,
    pub device_name: String,
    /// `true` when `IAudioClient3::InitializeSharedAudioStream` with the minimum period was used.
    pub low_latency: bool,
    /// Engine period in frames (the interval between wake-ups).
    pub period_frames: u32,
    /// Size of the render buffer in frames.
    pub buffer_frames: u32,
    /// Fill level (frames) the render loop tops the buffer up to: 1.5 engine periods, which
    /// bounds the queueing latency while tolerating ~5 ms of wake-up jitter at a 10 ms period.
    pub fill_target_frames: u32,
    /// `IAudioClient::GetStreamLatency` (additional latency reported by the engine).
    pub engine_latency_ms: f32,
    /// Estimated latency from `PullSource::fill` to the endpoint: average queued audio plus the
    /// engine-reported latency. Excludes the device/dongle's own hardware latency.
    pub latency_ms: f32,
}

/// Running render stream. Dropping it stops the thread and joins it.
pub struct PlaybackHandle {
    _thread: StreamThread,
    info: PlaybackInfo,
    underruns: Arc<AtomicU32>,
}

impl PlaybackHandle {
    /// Estimated software latency in ms (see [`PlaybackInfo::latency_ms`]).
    pub fn stream_latency_ms(&self) -> f32 {
        self.info.latency_ms
    }

    /// Friendly name of the endpoint being played to.
    pub fn device_name(&self) -> &str {
        &self.info.device_name
    }

    /// Endpoint id of the device being played to.
    pub fn device_id(&self) -> &str {
        &self.info.device_id
    }

    /// Number of wake-ups at which the render buffer was found completely empty (an audible
    /// glitch unless the source delivered silence anyway).
    pub fn underruns(&self) -> u32 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// Full negotiation details.
    pub fn info(&self) -> &PlaybackInfo {
        &self.info
    }
}

impl StreamHandle for PlaybackHandle {}

/// WASAPI [`AudioPlayback`].
#[derive(Debug, Default, Clone, Copy)]
pub struct WasapiPlayback;

impl WasapiPlayback {
    pub fn new() -> Self {
        Self
    }

    /// Starts rendering and returns the concrete handle exposing latency and device name.
    pub fn start_concrete(
        &self,
        device_id: Option<&str>,
        source: Arc<dyn PullSource>,
    ) -> Result<PlaybackHandle> {
        start_inner(device_id, source, None)
    }

    /// [`start_concrete`](Self::start_concrete) plus lifecycle notifications. On device loss the
    /// thread stops feeding and exits; restart when `DeviceEvent::Added` arrives.
    pub fn start_concrete_with_events(
        &self,
        device_id: Option<&str>,
        source: Arc<dyn PullSource>,
        lifecycle_tx: UnboundedSender<StreamEvent>,
    ) -> Result<PlaybackHandle> {
        start_inner(device_id, source, Some(lifecycle_tx))
    }

    /// Boxed-handle variant of [`start_concrete_with_events`](Self::start_concrete_with_events).
    pub fn start_with_events(
        &self,
        device_id: Option<&str>,
        source: Arc<dyn PullSource>,
        lifecycle_tx: UnboundedSender<StreamEvent>,
    ) -> Result<Box<dyn StreamHandle>> {
        Ok(Box::new(self.start_concrete_with_events(
            device_id,
            source,
            lifecycle_tx,
        )?))
    }
}

impl AudioPlayback for WasapiPlayback {
    fn start(
        &self,
        device_id: Option<&str>,
        source: Arc<dyn PullSource>,
    ) -> Result<Box<dyn StreamHandle>> {
        Ok(Box::new(self.start_concrete(device_id, source)?))
    }
}

fn start_inner(
    device_id: Option<&str>,
    source: Arc<dyn PullSource>,
    lifecycle: Option<UnboundedSender<StreamEvent>>,
) -> Result<PlaybackHandle> {
    let id = device_id.map(str::to_owned);
    let underruns = Arc::new(AtomicU32::new(0));
    let underruns_thread = underruns.clone();
    let (thread, info) = spawn_stream(
        "db-audio-render",
        lifecycle,
        move || open_render(id.as_deref()),
        move |st, stop| run_render(st, stop, source.as_ref(), &underruns_thread),
    )?;
    Ok(PlaybackHandle {
        _thread: thread,
        info,
        underruns,
    })
}

struct RenderState {
    client: IAudioClient,
    render: IAudioRenderClient,
    event: Event,
    buffer_frames: u32,
    fill_target: u32,
}

impl Drop for RenderState {
    fn drop(&mut self) {
        // SAFETY: the client is valid; Stop on an invalidated client just fails.
        let _ = unsafe { self.client.Stop() };
    }
}

struct Negotiated {
    client: IAudioClient,
    low_latency: bool,
    period_frames: u32,
}

/// `IAudioClient3` with the minimum shared-mode period, if the device/driver supports it for
/// 48 kHz stereo float.
fn init_low_latency(dev: &IMMDevice) -> windows::core::Result<Negotiated> {
    // SAFETY: pointers reference locals that outlive the calls; the format is copied by WASAPI.
    unsafe {
        let c3: IAudioClient3 = dev.Activate(CLSCTX_ALL, None)?;
        let fmt = float_format(CHANNELS as u16);
        let pfmt = std::ptr::from_ref(&fmt).cast::<WAVEFORMATEX>();
        let (mut default, mut fundamental, mut min, mut max) = (0u32, 0u32, 0u32, 0u32);
        c3.GetSharedModeEnginePeriod(pfmt, &mut default, &mut fundamental, &mut min, &mut max)?;
        c3.InitializeSharedAudioStream(AUDCLNT_STREAMFLAGS_EVENTCALLBACK, min, pfmt, None)?;
        // Read back what the engine actually runs with.
        let mut cur_fmt: *mut WAVEFORMATEX = std::ptr::null_mut();
        let mut cur = min;
        if c3
            .GetCurrentSharedModeEnginePeriod(&mut cur_fmt, &mut cur)
            .is_ok()
            && !cur_fmt.is_null()
        {
            CoTaskMemFree(Some(cur_fmt as *const _));
        }
        tracing::debug!(
            default,
            fundamental,
            min,
            max,
            cur,
            "IAudioClient3 engine periods (frames)"
        );
        Ok(Negotiated {
            client: c3.cast()?,
            low_latency: true,
            period_frames: cur,
        })
    }
}

/// Classic `IAudioClient::Initialize` with autoconversion and the device's default period.
fn init_classic(dev: &IMMDevice) -> windows::core::Result<Negotiated> {
    // SAFETY: as in `init_low_latency`.
    unsafe {
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
        let mut default_period = 0i64; // 100 ns units
        client.GetDevicePeriod(Some(&mut default_period), None)?;
        let fmt = float_format(CHANNELS as u16);
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            default_period,
            0,
            std::ptr::from_ref(&fmt).cast::<WAVEFORMATEX>(),
            None,
        )?;
        let period_frames =
            (default_period as u64 * u64::from(RATE) / 10_000_000).clamp(1, u32::MAX as u64) as u32;
        Ok(Negotiated {
            client,
            low_latency: false,
            period_frames,
        })
    }
}

fn open_render(id: Option<&str>) -> Result<(RenderState, PlaybackInfo)> {
    let en = super::wasapi::enumerator().map_err(os_err)?;
    let dev = open_device(&en, DeviceKind::Render, id, eConsole)?;
    let dev_id = device_id(&dev).map_err(os_err)?;
    let dev_name = friendly_name(&dev);

    let neg = match init_low_latency(&dev) {
        Ok(n) => n,
        Err(e) => {
            tracing::debug!("low-latency shared stream unavailable ({e}); using default period");
            init_classic(&dev).map_err(|e| os_ctx("initialise render stream", e))?
        }
    };

    // SAFETY: client is initialised; all out pointers are valid.
    let state = unsafe {
        let ctx = |what: &'static str| move |e| os_ctx(what, e);
        let event = Event::new(false).map_err(os_err)?;
        neg.client
            .SetEventHandle(event.raw())
            .map_err(ctx("SetEventHandle"))?;
        let buffer_frames = neg.client.GetBufferSize().map_err(ctx("GetBufferSize"))?;
        let render: IAudioRenderClient = neg.client.GetService().map_err(ctx("GetService"))?;
        // Start with silence up to the fill target so the first period does not underrun.
        let fill_target = fill_target(neg.period_frames, buffer_frames);
        render.GetBuffer(fill_target).map_err(ctx("GetBuffer"))?;
        render
            .ReleaseBuffer(fill_target, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)
            .map_err(ctx("ReleaseBuffer"))?;
        neg.client.Start().map_err(ctx("Start"))?;
        RenderState {
            client: neg.client.clone(),
            render,
            event,
            buffer_frames,
            fill_target,
        }
    };

    // SAFETY: simple getter on an initialised client.
    let engine_hns = unsafe { state.client.GetStreamLatency() }.unwrap_or(0);
    let engine_latency_ms = engine_hns as f32 / 10_000.0;
    // Queued audio oscillates between target-period and target; report the mean.
    let queued_ms =
        (state.fill_target as f32 - neg.period_frames as f32 / 2.0).max(0.0) * 1000.0 / RATE as f32;
    let info = PlaybackInfo {
        device_id: dev_id,
        device_name: dev_name,
        low_latency: neg.low_latency,
        period_frames: neg.period_frames,
        buffer_frames: state.buffer_frames,
        fill_target_frames: state.fill_target,
        engine_latency_ms,
        latency_ms: queued_ms + engine_latency_ms,
    };
    tracing::info!(?info, "render stream started");
    Ok((state, info))
}

/// Render fill level: 1.5 periods, clamped to the buffer.
fn fill_target(period_frames: u32, buffer_frames: u32) -> u32 {
    (period_frames + period_frames / 2).clamp(1, buffer_frames.max(1))
}

fn end_reason(e: &windows::core::Error, what: &str) -> EndReason {
    if is_device_lost(e) {
        EndReason::DeviceLost
    } else {
        EndReason::Error(format!("{what}: {e}"))
    }
}

fn run_render(
    st: RenderState,
    stop: &Event,
    source: &dyn PullSource,
    underruns: &AtomicU32,
) -> EndReason {
    let handles = [stop.raw(), st.event.raw()];
    loop {
        // SAFETY: both handles are valid for the duration of the loop.
        let r = unsafe { WaitForMultipleObjects(&handles, false, WAIT_MS) };
        if r == WAIT_OBJECT_0 {
            return EndReason::Stopped;
        }
        if r == WAIT_FAILED {
            return EndReason::Error("WaitForMultipleObjects failed".into());
        }

        // SAFETY: client is valid.
        let padding = match unsafe { st.client.GetCurrentPadding() } {
            Ok(p) => p,
            Err(e) => return end_reason(&e, "GetCurrentPadding"),
        };
        if padding == 0 {
            underruns.fetch_add(1, Ordering::Relaxed);
        }
        let avail = st.fill_target.saturating_sub(padding);
        if avail == 0 {
            continue;
        }
        // SAFETY: GetBuffer/ReleaseBuffer protocol; the buffer holds `avail` stereo f32 frames and
        // is valid until ReleaseBuffer.
        unsafe {
            let data = match st.render.GetBuffer(avail) {
                Ok(p) => p,
                Err(e) => return end_reason(&e, "GetBuffer"),
            };
            let out = std::slice::from_raw_parts_mut(data.cast::<f32>(), avail as usize * CHANNELS);
            // A panicking source must not unwind through the audio thread; play silence instead.
            let ok = catch_unwind(AssertUnwindSafe(|| source.fill(out))).is_ok();
            if !ok {
                out.fill(0.0);
            }
            if let Err(e) = st.render.ReleaseBuffer(avail, 0) {
                return end_reason(&e, "ReleaseBuffer");
            }
            if !ok {
                return EndReason::Error("PullSource::fill panicked".into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fill_target;

    #[test]
    fn fill_target_bounds() {
        assert_eq!(fill_target(480, 1056), 720);
        assert_eq!(fill_target(120, 240), 180);
        assert_eq!(fill_target(480, 600), 600);
        assert_eq!(fill_target(0, 0), 1);
    }
}
