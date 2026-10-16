//! FFmpeg hardware encoder (`VideoEncoder`).
//!
//! Zero-copy path (NVENC): the captured BGRA `ID3D11Texture2D` is converted to BT.709 NV12 by the
//! D3D11 video processor straight into a slice of an FFmpeg D3D11 hardware frame pool and handed
//! to `*_nvenc` without ever touching system memory.
//!
//! Fallback path (QSV, AMF, forced, or CPU frames): NV12 pictures in system memory (GPU frames are
//! converted on the GPU and read back through a staging texture; BGRA CPU frames go through
//! swscale). It works with every FFmpeg hardware encoder but costs a PCIe read-back per frame.

use std::ffi::{CString, c_void};
use std::time::Instant;

use deskbridge_platform::video::{
    EncodedPacket, EncoderConfig, Frame, FrameData, VideoCodec, VideoEncoder,
};
use deskbridge_platform::{PlatformError, Result};
use deskbridge_platform_windows::video::{
    ConvKind, GpuFrame, VpConverter, device_adapter_info, enable_multithread_protection,
};
use ffmpeg_next::ffi;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
    ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice};
use windows::core::Interface;

use crate::ff::{self, AvD3d11vaDeviceContext, AvD3d11vaFramesContext, BufRef, CodecCtx, Dict};

/// Tunables that the spike (ADR 0005) determines defaults for.
#[derive(Debug, Clone)]
pub struct EncoderOptions {
    /// NVENC preset `p1` (fastest) .. `p7`.
    pub preset: String,
    /// NVENC tune: `ull` (ultra low latency), `ll`, `hq`.
    pub tune: String,
    /// VBV buffer size in frames (1.0 = one frame at the target rate).
    pub vbv_frames: f32,
    /// Intra refresh instead of relying on IDR frames only (H.264/HEVC NVENC).
    pub intra_refresh: bool,
    /// Never use the zero-copy path (measures the cost of the fallback).
    pub force_readback: bool,
    /// Use exactly this FFmpeg encoder (`hevc_nvenc`, `libsvtav1`, ...). `None` = hardware autodetect.
    pub encoder_name: Option<String>,
}

impl Default for EncoderOptions {
    fn default() -> Self {
        Self {
            preset: "p2".into(),
            tune: "ull".into(),
            vbv_frames: 1.0,
            intra_refresh: false,
            force_readback: false,
            encoder_name: None,
        }
    }
}

/// Per-frame timing of the last `encode` call (microseconds).
#[derive(Debug, Clone, Copy, Default)]
pub struct EncodeTiming {
    /// GPU colour conversion (+ read-back on the fallback path) up to submission.
    pub prepare_us: u32,
    /// `send_frame` + `receive_packet` (hardware encode, blocking).
    pub encode_us: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    ZeroCopy,
    SystemMemory,
}

struct Readback {
    vp: VpConverter,
    nv12: ID3D11Texture2D,
    staging: ID3D11Texture2D,
    in_size: (u32, u32),
}

struct Sws(*mut ffi::SwsContext, (u32, u32));

impl Drop for Sws {
    fn drop(&mut self) {
        // SAFETY: we own the context (or it is null).
        unsafe { ffi::sws_freeContext(self.0) };
    }
}

struct Open {
    name: String,
    kind: Kind,
    /// Pixel format of system-memory input (NV12 for hardware encoders, YUV420P for libaom/SVT-AV1).
    sw_fmt: ffi::AVPixelFormat,
    ctx: CodecCtx,
    frame: ff::Frame,
    pkt: ff::Packet,
    hw_frames: BufRef,
    _hw_device: BufRef,
    device: Option<(ID3D11Device, ID3D11DeviceContext)>,
    vp: Option<VpConverter>,
    readback: Option<Readback>,
    sws: Option<Sws>,
}

/// FFmpeg based hardware encoder.
pub struct FfmpegEncoder {
    cfg: EncoderConfig,
    opts: EncoderOptions,
    device_hint: Option<ID3D11Device>,
    open: Option<Open>,
    kbps: u32,
    pending_bitrate: bool,
    timing: EncodeTiming,
    frames: u64,
    last_name: String,
}

// SAFETY: raw FFmpeg pointers are owned by this struct and only used through `&mut self`;
// the D3D11 interfaces belong to a multithread-protected device.
unsafe impl Send for FfmpegEncoder {}

fn codec_suffixes(c: VideoCodec) -> (&'static str, [&'static str; 3]) {
    match c {
        VideoCodec::H264 => ("h264", ["nvenc", "qsv", "amf"]),
        VideoCodec::Hevc => ("hevc", ["nvenc", "qsv", "amf"]),
        VideoCodec::Av1 => ("av1", ["nvenc", "qsv", "amf"]),
    }
}

/// Vendor id (PCI) of the adapter behind a device.
fn adapter_vendor(device: &ID3D11Device) -> u32 {
    (|| -> windows::core::Result<u32> {
        let dxgi: IDXGIDevice = device.cast()?;
        // SAFETY: plain COM calls.
        let adapter: IDXGIAdapter = unsafe { dxgi.GetAdapter()? };
        Ok(unsafe { adapter.GetDesc()? }.VendorId)
    })()
    .unwrap_or(0)
}

/// Encoder names to try. A known GPU vendor only gets its own family (AMF and QSV happily
/// "open" on foreign GPUs and then fail later); an unknown vendor tries all families.
fn candidates(codec: VideoCodec, vendor: u32) -> Vec<String> {
    let (base, fams) = codec_suffixes(codec);
    match vendor {
        0x10DE => vec![format!("{base}_nvenc")],
        0x8086 => vec![format!("{base}_qsv")],
        0x1002 | 0x1022 => vec![format!("{base}_amf")],
        _ => fams.iter().map(|f| format!("{base}_{f}")).collect(),
    }
}

impl FfmpegEncoder {
    /// Creates the encoder. With `device` the hardware encoder is opened immediately (errors are
    /// reported here); with `None` it is opened when the first frame arrives, on the device of that
    /// frame.
    pub fn new(cfg: EncoderConfig, device: Option<&ID3D11Device>) -> Result<Self> {
        Self::with_options(cfg, device, EncoderOptions::default())
    }

    pub fn with_options(
        cfg: EncoderConfig,
        device: Option<&ID3D11Device>,
        opts: EncoderOptions,
    ) -> Result<Self> {
        ff::init();
        if cfg.width == 0
            || cfg.height == 0
            || cfg.fps == 0
            || !cfg.width.is_multiple_of(2)
            || !cfg.height.is_multiple_of(2)
        {
            return Err(PlatformError::other(format!(
                "invalid encoder size {}x{}@{}",
                cfg.width, cfg.height, cfg.fps
            )));
        }
        if cfg.yuv444 {
            return Err(PlatformError::Unsupported(
                "4:4:4 encode (D3D11 frame path has no 4:4:4 format; needs a CUDA frame path)",
            ));
        }
        let kbps = cfg.bitrate_kbps.max(1_000);
        let mut enc = Self {
            cfg,
            opts,
            device_hint: device.cloned(),
            open: None,
            kbps,
            pending_bitrate: false,
            timing: EncodeTiming::default(),
            frames: 0,
            last_name: String::new(),
        };
        if enc.device_hint.is_some() {
            let dev = enc.device_hint.clone();
            enc.open_now(dev.as_ref())?;
        }
        Ok(enc)
    }

    /// Drains frames still held inside the encoder (software encoders buffer; NVENC does not).
    pub fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        let Some(o) = self.open.as_mut() else {
            return Ok(vec![]);
        };
        let mut out = Vec::new();
        // SAFETY: the context is open; a NULL frame starts draining.
        unsafe {
            let r = ffi::avcodec_send_frame(o.ctx.0, std::ptr::null());
            if r < 0 && r != ff::AVERROR_EOF {
                return Err(ff::ff_err("avcodec_send_frame(flush)", r));
            }
            loop {
                let r = ffi::avcodec_receive_packet(o.ctx.0, o.pkt.0);
                if r < 0 {
                    break;
                }
                let p = &*o.pkt.0;
                out.push(EncodedPacket {
                    data: std::slice::from_raw_parts(p.data, p.size as usize).to_vec(),
                    key: p.flags & ffi::AV_PKT_FLAG_KEY != 0,
                    ts_us: if p.pts == ffi::AV_NOPTS_VALUE {
                        0
                    } else {
                        p.pts as u64
                    },
                    encode_us: 0,
                });
                ffi::av_packet_unref(o.pkt.0);
            }
        }
        Ok(out)
    }

    /// Timing of the last `encode` call.
    #[must_use]
    pub fn last_timing(&self) -> EncodeTiming {
        self.timing
    }

    /// True if the zero-copy (D3D11 texture -> NVENC) path is in use.
    #[must_use]
    pub fn is_zero_copy(&self) -> bool {
        self.open.as_ref().is_some_and(|o| o.kind == Kind::ZeroCopy)
    }

    /// Short description of the path in use, for logs and the spike report.
    #[must_use]
    pub fn path_description(&self) -> String {
        match &self.open {
            None => "not opened yet".into(),
            Some(o) => match o.kind {
                Kind::ZeroCopy => format!(
                    "{} zero-copy (D3D11 BGRA -> video processor NV12 -> NVENC)",
                    o.name
                ),
                Kind::SystemMemory => {
                    format!("{} system-memory NV12 (GPU read-back / swscale)", o.name)
                }
            },
        }
    }

    fn open_now(&mut self, device: Option<&ID3D11Device>) -> Result<()> {
        let vendor = device.map_or(0, adapter_vendor);
        let names: Vec<String> = match &self.opts.encoder_name {
            Some(n) => vec![n.clone()],
            None => candidates(self.cfg.codec, vendor),
        };
        let mut last_err = PlatformError::other("no encoder candidates");
        for name in names {
            let zero_copy_allowed =
                name.ends_with("_nvenc") && device.is_some() && !self.opts.force_readback;
            match open_codec(
                &name,
                &self.cfg,
                &self.opts,
                self.kbps,
                device,
                zero_copy_allowed,
            ) {
                Ok(o) => {
                    tracing::info!("video encoder opened: {name} ({:?})", o.kind);
                    self.last_name = name;
                    self.open = Some(o);
                    return Ok(());
                }
                Err(e) => {
                    tracing::debug!("encoder {name} unavailable: {e}");
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    fn apply_bitrate(&mut self) -> Result<()> {
        if !self.pending_bitrate {
            return Ok(());
        }
        self.pending_bitrate = false;
        let Some(o) = self.open.as_mut() else {
            return Ok(());
        };
        // SAFETY: the context is open and owned by us; NVENC re-reads the rate control fields
        // when they change between frames.
        unsafe {
            let c = o.ctx.0;
            let bps = i64::from(self.kbps) * 1000;
            (*c).bit_rate = bps;
            (*c).rc_max_rate = bps;
            (*c).rc_buffer_size =
                ((bps as f64) * f64::from(self.opts.vbv_frames) / f64::from(self.cfg.fps)) as i32;
        }
        Ok(())
    }
}

impl VideoEncoder for FfmpegEncoder {
    fn encode(&mut self, frame: &Frame, force_key: bool) -> Result<Vec<EncodedPacket>> {
        // Lazy open on the capture device.
        if self.open.is_none() {
            let dev = match &frame.data {
                FrameData::Gpu(any) => GpuFrame::from_any(any.as_ref()).map(|g| g.device.clone()),
                FrameData::Bgra { .. } => None,
            };
            self.open_now(dev.as_ref())?;
        }
        self.apply_bitrate()?;
        let cfg_size = (self.cfg.width, self.cfg.height);
        let o = self.open.as_mut().expect("opened above");
        let t0 = Instant::now();

        // --- prepare the AVFrame ---
        // SAFETY: all pointers belong to `o`; FFmpeg API contracts are followed per call.
        unsafe {
            ffi::av_frame_unref(o.frame.0);
            let f = o.frame.0;
            match (&frame.data, o.kind) {
                (FrameData::Gpu(any), Kind::ZeroCopy) => {
                    let g = GpuFrame::from_any(any.as_ref())
                        .ok_or_else(|| PlatformError::other("GPU frame of unknown type"))?;
                    let (dev, ctx) = o.device.as_ref().expect("zero-copy has a device");
                    if g.device.as_raw() != dev.as_raw() {
                        return Err(PlatformError::other(
                            "frame lives on a different D3D11 device than the encoder",
                        ));
                    }
                    if o.vp
                        .as_ref()
                        .is_none_or(|v| v.in_size() != (g.width, g.height))
                    {
                        o.vp = Some(VpConverter::new(
                            dev,
                            ctx,
                            ConvKind::BgraToNv12,
                            (g.width, g.height),
                            cfg_size,
                        )?);
                    }
                    let r = ffi::av_hwframe_get_buffer(o.hw_frames.0, f, 0);
                    if r < 0 {
                        return Err(ff::ff_err("av_hwframe_get_buffer", r));
                    }
                    let tex_raw = (*f).data[0].cast::<c_void>();
                    let idx = (*f).data[1] as usize as u32;
                    let tex = ID3D11Texture2D::from_raw_borrowed(&tex_raw)
                        .ok_or_else(|| PlatformError::other("hw frame without texture"))?;
                    o.vp.as_mut().expect("converter ensured").blt(
                        &g.texture,
                        g.subresource,
                        tex,
                        idx,
                        None,
                    )?;
                    ctx.Flush();
                }
                (FrameData::Gpu(any), Kind::SystemMemory) => {
                    let g = GpuFrame::from_any(any.as_ref())
                        .ok_or_else(|| PlatformError::other("GPU frame of unknown type"))?;
                    let ctx = g
                        .device
                        .GetImmediateContext()
                        .map_err(|e| PlatformError::other(e.to_string()))?;
                    if o.readback
                        .as_ref()
                        .is_none_or(|r| r.in_size != (g.width, g.height))
                    {
                        o.readback = Some(make_readback(
                            &g.device,
                            &ctx,
                            (g.width, g.height),
                            cfg_size,
                        )?);
                    }
                    let rb = o.readback.as_mut().expect("readback ensured");
                    sys_frame_prepare(f, cfg_size, o.sw_fmt)?;
                    readback_nv12(rb, &ctx, g, f, cfg_size)?;
                }
                (FrameData::Bgra { data, stride }, _) => {
                    sys_frame_prepare(f, cfg_size, o.sw_fmt)?;
                    if o.sws
                        .as_ref()
                        .is_none_or(|s| s.1 != (frame.width, frame.height))
                    {
                        o.sws = Some(make_sws(frame.width, frame.height, cfg_size, o.sw_fmt)?);
                    }
                    if data.len() < stride * frame.height as usize {
                        return Err(PlatformError::other("BGRA frame buffer too small"));
                    }
                    let src = [
                        data.as_ptr(),
                        std::ptr::null(),
                        std::ptr::null(),
                        std::ptr::null(),
                    ];
                    let src_stride = [*stride as i32, 0, 0, 0];
                    ffi::sws_scale(
                        o.sws.as_ref().expect("sws ensured").0,
                        src.as_ptr(),
                        src_stride.as_ptr(),
                        0,
                        frame.height as i32,
                        (*f).data.as_ptr(),
                        (*f).linesize.as_ptr(),
                    );
                }
            }
            (*f).pts = frame.ts_us as i64;
            (*f).pict_type = if force_key {
                ffi::AVPictureType::AV_PICTURE_TYPE_I
            } else {
                ffi::AVPictureType::AV_PICTURE_TYPE_NONE
            };
            if force_key {
                (*f).flags |= ffi::AV_FRAME_FLAG_KEY;
            }
            (*f).colorspace = ffi::AVColorSpace::AVCOL_SPC_BT709;
            (*f).color_range = ffi::AVColorRange::AVCOL_RANGE_MPEG;
        }
        let prepared = Instant::now();

        // --- encode ---
        // SAFETY: the frame was prepared above; packets are copied out before being unref'd.
        let out = unsafe {
            let r = ffi::avcodec_send_frame(o.ctx.0, o.frame.0);
            if r < 0 {
                return Err(ff::ff_err("avcodec_send_frame", r));
            }
            let mut out = Vec::with_capacity(1);
            loop {
                let r = ffi::avcodec_receive_packet(o.ctx.0, o.pkt.0);
                if ff::is_again(r) || r == ff::AVERROR_EOF {
                    break;
                }
                if r < 0 {
                    return Err(ff::ff_err("avcodec_receive_packet", r));
                }
                let p = &*o.pkt.0;
                let data = std::slice::from_raw_parts(p.data, p.size as usize).to_vec();
                let ts = if p.pts == ffi::AV_NOPTS_VALUE {
                    frame.ts_us
                } else {
                    p.pts as u64
                };
                out.push(EncodedPacket {
                    data,
                    key: p.flags & ffi::AV_PKT_FLAG_KEY != 0,
                    ts_us: ts,
                    encode_us: 0,
                });
                ffi::av_packet_unref(o.pkt.0);
            }
            out
        };
        let done = Instant::now();
        self.timing = EncodeTiming {
            prepare_us: (prepared - t0).as_micros() as u32,
            encode_us: (done - prepared).as_micros() as u32,
        };
        let total = (done - t0).as_micros() as u32;
        let mut out = out;
        for p in &mut out {
            p.encode_us = total;
        }
        self.frames += 1;
        Ok(out)
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<()> {
        let kbps = kbps.max(1_000);
        if kbps != self.kbps {
            self.kbps = kbps;
            self.pending_bitrate = true;
        }
        Ok(())
    }

    fn name(&self) -> String {
        if self.last_name.is_empty() {
            format!("{:?} (not opened)", self.cfg.codec)
        } else {
            self.last_name.clone()
        }
    }

    fn describe(&self) -> String {
        self.path_description()
    }
}

// ---------------------------------------------------------------------------------------------
// Opening the codec
// ---------------------------------------------------------------------------------------------

fn open_codec(
    name: &str,
    cfg: &EncoderConfig,
    opts: &EncoderOptions,
    kbps: u32,
    device: Option<&ID3D11Device>,
    zero_copy: bool,
) -> Result<Open> {
    let cname = CString::new(name).map_err(|_| PlatformError::other("bad encoder name"))?;
    // SAFETY: standard FFmpeg setup sequence; every allocation is owned by an RAII wrapper that
    // is dropped on early return.
    unsafe {
        let codec = ffi::avcodec_find_encoder_by_name(cname.as_ptr());
        if codec.is_null() {
            return Err(PlatformError::DeviceNotFound(format!(
                "FFmpeg encoder {name}"
            )));
        }
        let c = ffi::avcodec_alloc_context3(codec);
        if c.is_null() {
            return Err(PlatformError::other("avcodec_alloc_context3 failed"));
        }
        let ctx = CodecCtx(c);
        (*c).width = cfg.width as i32;
        (*c).height = cfg.height as i32;
        (*c).time_base = ffi::AVRational {
            num: 1,
            den: 1_000_000,
        };
        (*c).framerate = ffi::AVRational {
            num: cfg.fps as i32,
            den: 1,
        };
        let bps = i64::from(kbps) * 1000;
        (*c).bit_rate = bps;
        (*c).rc_max_rate = bps;
        (*c).rc_buffer_size =
            ((bps as f64) * f64::from(opts.vbv_frames) / f64::from(cfg.fps)) as i32;
        if name.starts_with("lib") {
            // software test encoders do not accept a one-frame VBV
            (*c).rc_max_rate = 0;
            (*c).rc_buffer_size = 0;
            (*c).bit_rate = 0;
        }
        (*c).max_b_frames = 0;
        (*c).has_b_frames = 0;
        (*c).flags |= ffi::AV_CODEC_FLAG_LOW_DELAY as i32;
        (*c).thread_count = 1;
        (*c).colorspace = ffi::AVColorSpace::AVCOL_SPC_BT709;
        (*c).color_primaries = ffi::AVColorPrimaries::AVCOL_PRI_BT709;
        (*c).color_trc = ffi::AVColorTransferCharacteristic::AVCOL_TRC_BT709;
        (*c).color_range = ffi::AVColorRange::AVCOL_RANGE_MPEG;
        if !name.ends_with("_nvenc") {
            // Effectively infinite GOP; keyframes only on request.
            (*c).gop_size = 1_000_000;
        }

        let mut hw_device = BufRef::null();
        let mut hw_frames = BufRef::null();
        let mut dev_pair = None;
        let kind;
        let mut sw_fmt = ffi::AVPixelFormat::AV_PIX_FMT_NV12;
        if zero_copy {
            let dev = device.ok_or_else(|| PlatformError::other("zero-copy needs a device"))?;
            let immediate = dev
                .GetImmediateContext()
                .map_err(|e| PlatformError::other(e.to_string()))?;
            enable_multithread_protection(&immediate);
            hw_device = BufRef(ffi::av_hwdevice_ctx_alloc(
                ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
            ));
            if hw_device.is_null() {
                return Err(PlatformError::other("av_hwdevice_ctx_alloc failed"));
            }
            let dctx = (*hw_device.0).data.cast::<ffi::AVHWDeviceContext>();
            let dh = (*dctx).hwctx.cast::<AvD3d11vaDeviceContext>();
            // FFmpeg releases `device` (and the context it derives) when the device context dies,
            // so hand over our own reference.
            (*dh).device = dev.clone().into_raw();
            let r = ffi::av_hwdevice_ctx_init(hw_device.0);
            if r < 0 {
                return Err(ff::ff_err("av_hwdevice_ctx_init(d3d11va)", r));
            }
            hw_frames = BufRef(ffi::av_hwframe_ctx_alloc(hw_device.0));
            if hw_frames.is_null() {
                return Err(PlatformError::other("av_hwframe_ctx_alloc failed"));
            }
            let fc = (*hw_frames.0).data.cast::<ffi::AVHWFramesContext>();
            (*fc).format = ffi::AVPixelFormat::AV_PIX_FMT_D3D11;
            (*fc).sw_format = ffi::AVPixelFormat::AV_PIX_FMT_NV12;
            (*fc).width = cfg.width as i32;
            (*fc).height = cfg.height as i32;
            (*fc).initial_pool_size = 0; // NV12 texture arrays need BIND_DECODER; 0 = one texture per frame, recycled by the buffer pool
            let fh = (*fc).hwctx.cast::<AvD3d11vaFramesContext>();
            (*fh).bind_flags = (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
            let r = ffi::av_hwframe_ctx_init(hw_frames.0);
            if r < 0 {
                return Err(ff::ff_err("av_hwframe_ctx_init", r));
            }
            (*c).pix_fmt = ffi::AVPixelFormat::AV_PIX_FMT_D3D11;
            (*c).hw_frames_ctx = hw_frames.new_ref();
            (*c).hw_device_ctx = hw_device.new_ref();
            dev_pair = Some((dev.clone(), immediate));
            kind = Kind::ZeroCopy;
        } else {
            sw_fmt = if matches!(name, "libsvtav1" | "libaom-av1" | "librav1e") {
                ffi::AVPixelFormat::AV_PIX_FMT_YUV420P
            } else {
                ffi::AVPixelFormat::AV_PIX_FMT_NV12
            };
            (*c).pix_fmt = sw_fmt;
            kind = Kind::SystemMemory;
        }

        let mut d = Dict::new();
        set_family_options(&mut d, name, opts);
        let r = ffi::avcodec_open2(c, codec, &mut d.0);
        if r < 0 {
            return Err(ff::ff_err(&format!("avcodec_open2({name})"), r));
        }
        let unused = d.leftover();
        if !unused.is_empty() {
            tracing::debug!("encoder {name}: unused options {unused:?}");
        }
        Ok(Open {
            name: name.to_owned(),
            kind,
            sw_fmt,
            ctx,
            frame: ff::Frame::alloc()?,
            pkt: ff::Packet::alloc()?,
            hw_frames,
            _hw_device: hw_device,
            device: dev_pair,
            vp: None,
            readback: None,
            sws: None,
        })
    }
}

fn set_family_options(d: &mut Dict, name: &str, opts: &EncoderOptions) {
    if name.ends_with("_nvenc") {
        d.set("preset", &opts.preset);
        d.set("tune", &opts.tune);
        d.set("rc", "cbr");
        d.set("zerolatency", "1");
        d.set("delay", "0");
        d.set("forced-idr", "1");
        d.set("rc-lookahead", "0");
        d.set("spatial-aq", "0");
        d.set("temporal-aq", "0");
        d.set("multipass", "disabled");
        d.set("no-scenecut", "1");
        d.set("g", "-1");
        if opts.intra_refresh && !name.starts_with("av1") {
            d.set("intra-refresh", "1");
        }
        if name.starts_with("hevc") {
            d.set("profile", "main");
        } else if name.starts_with("h264") {
            d.set("profile", "high");
        }
    } else if name.ends_with("_qsv") {
        d.set("preset", "veryfast");
        d.set("async_depth", "1");
        d.set("look_ahead", "0");
        d.set("forced_idr", "1");
        d.set("scenario", "displayremoting");
    } else if name.ends_with("_amf") {
        d.set("usage", "ultralowlatency");
        d.set("quality", "speed");
        d.set("rc", "cbr");
        d.set("preanalysis", "0");
        d.set("forced_idr", "1");
    } else if name == "libsvtav1" {
        // Software AV1 is only used by the spike to produce a test stream for decode benchmarks.
        d.set("preset", "12");
        d.set("crf", "36");
        d.set("svtav1-params", "pred-struct=1");
        d.set("g", "100000");
    } else if name == "libopenh264" {
        d.set("allow_skip_frames", "0");
    }
}

/// Prepares `f` as a system-memory picture (NV12 or YUV420P) of `size`.
unsafe fn sys_frame_prepare(
    f: *mut ffi::AVFrame,
    size: (u32, u32),
    fmt: ffi::AVPixelFormat,
) -> Result<()> {
    // SAFETY: `f` is a valid, unreferenced frame.
    unsafe {
        (*f).format = fmt as i32;
        (*f).width = size.0 as i32;
        (*f).height = size.1 as i32;
        let r = ffi::av_frame_get_buffer(f, 32);
        if r < 0 {
            return Err(ff::ff_err("av_frame_get_buffer", r));
        }
    }
    Ok(())
}

fn make_sws(src_w: u32, src_h: u32, dst: (u32, u32), fmt: ffi::AVPixelFormat) -> Result<Sws> {
    // SAFETY: plain swscale setup; coefficient tables are static.
    unsafe {
        let s = ffi::sws_getContext(
            src_w as i32,
            src_h as i32,
            ffi::AVPixelFormat::AV_PIX_FMT_BGRA,
            dst.0 as i32,
            dst.1 as i32,
            fmt,
            ffi::SwsFlags::SWS_BILINEAR as i32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        if s.is_null() {
            return Err(PlatformError::other("sws_getContext failed"));
        }
        let co = ffi::sws_getCoefficients(ffi::SWS_CS_ITU709);
        // full-range RGB source, limited-range BT.709 destination
        ffi::sws_setColorspaceDetails(s, co, 1, co, 0, 0, 1 << 16, 1 << 16);
        Ok(Sws(s, (src_w, src_h)))
    }
}

fn make_readback(
    device: &ID3D11Device,
    ctx: &ID3D11DeviceContext,
    in_size: (u32, u32),
    out_size: (u32, u32),
) -> Result<Readback> {
    let vp = VpConverter::new(device, ctx, ConvKind::BgraToNv12, in_size, out_size)?;
    let mk = |usage, bind: u32, cpu: u32| -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: out_size.0,
            Height: out_size.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: usage,
            BindFlags: bind,
            CPUAccessFlags: cpu,
            MiscFlags: 0,
        };
        let mut t: Option<ID3D11Texture2D> = None;
        // SAFETY: valid descriptor and out pointer.
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut t)) }
            .map_err(|e| PlatformError::other(format!("CreateTexture2D(NV12): {e}")))?;
        t.ok_or_else(|| PlatformError::other("null texture"))
    };
    Ok(Readback {
        vp,
        nv12: mk(D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET.0 as u32, 0)?,
        staging: mk(D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ.0 as u32)?,
        in_size,
    })
}

/// GPU BGRA -> NV12 (video processor) -> staging -> AVFrame planes.
unsafe fn readback_nv12(
    rb: &mut Readback,
    ctx: &ID3D11DeviceContext,
    g: &GpuFrame,
    f: *mut ffi::AVFrame,
    size: (u32, u32),
) -> Result<()> {
    rb.vp.blt(&g.texture, g.subresource, &rb.nv12, 0, None)?;
    let dst: ID3D11Resource = rb
        .staging
        .cast()
        .map_err(|e| PlatformError::other(e.to_string()))?;
    let src: ID3D11Resource = rb
        .nv12
        .cast()
        .map_err(|e| PlatformError::other(e.to_string()))?;
    let mut m = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: the staging texture is CPU-readable; rows are copied within the mapped pitch and
    // within the AVFrame plane sizes (same dimensions).
    unsafe {
        ctx.CopyResource(&dst, &src);
        ctx.Map(&dst, 0, D3D11_MAP_READ, 0, Some(&mut m))
            .map_err(|e| PlatformError::other(format!("Map(staging): {e}")))?;
        let base = m.pData.cast::<u8>();
        let pitch = m.RowPitch as usize;
        let (w, h) = (size.0 as usize, size.1 as usize);
        for y in 0..h {
            std::ptr::copy_nonoverlapping(
                base.add(y * pitch),
                (*f).data[0].add(y * (*f).linesize[0] as usize),
                w,
            );
        }
        let uv = base.add(h * pitch);
        if (*f).format == ffi::AVPixelFormat::AV_PIX_FMT_YUV420P as i32 {
            // de-interleave UV for planar encoders
            for y in 0..h / 2 {
                let row = uv.add(y * pitch);
                let (du, dv) = (
                    (*f).data[1].add(y * (*f).linesize[1] as usize),
                    (*f).data[2].add(y * (*f).linesize[2] as usize),
                );
                for x in 0..w / 2 {
                    *du.add(x) = *row.add(2 * x);
                    *dv.add(x) = *row.add(2 * x + 1);
                }
            }
        } else {
            for y in 0..h / 2 {
                std::ptr::copy_nonoverlapping(
                    uv.add(y * pitch),
                    (*f).data[1].add(y * (*f).linesize[1] as usize),
                    w,
                );
            }
        }
        ctx.Unmap(&dst, 0);
    }
    Ok(())
}

/// Adapter description helper for logs.
#[must_use]
pub fn describe_device(device: &ID3D11Device) -> String {
    let (luid, name) = device_adapter_info(device);
    format!("{name} (LUID {luid:#x})")
}
