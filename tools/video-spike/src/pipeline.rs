//! The measured pipeline: source -> encoder thread -> framing ("network") -> decoder thread
//! (-> presenter). All stages run on their own threads like in the real engine.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use deskbridge_platform::unbounded_channel;
use deskbridge_platform::video::{
    CaptureOutput, CursorShapeData, CursorState, EncoderConfig, Frame, FrameData, ScreenCapturer,
    VideoCodec, VideoDecoder, VideoEncoder, VideoPresenter,
};
use deskbridge_platform_windows::video::{
    D3dDevice, D3dPresenter, DxgiCapturer, GpuFrame, PresentMode, PxRect, adapter_by_index,
    create_device, list_outputs, now_us,
};
use deskbridge_proto::media::VideoHeader;
use deskbridge_video::stream::{LateFrameFilter, Verdict, frame_header, read_frame, write_frame};
use deskbridge_video::{EncoderOptions, FfmpegDecoder, FfmpegEncoder};

use crate::stats::{Hist, table_header, table_row};
use crate::synthetic::SynthSource;
use crate::verify;

pub const WARMUP_FRAMES: u32 = 30;

/// Blocks until all GPU work submitted so far on `dev` has finished (event query). Hardware
/// decode and the video processor only *submit* work; this makes their real GPU time visible.
pub struct GpuSync {
    ctx: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    query: windows::Win32::Graphics::Direct3D11::ID3D11Query,
}

impl GpuSync {
    pub fn new(dev: &D3dDevice) -> Result<Self> {
        use windows::Win32::Graphics::Direct3D11::{D3D11_QUERY_DESC, D3D11_QUERY_EVENT};
        let desc = D3D11_QUERY_DESC {
            Query: D3D11_QUERY_EVENT,
            MiscFlags: 0,
        };
        let mut q = None;
        // SAFETY: valid descriptor and out pointer.
        unsafe { dev.device.CreateQuery(&desc, Some(&mut q))? };
        Ok(Self {
            ctx: dev.context.clone(),
            query: q.context("null query")?,
        })
    }

    pub fn wait(&self) {
        // SAFETY: plain COM calls; `done` is a 4-byte BOOL the query writes.
        unsafe {
            self.ctx.End(&self.query);
            self.ctx.Flush();
            let mut done = windows::Win32::Foundation::BOOL(0);
            let start = Instant::now();
            while self
                .ctx
                .GetData(
                    &self.query,
                    Some((&mut done as *mut windows::Win32::Foundation::BOOL).cast()),
                    4,
                    0,
                )
                .is_err()
                || !done.as_bool()
            {
                if start.elapsed() > Duration::from_millis(500) {
                    break;
                }
                std::hint::spin_loop();
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Synthetic,
    Capture,
}

#[derive(Clone)]
pub struct RunConfig {
    pub source: Source,
    pub codec: VideoCodec,
    pub bitrate_kbps: u32,
    pub fps: u32,
    pub width: u32,
    pub height: u32,
    pub seconds: f32,
    pub present_secs: f32,
    pub opts: EncoderOptions,
    pub verify: bool,
    pub adapter: Option<u32>,
    /// `(at second, new kbps)`: exercise `set_bitrate` mid-run.
    pub bitrate_step: Option<(f32, u32)>,
    /// Presenter window mode (default: small normal window).
    pub present_mode: PresentModeArg,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PresentModeArg {
    Window,
    Fullscreen,
    /// Right half of the primary monitor, registered as AppBar.
    RegionAppbar,
}

struct Encoded {
    hdr: VideoHeader,
    payload: Vec<u8>,
}

struct Wire {
    hdr: VideoHeader,
    payload: Vec<u8>,
    packetize_us: u64,
    sent_us: u64,
}

#[derive(Default)]
struct Shared {
    capture_to_submit: Mutex<Hist>,
    convert: Mutex<Hist>,
    encode: Mutex<Hist>,
    packetize: Mutex<Hist>,
    wire_to_decoder: Mutex<Hist>,
    decode: Mutex<Hist>,
    decode_submit: Mutex<Hist>,
    present: Mutex<Hist>,
    present_submit: Mutex<Hist>,
    g2g_present: Mutex<Hist>,
    g2g_decoded: Mutex<Hist>,
    frame_interval_present: Mutex<Hist>,
    produced: AtomicU64,
    dropped_source: AtomicU64,
    encoded: AtomicU64,
    decoded: AtomicU64,
    presented: AtomicU64,
    filter_dropped: AtomicU64,
    missed_deadlines: AtomicU64,
    bytes: AtomicU64,
    key_frames: AtomicU64,
    notes: Mutex<Vec<String>>,
    /// bytes per whole second of the run, for the bitrate series
    bytes_per_sec: Mutex<Vec<u64>>,
    first_packet_us: AtomicU64,
    verify_result: Mutex<Option<(f32, f32)>>,
    verify_src: Mutex<Option<Vec<f32>>>,
}

pub struct Report {
    pub text: String,
}

fn pick_adapter(arg: Option<u32>) -> Result<(u32, D3dDevice)> {
    let mut chosen = arg;
    if chosen.is_none() {
        for i in 0..8 {
            let Ok(a) = adapter_by_index(i) else { break };
            // SAFETY: plain COM call.
            let Ok(d) = (unsafe { a.GetDesc1() }) else {
                continue;
            };
            if d.Flags & 2 != 0 {
                continue;
            }
            let name = String::from_utf16_lossy(&d.Description);
            if name.to_lowercase().contains("nvidia") {
                chosen = Some(i);
                break;
            }
        }
    }
    let idx = chosen.unwrap_or(0);
    let adapter = adapter_by_index(idx).map_err(|e| anyhow!("adapter {idx}: {e}"))?;
    let dev = create_device(Some(&adapter)).map_err(|e| anyhow!("device on adapter {idx}: {e}"))?;
    Ok((idx, dev))
}

/// Runs the pipeline for `cfg.seconds` and returns the markdown report.
pub fn run(cfg: &RunConfig) -> Result<Report> {
    let (adapter_idx, capture_dev) = pick_adapter(cfg.adapter)?;
    // A second, independent device on the same adapter plays the role of the viewing PC.
    let view_dev = create_device(Some(
        &adapter_by_index(adapter_idx).map_err(|e| anyhow!("{e}"))?,
    ))
    .map_err(|e| anyhow!("view device: {e}"))?;
    let sh = Arc::new(Shared::default());
    let stop = Arc::new(AtomicBool::new(false));
    let start = Instant::now();

    // ---- source ----
    let (frame_tx, frame_rx): (SyncSender<Frame>, Receiver<Frame>) = sync_channel(2);
    let mut capturer: Option<DxgiCapturer> = None;
    let (mut width, mut height) = (cfg.width, cfg.height);
    let mut source_handles = Vec::new();
    match cfg.source {
        Source::Synthetic => {
            let synth = SynthSource::new(capture_dev.clone(), width, height)?;
            let (sh2, stop2, fps, verify_on) = (sh.clone(), stop.clone(), cfg.fps, cfg.verify);
            source_handles.push(thread::Builder::new().name("spike-source".into()).spawn(
                move || {
                    let interval = Duration::from_secs_f64(1.0 / f64::from(fps));
                    let t0 = Instant::now();
                    let mut n = 0u64;
                    while !stop2.load(Ordering::Relaxed) {
                        let target = t0 + interval.mul_f64(n as f64);
                        loop {
                            let now = Instant::now();
                            if now >= target {
                                break;
                            }
                            if target - now > Duration::from_micros(1500) {
                                thread::sleep(Duration::from_micros(500));
                            } else {
                                std::hint::spin_loop();
                            }
                        }
                        let t = n as f32 / fps as f32;
                        if let Some(f) = synth.render(t) {
                            if n == 0
                                && verify_on
                                && let FrameData::Gpu(any) = &f.data
                                && let Some(g) = GpuFrame::from_any(any.as_ref())
                                && let Ok(grid) = verify::grid_from_bgra(g)
                            {
                                *sh2.verify_src.lock().expect("lock") = Some(grid);
                            }
                            sh2.produced.fetch_add(1, Ordering::Relaxed);
                            if frame_tx.try_send(f).is_err() {
                                sh2.dropped_source.fetch_add(1, Ordering::Relaxed);
                            }
                        } else {
                            sh2.dropped_source.fetch_add(1, Ordering::Relaxed);
                        }
                        n += 1;
                    }
                },
            )?);
        }
        Source::Capture => {
            let outputs = list_outputs().map_err(|e| anyhow!("{e}"))?;
            let out = outputs
                .iter()
                .find(|o| o.adapter_index == adapter_idx && o.attached)
                .or_else(|| outputs.iter().find(|o| o.attached && !o.software))
                .context("no attached output")?;
            width = out.w;
            height = out.h;
            println!(
                "capturing {} ({}x{}) on {}",
                out.device_name, out.w, out.h, out.adapter_name
            );
            let mut c = DxgiCapturer::for_output(out.adapter_index, out.output_index);
            let (tx, mut rx) = unbounded_channel::<CaptureOutput>();
            c.start(0, cfg.fps, tx)
                .map_err(|e| anyhow!("capture start: {e}"))?;
            let (sh2, stop2) = (sh.clone(), stop.clone());
            source_handles.push(
                thread::Builder::new()
                    .name("spike-capture-fwd".into())
                    .spawn(move || {
                        while !stop2.load(Ordering::Relaxed) {
                            match rx.try_recv() {
                                Ok(CaptureOutput::Frame(f)) => {
                                    sh2.produced.fetch_add(1, Ordering::Relaxed);
                                    if frame_tx.try_send(f).is_err() {
                                        sh2.dropped_source.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                                Ok(CaptureOutput::Cursor(_) | CaptureOutput::Status(_)) => {}
                                Err(_) => thread::sleep(Duration::from_micros(300)),
                            }
                        }
                    })?,
            );
            capturer = Some(c);
        }
    }

    // ---- encoder thread ----
    let enc_cfg = EncoderConfig {
        codec: cfg.codec,
        width,
        height,
        fps: cfg.fps,
        bitrate_kbps: cfg.bitrate_kbps,
        yuv444: false,
    };
    let (enc_tx, enc_rx) = channel::<Encoded>();
    let enc_handle = {
        let (sh2, stop2, opts, step) =
            (sh.clone(), stop.clone(), cfg.opts.clone(), cfg.bitrate_step);
        thread::Builder::new().name("spike-encoder".into()).spawn(
            move || -> Result<(String, String)> {
                let mut enc = FfmpegEncoder::with_options(enc_cfg.clone(), None, opts)
                    .map_err(|e| anyhow!("{e}"))?;
                let mut frame_id = 0u32;
                let mut stepped = false;
                let mut path = String::new();
                loop {
                    let frame = match frame_rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(f) => f,
                        Err(RecvTimeoutError::Timeout) => {
                            if stop2.load(Ordering::Relaxed) {
                                break;
                            }
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    };
                    if let Some((at, kbps)) = step
                        && !stepped
                        && start.elapsed().as_secs_f32() >= at
                    {
                        stepped = true;
                        let _ = enc.set_bitrate(kbps);
                        sh2.notes
                            .lock()
                            .expect("lock")
                            .push(format!("set_bitrate({kbps} kbps) at {at:.1}s"));
                    }
                    if std::env::var_os("DB_NOENC").is_some() {
                        // capture-only diagnostic: when has the GPU finished the desktop copy?
                        let dev = deskbridge_platform_windows::video::GpuFrame::from_any(
                            match &frame.data {
                                FrameData::Gpu(a) => a.as_ref(),
                                FrameData::Bgra { .. } => continue,
                            },
                        )
                        .map(|g| g.device.clone());
                        if let Some(d) = dev {
                            let ctx = unsafe { d.GetImmediateContext().ok() };
                            if let Some(ctx) = ctx {
                                let sync = GpuSync::new(&D3dDevice {
                                    device: d.clone(),
                                    context: ctx,
                                    adapter_luid: 0,
                                    adapter_name: String::new(),
                                })?;
                                sync.wait();
                            }
                        }
                        sh2.capture_to_submit
                            .lock()
                            .expect("lock")
                            .push(now_us().saturating_sub(frame.ts_us));
                        frame_id += 1;
                        continue;
                    }
                    let submit_us = now_us();
                    let key = frame_id == 0;
                    let pkts = enc
                        .encode(&frame, key)
                        .map_err(|e| anyhow!("encode: {e}"))?;
                    let timing = enc.last_timing();
                    if path.is_empty() {
                        path = enc.path_description();
                        sh2.first_packet_us
                            .store(now_us().saturating_sub(submit_us), Ordering::Relaxed);
                    }
                    let record = frame_id >= WARMUP_FRAMES;
                    if record {
                        sh2.capture_to_submit
                            .lock()
                            .expect("lock")
                            .push(submit_us.saturating_sub(frame.ts_us));
                        sh2.convert
                            .lock()
                            .expect("lock")
                            .push(u64::from(timing.prepare_us));
                        sh2.encode
                            .lock()
                            .expect("lock")
                            .push(u64::from(timing.encode_us));
                    }
                    for p in pkts {
                        sh2.encoded.fetch_add(1, Ordering::Relaxed);
                        sh2.bytes.fetch_add(p.data.len() as u64, Ordering::Relaxed);
                        if p.key {
                            sh2.key_frames.fetch_add(1, Ordering::Relaxed);
                        }
                        let sec = start.elapsed().as_secs() as usize;
                        {
                            let mut v = sh2.bytes_per_sec.lock().expect("lock");
                            if v.len() <= sec {
                                v.resize(sec + 1, 0);
                            }
                            v[sec] += p.data.len() as u64;
                        }
                        let hdr = frame_header(
                            frame_id,
                            p.key,
                            enc_cfg.codec,
                            frame.width,
                            frame.height,
                            frame.ts_us,
                        );
                        frame_id = frame_id.wrapping_add(1);
                        if enc_tx
                            .send(Encoded {
                                hdr,
                                payload: p.data,
                            })
                            .is_err()
                        {
                            return Ok((enc.name(), path));
                        }
                    }
                    if timing.prepare_us + timing.encode_us > 1_000_000 / enc_cfg.fps {
                        sh2.missed_deadlines.fetch_add(1, Ordering::Relaxed);
                    }
                    drop(frame);
                }
                Ok((enc.name(), path))
            },
        )?
    };

    // ---- transport thread: real framing over an in-memory stream per frame ----
    let (wire_tx, wire_rx) = channel::<Wire>();
    let transport = {
        thread::Builder::new()
            .name("spike-transport".into())
            .spawn(move || -> Result<()> {
                let rt = tokio::runtime::Builder::new_current_thread().build()?;
                while let Ok(e) = enc_rx.recv() {
                    let t0 = now_us();
                    let (hdr, payload) = rt.block_on(async {
                        let (mut a, mut b) = tokio::io::duplex(e.payload.len() + 64);
                        write_frame(&mut a, &e.hdr, &e.payload).await?;
                        drop(a);
                        read_frame(&mut b).await
                    })?;
                    let t1 = now_us();
                    if wire_tx
                        .send(Wire {
                            hdr,
                            payload,
                            packetize_us: t1 - t0,
                            sent_us: t1,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(())
            })?
    };

    // ---- decoder + presenter thread ----
    let dec_handle = {
        let (sh2, stop2, codec, present_secs, verify_on, present_mode) = (
            sh.clone(),
            stop.clone(),
            cfg.codec,
            cfg.present_secs,
            cfg.verify,
            cfg.present_mode,
        );
        let vdev = view_dev.clone();
        thread::Builder::new().name("spike-decoder".into()).spawn(
            move || -> Result<(String, Option<String>)> {
                let mut dec =
                    FfmpegDecoder::new(codec, Some(&vdev.device)).map_err(|e| anyhow!("{e}"))?;
                let dec_name = dec.name();
                let mut presenter: Option<D3dPresenter> = None;
                let mut present_info = None;
                if present_secs > 0.0 {
                    let mode = match present_mode {
                        PresentModeArg::Window => PresentMode::Window {
                            width: 1280,
                            height: 360,
                        },
                        PresentModeArg::Fullscreen | PresentModeArg::RegionAppbar => {
                            let outs = list_outputs().unwrap_or_default();
                            let o = outs.iter().find(|o| o.attached && !o.software).cloned();
                            match (o, present_mode) {
                                (Some(o), PresentModeArg::Fullscreen) => PresentMode::Fullscreen {
                                    monitor_rect: PxRect::new(o.x, o.y, o.w, o.h),
                                },
                                (Some(o), _) => PresentMode::Region {
                                    rect: PxRect::new(o.x + (o.w / 2) as i32, o.y, o.w / 2, o.h),
                                    appbar: true,
                                },
                                (None, _) => PresentMode::Window {
                                    width: 1280,
                                    height: 360,
                                },
                            }
                        }
                    };
                    match D3dPresenter::new(mode, Some(&vdev.device)) {
                        Ok(p) => {
                            present_info = Some(format!("window on {}", p.adapter_name()));
                            presenter = Some(p);
                        }
                        Err(e) => sh2
                            .notes
                            .lock()
                            .expect("lock")
                            .push(format!("presenter failed: {e}")),
                    }
                }
                let present_until = start + Duration::from_secs_f32(present_secs);
                let gpu_sync = GpuSync::new(&vdev)?;
                let mut filter = LateFrameFilter::new();
                let mut last_present: Option<u64> = None;
                let mut overlay_set = false;
                loop {
                    let w = match wire_rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(w) => w,
                        Err(RecvTimeoutError::Timeout) => {
                            if stop2.load(Ordering::Relaxed) {
                                break;
                            }
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    };
                    let t_recv = now_us();
                    let out = filter.on_frame(&w.hdr, t_recv / 1000, None);
                    if out.verdict != Verdict::Accept {
                        sh2.filter_dropped.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    let record = w.hdr.frame_id >= WARMUP_FRAMES;
                    let t0 = now_us();
                    let frame = match dec.decode_with_ts(&w.payload, w.hdr.is_key(), w.hdr.ts_us) {
                        Ok(Some(f)) => f,
                        Ok(None) => continue,
                        Err(e) => {
                            sh2.notes
                                .lock()
                                .expect("lock")
                                .push(format!("decode error: {e}"));
                            continue;
                        }
                    };
                    let t_submit = now_us();
                    // While the presenter shares the device, an event query would also wait for the
                    // previous flip; only sync when nothing is being presented.
                    if presenter.is_none() {
                        gpu_sync.wait();
                    }
                    let t1 = now_us();
                    sh2.decoded.fetch_add(1, Ordering::Relaxed);
                    if record {
                        sh2.packetize.lock().expect("lock").push(w.packetize_us);
                        sh2.wire_to_decoder
                            .lock()
                            .expect("lock")
                            .push(t_recv.saturating_sub(w.sent_us));
                        sh2.decode_submit.lock().expect("lock").push(t_submit - t0);
                        sh2.decode.lock().expect("lock").push(t1 - t0);
                        sh2.g2g_decoded
                            .lock()
                            .expect("lock")
                            .push(t1.saturating_sub(w.hdr.ts_us));
                    }
                    if verify_on
                        && w.hdr.frame_id == 0
                        && let FrameData::Gpu(any) = &frame.data
                        && let Some(g) = GpuFrame::from_any(any.as_ref())
                    {
                        let dec_grid = verify::grid_from_nv12(g);
                        let src = sh2.verify_src.lock().expect("lock").clone();
                        if let (Ok(d), Some(s)) = (dec_grid, src) {
                            *sh2.verify_result.lock().expect("lock") =
                                Some(verify::compare(&s, &d));
                        }
                    }
                    if let Some(p) = presenter.as_mut() {
                        if Instant::now() >= present_until {
                            p.close();
                            presenter = None;
                        } else {
                            if !overlay_set {
                                overlay_set = true;
                                p.set_stats_overlay(true, "video-spike\nloopback test");
                                p.set_cursor(&CursorState {
                                    x: 0.5,
                                    y: 0.5,
                                    visible: true,
                                    shape_serial: 1,
                                    shape: Some(CursorShapeData {
                                        width: 12,
                                        height: 12,
                                        hot_x: 0,
                                        hot_y: 0,
                                        rgba: [255u8, 0, 255, 255].repeat(12 * 12),
                                    }),
                                });
                            }
                            let tp0 = now_us();
                            if let Err(e) = p.present(&frame) {
                                sh2.notes
                                    .lock()
                                    .expect("lock")
                                    .push(format!("present error: {e}"));
                            } else {
                                let tp_call = now_us();
                                let tp1 = tp_call;
                                sh2.presented.fetch_add(1, Ordering::Relaxed);
                                if record {
                                    sh2.present_submit.lock().expect("lock").push(tp_call - tp0);
                                    sh2.present.lock().expect("lock").push(tp1 - tp0);
                                    sh2.g2g_present
                                        .lock()
                                        .expect("lock")
                                        .push(tp1.saturating_sub(w.hdr.ts_us));
                                    if let Some(lp) = last_present {
                                        sh2.frame_interval_present
                                            .lock()
                                            .expect("lock")
                                            .push(tp1 - lp);
                                    }
                                }
                                last_present = Some(tp1);
                            }
                        }
                    }
                    drop(frame);
                }
                if let Some(mut p) = presenter {
                    p.close();
                }
                Ok((dec_name, present_info))
            },
        )?
    };

    // ---- run ----
    thread::sleep(Duration::from_secs_f32(cfg.seconds));
    stop.store(true, Ordering::SeqCst);
    if let Some(mut c) = capturer {
        c.stop();
    }
    for h in source_handles {
        let _ = h.join();
    }
    let enc_res = enc_handle
        .join()
        .map_err(|_| anyhow!("encoder thread panicked"))?;
    let _ = transport.join();
    let dec_res = dec_handle
        .join()
        .map_err(|_| anyhow!("decoder thread panicked"))?;
    let elapsed = start.elapsed().as_secs_f64();

    let (enc_name, path) = enc_res?;
    let (dec_name, present_info) = dec_res?;
    Ok(render_report(
        cfg,
        &sh,
        adapter_idx,
        &capture_dev,
        &view_dev,
        &enc_name,
        &path,
        &dec_name,
        present_info,
        elapsed,
        (width, height),
    ))
}

#[allow(clippy::too_many_arguments)]
fn render_report(
    cfg: &RunConfig,
    sh: &Shared,
    adapter_idx: u32,
    cap: &D3dDevice,
    view: &D3dDevice,
    enc_name: &str,
    path: &str,
    dec_name: &str,
    present_info: Option<String>,
    elapsed: f64,
    size: (u32, u32),
) -> Report {
    let mut s = String::new();
    let g = |m: &Mutex<Hist>| m.lock().expect("lock").clone();
    let n = |a: &AtomicU64| a.load(Ordering::Relaxed);
    s += &format!(
        "### {:?} {}x{}@{} {:?} target {} Mbit/s ({:?})\n\n",
        cfg.codec,
        size.0,
        size.1,
        cfg.fps,
        cfg.source,
        f64::from(cfg.bitrate_kbps) / 1000.0,
        cfg.opts.preset
    );
    s += &format!(
        "- adapter #{adapter_idx}: capture/encode `{}`; decode/present device `{}`\n",
        cap.adapter_name, view.adapter_name
    );
    s += &format!(
        "- encoder: `{enc_name}`; path: {path}\n- decoder: `{dec_name}`; presenter: {}\n",
        present_info.unwrap_or_else(|| "none (decode only)".into())
    );
    s += &format!(
        "- encoder options: preset {}, tune {}, vbv {} frame(s), intra-refresh {}, force-readback {}\n",
        cfg.opts.preset,
        cfg.opts.tune,
        cfg.opts.vbv_frames,
        cfg.opts.intra_refresh,
        cfg.opts.force_readback
    );
    let bytes = n(&sh.bytes);
    s += &format!(
        "- run {:.1}s: produced {}, dropped at source/encoder queue {}, encoded {}, decoded {}, presented {}, receiver-filter drops {}, encode deadline misses (prepare+encode > frame budget) {}, key frames {}\n",
        elapsed,
        n(&sh.produced),
        n(&sh.dropped_source),
        n(&sh.encoded),
        n(&sh.decoded),
        n(&sh.presented),
        n(&sh.filter_dropped),
        n(&sh.missed_deadlines),
        n(&sh.key_frames)
    );
    s += &format!(
        "- achieved: encode {:.1} fps, {:.1} Mbit/s average; time to first packet (incl. encoder open) {:.0} ms\n",
        n(&sh.encoded) as f64 / elapsed,
        bytes as f64 * 8.0 / elapsed / 1e6,
        n(&sh.first_packet_us) as f64 / 1000.0
    );
    if let Some((mae, max)) = *sh.verify_result.lock().expect("lock") {
        s += &format!(
            "- picture check (first frame, {}x{} luma grid, BT.709): mean abs error {:.2}, max {:.1} (8-bit levels)\n",
            verify::GRID_X,
            verify::GRID_Y,
            mae,
            max
        );
    }
    let series = sh.bytes_per_sec.lock().expect("lock").clone();
    if series.len() > 2 {
        let mbit: Vec<String> = series
            .iter()
            .take(series.len() - 1)
            .map(|b| format!("{:.0}", *b as f64 * 8.0 / 1e6))
            .collect();
        s += &format!("- Mbit/s per second: {}\n", mbit.join(" "));
    }
    for note in sh.notes.lock().expect("lock").iter() {
        s += &format!("- note: {note}\n");
    }
    s += "\n";
    s += table_header();
    s += "\n";
    for (name, h) in [
        ("capture -> encode submit (queue)", g(&sh.capture_to_submit)),
        ("colour convert (+ read-back on fallback)", g(&sh.convert)),
        ("encode (send_frame..packet)", g(&sh.encode)),
        ("packetize (write_frame + read_frame)", g(&sh.packetize)),
        ("decode, CPU submit only", g(&sh.decode_submit)),
        ("decode, incl. GPU completion", g(&sh.decode)),
        (
            "present call (CPU submit; GPU work is async)",
            g(&sh.present_submit),
        ),
        ("glass-to-glass: capture -> decoded", g(&sh.g2g_decoded)),
        ("glass-to-glass: capture -> presented", g(&sh.g2g_present)),
        ("interval between presents", g(&sh.frame_interval_present)),
    ] {
        if !h.0.is_empty() {
            s += &table_row(name, &h);
            s += "\n";
        }
    }
    Report { text: s }
}

/// Decode benchmark: pre-encodes `frames` synthetic pictures with the given encoder, then decodes
/// them in a loop for `seconds` (used for AV1 where no hardware encoder is available here).
pub fn decode_bench(cfg: &RunConfig, frames: u32) -> Result<Report> {
    let (idx, dev) = pick_adapter(cfg.adapter)?;
    let view = create_device(Some(&adapter_by_index(idx).map_err(|e| anyhow!("{e}"))?))
        .map_err(|e| anyhow!("{e}"))?;
    let synth = SynthSource::new(dev.clone(), cfg.width, cfg.height)?;
    let enc_cfg = EncoderConfig {
        codec: cfg.codec,
        width: cfg.width,
        height: cfg.height,
        fps: cfg.fps,
        bitrate_kbps: cfg.bitrate_kbps,
        yuv444: false,
    };
    let mut enc = FfmpegEncoder::with_options(enc_cfg.clone(), Some(&dev.device), cfg.opts.clone())
        .map_err(|e| anyhow!("encoder: {e}"))?;
    println!("pre-encoding {frames} frames with {} ...", enc.name());
    let t0 = Instant::now();
    let mut pkts: Vec<(Vec<u8>, bool, u64)> = Vec::new();
    for n in 0..frames {
        let Some(f) = synth.render(n as f32 / cfg.fps as f32) else {
            continue;
        };
        for p in enc.encode(&f, n == 0).map_err(|e| anyhow!("{e}"))? {
            pkts.push((p.data, p.key, f.ts_us));
        }
    }
    for p in enc.flush().map_err(|e| anyhow!("{e}"))? {
        pkts.push((p.data, p.key, 0));
    }
    println!(
        "encoded {} packets in {:.1}s",
        pkts.len(),
        t0.elapsed().as_secs_f32()
    );
    let total: usize = pkts.iter().map(|p| p.0.len()).sum();
    let mut dec = FfmpegDecoder::new(cfg.codec, Some(&view.device)).map_err(|e| anyhow!("{e}"))?;
    let gpu_sync = GpuSync::new(&view)?;
    let mut presenter = if cfg.present_secs > 0.0 {
        D3dPresenter::new(
            PresentMode::Window {
                width: 1280,
                height: 360,
            },
            Some(&view.device),
        )
        .map_err(|e| println!("presenter: {e}"))
        .ok()
    } else {
        None
    };
    let mut present_hist = Hist::default();
    let mut hist = Hist::default();
    let mut key_hist = Hist::default();
    let mut n_dec = 0u64;
    let start = Instant::now();
    'outer: loop {
        for (data, key, ts) in &pkts {
            if start.elapsed().as_secs_f32() >= cfg.seconds {
                break 'outer;
            }
            let a = now_us();
            let r = dec
                .decode_with_ts(data, *key, *ts)
                .map_err(|e| anyhow!("{e}"))?;
            if std::env::var_os("DB_NOSYNC").is_none() {
                gpu_sync.wait();
            }
            let b = now_us();
            if let (Some(p), Some(f)) = (presenter.as_mut(), r.as_ref()) {
                let c = now_us();
                if let Err(e) = p.present(f) {
                    println!("present error: {e}");
                }
                if std::env::var_os("DB_NOSYNC").is_none() {
                    gpu_sync.wait();
                }
                present_hist.push(now_us() - c);
            }
            if r.is_some() {
                n_dec += 1;
                if *key {
                    key_hist.push(b - a)
                } else {
                    hist.push(b - a)
                }
            }
        }
    }
    let el = start.elapsed().as_secs_f64();
    let mut s = format!(
        "### decode benchmark {:?} {}x{} (stream encoded by `{}`, {:.1} Mbit/s average at {} fps)\n\n- decoder `{}` on `{}`\n- decoded {} frames in {:.1}s = {:.1} fps (unthrottled, back-to-back)\n\n",
        cfg.codec,
        cfg.width,
        cfg.height,
        enc.name(),
        total as f64 * 8.0 / (pkts.len().max(1) as f64 / f64::from(cfg.fps)) / 1e6,
        cfg.fps,
        dec.name(),
        view.adapter_name,
        n_dec,
        el,
        n_dec as f64 / el
    );
    s += table_header();
    s += "\n";
    s += &table_row("decode, delta frames", &hist);
    s += "\n";
    if !key_hist.0.is_empty() {
        s += &table_row("decode, key frames", &key_hist);
        s += "\n";
    }
    if !present_hist.0.is_empty() {
        s += &table_row(
            "present incl. GPU completion (NV12 -> BGRA VP + draw)",
            &present_hist,
        );
        s += "
";
    }
    Ok(Report { text: s })
}
