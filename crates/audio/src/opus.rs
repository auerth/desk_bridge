//! Thin safe wrapper around libopus (statically linked through `audiopus_sys`).
//!
//! All streams run at 48 kHz with `f32` interleaved samples.

use crate::error::{Error, Result};
use audiopus_sys as sys;
use std::ffi::CStr;
use std::os::raw::c_int;
use std::ptr::NonNull;

/// Sample rate of all Opus streams.
pub const OPUS_RATE: i32 = 48_000;
/// Largest packet we ever produce (libopus max for a single frame is 1275 bytes).
const MAX_PACKET: usize = 1500;
/// Largest frame (120 ms) a decoder call may produce, per channel.
pub const MAX_DECODE_FRAMES: usize = 5760;

/// Opus application mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpusApp {
    /// Optimised for speech (`OPUS_APPLICATION_VOIP`).
    Voip,
    /// `OPUS_APPLICATION_RESTRICTED_LOWDELAY`: CELT only, 2.5 ms look-ahead.
    LowDelay,
}

impl OpusApp {
    fn raw(self) -> c_int {
        match self {
            Self::Voip => sys::OPUS_APPLICATION_VOIP,
            Self::LowDelay => sys::OPUS_APPLICATION_RESTRICTED_LOWDELAY,
        }
    }
}

fn opus_err(code: c_int) -> Error {
    // SAFETY: `opus_strerror` returns a pointer to a static NUL terminated string.
    let msg = unsafe { CStr::from_ptr(sys::opus_strerror(code)) };
    Error::Opus(format!("{} ({code})", msg.to_string_lossy()))
}

fn check_channels(channels: u16) -> Result<c_int> {
    match channels {
        1 | 2 => Ok(c_int::from(channels)),
        _ => Err(Error::Invalid("channels must be 1 or 2")),
    }
}

/// Opus encoder (CBR-like low delay, 10 ms frames are the intended use).
pub struct OpusEncoder {
    st: NonNull<sys::OpusEncoder>,
    channels: usize,
    lookahead: usize,
}

// SAFETY: the encoder state is only accessed through `&mut self`; libopus keeps no
// thread-affine state.
unsafe impl Send for OpusEncoder {}

impl OpusEncoder {
    pub fn new(channels: u16, bitrate_bps: u32, app: OpusApp) -> Result<Self> {
        let ch = check_channels(channels)?;
        let mut err: c_int = 0;
        // SAFETY: plain FFI constructor, `err` is a valid out pointer.
        let raw = unsafe { sys::opus_encoder_create(OPUS_RATE, ch, app.raw(), &mut err) };
        if err != sys::OPUS_OK {
            return Err(opus_err(err));
        }
        let st = NonNull::new(raw).ok_or_else(|| opus_err(sys::OPUS_ALLOC_FAIL))?;
        let mut enc = Self {
            st,
            channels: usize::from(channels),
            lookahead: 0,
        };
        enc.set_bitrate(bitrate_bps)?;
        enc.ctl(sys::OPUS_SET_COMPLEXITY_REQUEST, 7)?;
        enc.ctl(sys::OPUS_SET_INBAND_FEC_REQUEST, 0)?;
        let mut la: c_int = 0;
        // SAFETY: GET_LOOKAHEAD takes an `opus_int32*`.
        let rc = unsafe {
            sys::opus_encoder_ctl(
                enc.st.as_ptr(),
                sys::OPUS_GET_LOOKAHEAD_REQUEST,
                &mut la as *mut c_int,
            )
        };
        if rc != sys::OPUS_OK {
            return Err(opus_err(rc));
        }
        enc.lookahead = usize::try_from(la).unwrap_or(0);
        Ok(enc)
    }

    fn ctl(&mut self, req: c_int, val: c_int) -> Result<()> {
        // SAFETY: all requests used here take a single `opus_int32` value.
        let rc = unsafe { sys::opus_encoder_ctl(self.st.as_ptr(), req, val) };
        if rc == sys::OPUS_OK {
            Ok(())
        } else {
            Err(opus_err(rc))
        }
    }

    /// Encoder look-ahead in samples per channel (decoded audio lags the input by this).
    #[must_use]
    pub fn lookahead(&self) -> usize {
        self.lookahead
    }

    /// Encodes one frame of interleaved samples (2.5/5/10/20/... ms, i.e. a multiple of
    /// 120 frames) and *replaces* the content of `out` with the packet.
    pub fn encode(&mut self, pcm: &[f32], out: &mut Vec<u8>) -> Result<()> {
        if pcm.is_empty() || pcm.len() % self.channels != 0 {
            return Err(Error::Invalid(
                "pcm length must be a multiple of the channel count",
            ));
        }
        let frames = pcm.len() / self.channels;
        out.clear();
        out.resize(MAX_PACKET, 0);
        // SAFETY: `pcm` holds `frames * channels` floats, `out` MAX_PACKET writable bytes.
        let n = unsafe {
            sys::opus_encode_float(
                self.st.as_ptr(),
                pcm.as_ptr(),
                frames as c_int,
                out.as_mut_ptr(),
                MAX_PACKET as i32,
            )
        };
        if n < 0 {
            out.clear();
            return Err(opus_err(n));
        }
        out.truncate(n as usize);
        Ok(())
    }

    /// Resets the codec state (equivalent to a fresh encoder, settings are kept).
    pub fn reset(&mut self) {
        // SAFETY: OPUS_RESET_STATE takes no argument.
        unsafe { sys::opus_encoder_ctl(self.st.as_ptr(), sys::OPUS_RESET_STATE) };
    }

    pub fn set_bitrate(&mut self, bps: u32) -> Result<()> {
        let bps = bps.clamp(6_000, 510_000);
        self.ctl(sys::OPUS_SET_BITRATE_REQUEST, bps as c_int)
    }
}

impl Drop for OpusEncoder {
    fn drop(&mut self) {
        // SAFETY: `st` was created by `opus_encoder_create` and is destroyed exactly once.
        unsafe { sys::opus_encoder_destroy(self.st.as_ptr()) }
    }
}

/// Opus decoder with packet-loss concealment.
pub struct OpusDecoder {
    st: NonNull<sys::OpusDecoder>,
    channels: usize,
}

// SAFETY: see `OpusEncoder`.
unsafe impl Send for OpusDecoder {}

impl OpusDecoder {
    pub fn new(channels: u16) -> Result<Self> {
        let ch = check_channels(channels)?;
        let mut err: c_int = 0;
        // SAFETY: plain FFI constructor, `err` is a valid out pointer.
        let raw = unsafe { sys::opus_decoder_create(OPUS_RATE, ch, &mut err) };
        if err != sys::OPUS_OK {
            return Err(opus_err(err));
        }
        let st = NonNull::new(raw).ok_or_else(|| opus_err(sys::OPUS_ALLOC_FAIL))?;
        Ok(Self {
            st,
            channels: usize::from(channels),
        })
    }

    #[must_use]
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Resets the decoder to the state of a fresh decoder.
    pub fn reset(&mut self) {
        // SAFETY: OPUS_RESET_STATE takes no argument.
        unsafe { sys::opus_decoder_ctl(self.st.as_ptr(), sys::OPUS_RESET_STATE) };
    }

    /// Decodes one packet into `out` (interleaved). `out` must be large enough for the
    /// whole packet. Returns the number of frames per channel.
    pub fn decode(&mut self, data: &[u8], out: &mut [f32]) -> Result<usize> {
        if data.is_empty() {
            return Err(Error::Invalid("empty opus packet"));
        }
        let cap = (out.len() / self.channels).min(MAX_DECODE_FRAMES);
        // SAFETY: `data` is readable for `len` bytes, `out` writable for `cap * channels`.
        let n = unsafe {
            sys::opus_decode_float(
                self.st.as_ptr(),
                data.as_ptr(),
                data.len() as i32,
                out.as_mut_ptr(),
                cap as c_int,
                0,
            )
        };
        if n < 0 {
            Err(opus_err(n))
        } else {
            Ok(n as usize)
        }
    }

    /// Packet-loss concealment: synthesises `out.len() / channels` frames (must be a
    /// multiple of 2.5 ms = 120 frames).
    pub fn conceal(&mut self, out: &mut [f32]) -> Result<usize> {
        let cap = (out.len() / self.channels).min(MAX_DECODE_FRAMES);
        // SAFETY: a NULL packet with length 0 requests PLC; `out` is writable.
        let n = unsafe {
            sys::opus_decode_float(
                self.st.as_ptr(),
                std::ptr::null(),
                0,
                out.as_mut_ptr(),
                cap as c_int,
                0,
            )
        };
        if n < 0 {
            Err(opus_err(n))
        } else {
            Ok(n as usize)
        }
    }
}

impl Drop for OpusDecoder {
    fn drop(&mut self) {
        // SAFETY: `st` was created by `opus_decoder_create` and is destroyed exactly once.
        unsafe { sys::opus_decoder_destroy(self.st.as_ptr()) }
    }
}

/// Number of frames per channel in an Opus packet (from its TOC), `None` if invalid.
#[must_use]
pub fn packet_frames(data: &[u8]) -> Option<usize> {
    if data.is_empty() {
        return None;
    }
    // SAFETY: `data` is readable for its length.
    let n = unsafe { sys::opus_packet_get_nb_samples(data.as_ptr(), data.len() as i32, OPUS_RATE) };
    usize::try_from(n).ok().filter(|&n| n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frames: usize, ch: usize, f: f32, start: usize, amp: f32) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * ch);
        for i in 0..frames {
            let s = amp * (2.0 * std::f32::consts::PI * f * (start + i) as f32 / 48_000.0).sin();
            for _ in 0..ch {
                v.push(s);
            }
        }
        v
    }

    #[test]
    fn roundtrip_snr_stereo() {
        let mut enc = OpusEncoder::new(2, 128_000, OpusApp::LowDelay).unwrap();
        let mut dec = OpusDecoder::new(2).unwrap();
        let la = enc.lookahead();
        assert_eq!(la, 120);
        let total = 480 * 100;
        let src = sine(total, 2, 1000.0, 0, 0.5);
        let mut decoded: Vec<f32> = Vec::new();
        let mut pkt = Vec::new();
        let mut buf = vec![0.0f32; 480 * 2];
        for c in src.chunks(480 * 2) {
            enc.encode(c, &mut pkt).unwrap();
            assert!(!pkt.is_empty() && pkt.len() < 400, "packet {}", pkt.len());
            assert_eq!(packet_frames(&pkt), Some(480));
            let n = dec.decode(&pkt, &mut buf).unwrap();
            assert_eq!(n, 480);
            decoded.extend_from_slice(&buf);
        }
        // Compare after the codec delay, skipping the start-up transient.
        let (mut sig, mut err) = (0f64, 0f64);
        for f in 2000..total - 200 {
            let a = f64::from(src[(f - la) * 2]);
            let b = f64::from(decoded[f * 2]);
            sig += a * a;
            err += (a - b) * (a - b);
        }
        let snr = 10.0 * (sig / err).log10();
        assert!(snr > 25.0, "snr {snr} dB");
    }

    #[test]
    fn mono_voip_and_plc() {
        let mut enc = OpusEncoder::new(1, 64_000, OpusApp::Voip).unwrap();
        let mut dec = OpusDecoder::new(1).unwrap();
        let src = sine(480 * 30, 1, 440.0, 0, 0.4);
        let mut pkt = Vec::new();
        let mut buf = vec![0.0f32; 480];
        for (i, c) in src.chunks(480).enumerate() {
            enc.encode(c, &mut pkt).unwrap();
            if i == 20 {
                assert_eq!(dec.conceal(&mut buf).unwrap(), 480);
                // concealed audio continues the tone: not silent, not clipping
                let p = buf.iter().fold(0f32, |m, v| m.max(v.abs()));
                assert!(p > 0.05 && p < 0.9, "plc peak {p}");
            } else {
                assert_eq!(dec.decode(&pkt, &mut buf).unwrap(), 480);
            }
        }
        enc.set_bitrate(24_000).unwrap();
    }

    #[test]
    fn invalid_args() {
        assert!(OpusEncoder::new(3, 64_000, OpusApp::Voip).is_err());
        assert!(OpusDecoder::new(0).is_err());
        let mut enc = OpusEncoder::new(2, 64_000, OpusApp::LowDelay).unwrap();
        let mut out = Vec::new();
        assert!(enc.encode(&[0.0; 3], &mut out).is_err());
        assert!(enc.encode(&[0.0; 100], &mut out).is_err()); // 50 frames: not a valid frame size
        let mut dec = OpusDecoder::new(2).unwrap();
        let mut buf = vec![0.0; 960];
        assert!(dec.decode(&[], &mut buf).is_err());
    }
}
