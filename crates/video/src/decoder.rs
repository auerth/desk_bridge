//! FFmpeg decoder (`VideoDecoder`): D3D11VA hardware decode into a texture of the caller's
//! D3D11 device (zero-copy hand-over to the presenter), with a software fallback that produces
//! BGRA in system memory.

use std::ffi::c_void;
use std::time::Instant;

use deskbridge_platform::video::{Frame, FrameData, VideoCodec, VideoDecoder};
use deskbridge_platform::{PlatformError, Result};
use deskbridge_platform_windows::video::{
    GpuFrame, device_adapter_info, enable_multithread_protection,
};
use ffmpeg_next::ffi;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_DECODER, D3D11_BIND_SHADER_RESOURCE, ID3D11Device, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12;
use windows::core::Interface;

use crate::ff::{self, AvD3d11vaDeviceContext, AvD3d11vaFramesContext, BufRef, CodecCtx};

/// Decoder-pool headroom beyond what the codec needs (frames held by the presenter, queueing).
const EXTRA_HW_FRAMES: i32 = 4;

/// Keeps a decoded hardware frame (and thereby its slot in the decoder pool) alive.
struct HeldFrame(#[allow(dead_code)] ff::Frame);

struct Hw {
    device: ID3D11Device,
    luid: i64,
    _hw_device: BufRef,
    /// A picture was successfully decoded through D3D11VA at least once.
    confirmed: bool,
}

/// FFmpeg video decoder.
pub struct FfmpegDecoder {
    codec: VideoCodec,
    ctx: CodecCtx,
    pkt: ff::Packet,
    frame: ff::Frame,
    hw: Option<Hw>,
    sws: Option<(*mut ffi::SwsContext, (i32, i32, i32))>,
    name: String,
    last_decode_us: u32,
    adapter_hint: Option<ID3D11Device>,
}

// SAFETY: raw FFmpeg objects are owned and only used through `&mut self`.
unsafe impl Send for FfmpegDecoder {}

impl Drop for FfmpegDecoder {
    fn drop(&mut self) {
        if let Some((s, _)) = self.sws.take() {
            // SAFETY: we own the swscale context.
            unsafe { ffi::sws_freeContext(s) };
        }
    }
}

unsafe extern "C" fn get_hw_format(
    ctx: *mut ffi::AVCodecContext,
    fmts: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    // SAFETY: FFmpeg passes a valid context and a NONE-terminated list.
    unsafe {
        let mut p = fmts;
        while *p != ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            if *p == ffi::AVPixelFormat::AV_PIX_FMT_D3D11 {
                let mut frames: *mut ffi::AVBufferRef = std::ptr::null_mut();
                let r = ffi::avcodec_get_hw_frames_parameters(
                    ctx,
                    (*ctx).hw_device_ctx,
                    ffi::AVPixelFormat::AV_PIX_FMT_D3D11,
                    &mut frames,
                );
                if r < 0 || frames.is_null() {
                    return ffi::AVPixelFormat::AV_PIX_FMT_NONE;
                }
                let fc = (*frames).data.cast::<ffi::AVHWFramesContext>();
                let fh = (*fc).hwctx.cast::<AvD3d11vaFramesContext>();
                // The presenter samples the decoded NV12 surface through the video processor.
                (*fh).bind_flags |= (D3D11_BIND_DECODER.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
                if ffi::av_hwframe_ctx_init(frames) < 0 {
                    ffi::av_buffer_unref(&mut frames);
                    return ffi::AVPixelFormat::AV_PIX_FMT_NONE;
                }
                (*ctx).hw_frames_ctx = frames;
                return ffi::AVPixelFormat::AV_PIX_FMT_D3D11;
            }
            p = p.add(1);
        }
    }
    ffi::AVPixelFormat::AV_PIX_FMT_NONE
}

fn codec_id(c: VideoCodec) -> ffi::AVCodecID {
    match c {
        VideoCodec::H264 => ffi::AVCodecID::AV_CODEC_ID_H264,
        VideoCodec::Hevc => ffi::AVCodecID::AV_CODEC_ID_HEVC,
        VideoCodec::Av1 => ffi::AVCodecID::AV_CODEC_ID_AV1,
    }
}

fn native_decoder_name(c: VideoCodec) -> &'static std::ffi::CStr {
    match c {
        VideoCodec::H264 => c"h264",
        VideoCodec::Hevc => c"hevc",
        VideoCodec::Av1 => c"av1",
    }
}

impl FfmpegDecoder {
    /// Hardware decoder on `device` (D3D11VA); falls back to software when the device is `None`
    /// or the hardware path cannot be set up.
    pub fn new(codec: VideoCodec, device: Option<&ID3D11Device>) -> Result<Self> {
        ff::init();
        if let Some(dev) = device {
            match Self::open_hw(codec, dev) {
                Ok(d) => return Ok(d),
                Err(e) => {
                    tracing::warn!("D3D11VA decoder unavailable ({e}); using software decode")
                }
            }
        }
        Self::open_sw(codec, device.cloned())
    }

    /// Software decoder only (system-memory BGRA output).
    pub fn new_software(codec: VideoCodec) -> Result<Self> {
        ff::init();
        Self::open_sw(codec, None)
    }

    /// True while decoding through D3D11VA.
    #[must_use]
    pub fn is_hardware(&self) -> bool {
        self.hw.is_some()
    }

    /// Duration of the last `decode` call in microseconds.
    #[must_use]
    pub fn last_decode_us(&self) -> u32 {
        self.last_decode_us
    }

    fn open_hw(codec: VideoCodec, dev: &ID3D11Device) -> Result<Self> {
        // SAFETY: standard FFmpeg setup; RAII wrappers free everything on early return.
        unsafe {
            let dec = ffi::avcodec_find_decoder_by_name(native_decoder_name(codec).as_ptr());
            if dec.is_null() {
                return Err(PlatformError::DeviceNotFound(
                    "FFmpeg native decoder".into(),
                ));
            }
            let immediate = dev
                .GetImmediateContext()
                .map_err(|e| PlatformError::other(e.to_string()))?;
            enable_multithread_protection(&immediate);
            let hw_dev = BufRef(ffi::av_hwdevice_ctx_alloc(
                ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
            ));
            if hw_dev.is_null() {
                return Err(PlatformError::other("av_hwdevice_ctx_alloc failed"));
            }
            let dctx = (*hw_dev.0).data.cast::<ffi::AVHWDeviceContext>();
            let dh = (*dctx).hwctx.cast::<AvD3d11vaDeviceContext>();
            (*dh).device = dev.clone().into_raw();
            let r = ffi::av_hwdevice_ctx_init(hw_dev.0);
            if r < 0 {
                return Err(ff::ff_err("av_hwdevice_ctx_init(d3d11va)", r));
            }
            let c = ffi::avcodec_alloc_context3(dec);
            if c.is_null() {
                return Err(PlatformError::other("avcodec_alloc_context3 failed"));
            }
            let ctx = CodecCtx(c);
            (*c).hw_device_ctx = hw_dev.new_ref();
            (*c).get_format = Some(get_hw_format);
            (*c).extra_hw_frames = EXTRA_HW_FRAMES;
            (*c).flags |= ffi::AV_CODEC_FLAG_LOW_DELAY as i32;
            (*c).thread_count = 1;
            let r = ffi::avcodec_open2(c, dec, std::ptr::null_mut());
            if r < 0 {
                return Err(ff::ff_err("avcodec_open2(hw decoder)", r));
            }
            let (luid, name) = device_adapter_info(dev);
            Ok(Self {
                codec,
                ctx,
                pkt: ff::Packet::alloc()?,
                frame: ff::Frame::alloc()?,
                hw: Some(Hw {
                    device: dev.clone(),
                    luid,
                    _hw_device: hw_dev,
                    confirmed: false,
                }),
                sws: None,
                name: format!(
                    "{} (d3d11va on {name})",
                    native_decoder_name(codec).to_string_lossy()
                ),
                last_decode_us: 0,
                adapter_hint: Some(dev.clone()),
            })
        }
    }

    fn open_sw(codec: VideoCodec, hint: Option<ID3D11Device>) -> Result<Self> {
        // SAFETY: standard FFmpeg setup.
        unsafe {
            // For AV1 this picks libdav1d (the native "av1" decoder is hardware-only).
            let dec = ffi::avcodec_find_decoder(codec_id(codec));
            if dec.is_null() {
                return Err(PlatformError::DeviceNotFound(
                    "FFmpeg software decoder".into(),
                ));
            }
            let c = ffi::avcodec_alloc_context3(dec);
            if c.is_null() {
                return Err(PlatformError::other("avcodec_alloc_context3 failed"));
            }
            let ctx = CodecCtx(c);
            (*c).flags |= ffi::AV_CODEC_FLAG_LOW_DELAY as i32;
            (*c).thread_count = 8;
            (*c).thread_type = ffi::FF_THREAD_SLICE;
            let r = ffi::avcodec_open2(c, dec, std::ptr::null_mut());
            if r < 0 {
                return Err(ff::ff_err("avcodec_open2(sw decoder)", r));
            }
            let name = std::ffi::CStr::from_ptr((*dec).name)
                .to_string_lossy()
                .into_owned();
            Ok(Self {
                codec,
                ctx,
                pkt: ff::Packet::alloc()?,
                frame: ff::Frame::alloc()?,
                hw: None,
                sws: None,
                name: format!("{name} (software)"),
                last_decode_us: 0,
                adapter_hint: hint,
            })
        }
    }

    fn switch_to_software(&mut self) -> Result<()> {
        let sw = Self::open_sw(self.codec, self.adapter_hint.clone())?;
        *self = sw;
        Ok(())
    }

    /// Sends one packet and drains the decoder.
    fn decode_inner(&mut self, data: &[u8]) -> Result<Option<Frame>> {
        // SAFETY: the packet owns a padded copy of `data`; frames are moved or converted before
        // the next call.
        unsafe {
            ffi::av_packet_unref(self.pkt.0);
            let r = ffi::av_new_packet(self.pkt.0, data.len() as i32);
            if r < 0 {
                return Err(ff::ff_err("av_new_packet", r));
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), (*self.pkt.0).data, data.len());
            let r = ffi::avcodec_send_packet(self.ctx.0, self.pkt.0);
            if r < 0 && !ff::is_again(r) {
                return Err(ff::ff_err("avcodec_send_packet", r));
            }
            let mut result = None;
            loop {
                let r = ffi::avcodec_receive_frame(self.ctx.0, self.frame.0);
                if ff::is_again(r) || r == ff::AVERROR_EOF {
                    break;
                }
                if r < 0 {
                    return Err(ff::ff_err("avcodec_receive_frame", r));
                }
                // No B-frames: one picture per packet; if several came out keep the newest.
                result = Some(self.take_frame()?);
            }
            Ok(result)
        }
    }

    unsafe fn take_frame(&mut self) -> Result<Frame> {
        // SAFETY: `self.frame` holds a freshly decoded frame.
        unsafe {
            let f = self.frame.0;
            let (w, h) = ((*f).width as u32, (*f).height as u32);
            let ts = if (*f).pts == ffi::AV_NOPTS_VALUE {
                0
            } else {
                (*f).pts as u64
            };
            if (*f).format == ffi::AVPixelFormat::AV_PIX_FMT_D3D11 as i32 {
                let hw = self
                    .hw
                    .as_mut()
                    .ok_or_else(|| PlatformError::other("hw frame without hw state"))?;
                hw.confirmed = true;
                let tex_raw = (*f).data[0].cast::<c_void>();
                let idx = (*f).data[1] as usize as u32;
                let tex = ID3D11Texture2D::from_raw_borrowed(&tex_raw)
                    .ok_or_else(|| PlatformError::other("decoded frame without texture"))?
                    .clone();
                let mut held = ff::Frame::alloc()?;
                ffi::av_frame_move_ref(held.0, f);
                let _ = &mut held;
                return Ok(Frame {
                    width: w,
                    height: h,
                    ts_us: ts,
                    data: FrameData::Gpu(Box::new(GpuFrame {
                        texture: tex,
                        device: hw.device.clone(),
                        adapter_luid: hw.luid,
                        subresource: idx,
                        format: DXGI_FORMAT_NV12,
                        width: w,
                        height: h,
                        hold: Some(Box::new(HeldFrame(held))),
                    })),
                });
            }
            // Software picture -> BGRA.
            let key = ((*f).format, (*f).width, (*f).height);
            let need_new = !matches!(&self.sws, Some((_, k)) if *k == key);
            if need_new {
                if let Some((s, _)) = self.sws.take() {
                    ffi::sws_freeContext(s);
                }
                let src_fmt: ffi::AVPixelFormat = std::mem::transmute((*f).format);
                let s = ffi::sws_getContext(
                    (*f).width,
                    (*f).height,
                    src_fmt,
                    (*f).width,
                    (*f).height,
                    ffi::AVPixelFormat::AV_PIX_FMT_BGRA,
                    ffi::SwsFlags::SWS_BILINEAR as i32,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                );
                if s.is_null() {
                    return Err(PlatformError::other(
                        "sws_getContext failed for decoded picture",
                    ));
                }
                let co = ffi::sws_getCoefficients(ffi::SWS_CS_ITU709);
                ffi::sws_setColorspaceDetails(s, co, 0, co, 1, 0, 1 << 16, 1 << 16);
                self.sws = Some((s, key));
            }
            let stride = w as usize * 4;
            let mut out = vec![0u8; stride * h as usize];
            let dst = [
                out.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ];
            let dst_stride = [stride as i32, 0, 0, 0];
            ffi::sws_scale(
                self.sws.as_ref().expect("sws just created").0,
                (*f).data.as_ptr().cast(),
                (*f).linesize.as_ptr(),
                0,
                (*f).height,
                dst.as_ptr(),
                dst_stride.as_ptr(),
            );
            ffi::av_frame_unref(f);
            Ok(Frame {
                width: w,
                height: h,
                ts_us: ts,
                data: FrameData::Bgra { data: out, stride },
            })
        }
    }
}

impl VideoDecoder for FfmpegDecoder {
    fn decode(&mut self, data: &[u8], _key: bool) -> Result<Option<Frame>> {
        let t0 = Instant::now();
        let r = match self.decode_inner(data) {
            Err(e) if self.hw.as_ref().is_some_and(|h| !h.confirmed) => {
                // Hardware path never produced a picture: switch to software and retry this packet.
                tracing::warn!(
                    "D3D11VA decode failed before the first picture ({e}); switching to software"
                );
                self.switch_to_software()?;
                self.decode_inner(data)
            }
            other => other,
        };
        self.last_decode_us = t0.elapsed().as_micros() as u32;
        r
    }

    fn name(&self) -> String {
        self.name.clone()
    }
}
