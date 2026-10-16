//! Adaptive jitter buffer, loss concealment and clock-drift compensation.
//!
//! Data flow:
//!
//! ```text
//! on_packet (network task)                         fill (render callback)
//!   reorder window (128 slots)                       ensure data (decode parked / conceal)
//!   -> decode in order (Opus / PCM)  -> ring ------> windowed-sinc read (ratio ~ 1 +- 0.2 %)
//!                                                    -> fade / volume -> stereo f32 out
//! ```
//!
//! Everything lives behind one short-held `parking_lot::Mutex`; `fill` never allocates.
//!
//! ## Playout control
//!
//! * `target_ms` is the desired *mean* buffered audio right after a render pull. It is
//!   `packet/2 + 5 ms + 1.3 * peak inter-arrival jitter`, clamped to the configured range;
//!   it starts at 10 ms (wired) / 20 ms and shrinks slowly, but jumps up immediately.
//! * The buffer level is measured as a *fluid* level (packets that just arrived are
//!   counted proportionally to their age) so the 5/10 ms packet granularity does not alias
//!   with the render period. A PI controller turns the smoothed level error into the
//!   resampling ratio; its integrator converges to the true clock drift.
//! * Underrun: fade out, silence, re-buffer to the target, fade in. Overrun (burst):
//!   cross-fade forward to the target level. Silence keep-alives never add audio, so the
//!   buffer simply drains and re-centres when the signal resumes.

use crate::clock::{Clock, SystemClock};
use crate::error::Result;
use crate::level::LevelMeter;
use crate::opus::{MAX_DECODE_FRAMES, OpusDecoder, packet_frames};
use crate::resampler::{self, LOOKAHEAD, LOOKBEHIND};
use deskbridge_platform::audio::{PullSource, RATE};
use deskbridge_proto::media::{AudioPacket, audio_flags};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Reorder window (packets).
const SLOTS: usize = 128;
const SLOT_MASK: usize = SLOTS - 1;
/// Largest datagram payload we keep (one QUIC datagram).
const SLOT_CAP: usize = 1500;
const SEEN: usize = 256;
/// Ring capacity in frames (power of two).
const RING_FRAMES: usize = 1 << 15;
/// Frames of the ring the read side may still need behind the read pointer.
const RING_KEEP: usize = LOOKBEHIND + 2;
/// Frames beyond the integer read position the interpolation touches (+1 phase carry).
const LOOK_OVERHEAD: usize = LOOKAHEAD + 1;
/// Extra frames that must be available beyond a block before it is played (underrun guard).
const GUARD: usize = 96;
/// Fade length for underrun / restart ramps.
const FADE: usize = 64;
/// Cross-fade length when dropping audio on overrun.
const XF_LEN: usize = 240;
/// Opus decoder start-up frames (encoder look-ahead) dropped after PCM.
const OPUS_PRESKIP: usize = 120;
/// Extension generated behind PCM concealment for the cross-fade to the next packet.
const PLC_XF: usize = 64;
const PLC_HIST: usize = 1024;
const PLC_WIN: usize = 384;
const PLC_MIN_LAG: usize = 32;
const PLC_MAX_LAG: usize = 640;
/// Time constant of the concealment fade (frames) and hard mute point.
const PLC_TAU: f32 = 720.0;
const PLC_MUTE_AFTER: usize = 2400;
const MAX_PCM_FRAMES: usize = 4096;
/// Smoothing of the mean transit time, in packets.
const TAU_EMA_PACKETS: f64 = 64.0;

const FRAMES_PER_MS: f64 = 48.0;
/// Smoothing time constant of the buffer level (seconds).
const LEVEL_TAU_S: f64 = 1.0;
/// PI controller gains: ratio change per ms of level error / per ms and second.
const KP: f64 = 3.0e-4;
const KI: f64 = 2.25e-5;
/// The integrator only runs while the level error is below this (ms).
const INT_ERR_MAX_MS: f64 = 3.0;
const U_MAX: f64 = 2.0e-3;
const I_MAX: f64 = 1.5e-3;
/// Maximum ratio slew (relative change per second).
const SLEW_PER_S: f64 = 4.0e-4;
/// Margin on top of half a packet in the target formula (ms): guard + safety.
const TARGET_BASE_MS: f32 = 3.0;
const TARGET_JITTER_K: f32 = 1.3;
/// Loss rate (percent) at which the target includes a full extra packet.
const LOSS_FULL_PCT: f32 = 0.5;
/// Target shrink rate (ms per second).
const TARGET_DECAY_MS_PER_S: f32 = 1.0;
/// Jitter peak decay time constant (seconds).
const JITTER_PEAK_TAU_S: f32 = 10.0;
/// Smoothing factor of the loss / late statistics (per packet).
const RATE_ALPHA: f32 = 1.0 / 500.0;
const LOSS_SLOW_ALPHA: f32 = 1.0 / 2000.0;
/// Integrator freeze after a target step (microseconds).
const INT_FREEZE_US: u64 = 4_000_000;

#[derive(Debug, Clone)]
pub struct ReceiverConfig {
    /// 1 (mic) or 2 (speaker). Informational: the per-packet `MONO` flag is authoritative.
    pub channels_in: u16,
    /// Wired link: start with a 10 ms target instead of 20 ms.
    pub wired: bool,
    pub min_ms: f32,
    pub max_ms: f32,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            channels_in: 2,
            wired: true,
            min_ms: 5.0,
            max_ms: 60.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayState {
    /// Waiting for the buffer to fill (start, after an underrun, after silence).
    #[default]
    Buffering,
    Playing,
}

#[derive(Debug, Clone, Default)]
pub struct ReceiverStats {
    /// Smoothed buffered audio right after a render pull (ms).
    pub fill_ms: f32,
    /// Desired value of `fill_ms`.
    pub target_ms: f32,
    pub loss_pct: f32,
    pub late_pct: f32,
    pub plc_count: u64,
    pub underruns: u64,
    pub overruns: u64,
    /// Render clock speed relative to the sender clock (positive: render is faster).
    pub drift_ppm: f32,
    /// Input frames consumed per output frame.
    pub resample_ratio: f64,
    pub packets: u64,
    /// Smoothed inter-arrival jitter (ms) and its decaying peak.
    pub jitter_ms: f32,
    pub jitter_peak_ms: f32,
    pub duplicates: u64,
    pub late: u64,
    pub resyncs: u64,
    pub state: PlayState,
}

// ---------------------------------------------------------------------------------------
// Ring
// ---------------------------------------------------------------------------------------

struct Ring {
    buf: Vec<f32>,
    mask: usize,
    wr: u64,
    rd: u64,
    /// Fractional read position in `[0, 1)` after `rd`.
    phase: f64,
}

impl Ring {
    fn new() -> Self {
        let start = 1u64 << 20;
        Self {
            buf: vec![0.0; RING_FRAMES * 2],
            mask: RING_FRAMES - 1,
            wr: start,
            rd: start,
            phase: 0.0,
        }
    }

    fn avail(&self) -> usize {
        (self.wr - self.rd) as usize
    }

    fn free(&self) -> usize {
        RING_FRAMES.saturating_sub(self.avail() + RING_KEEP)
    }

    /// Appends stereo frames; returns how many were accepted.
    fn push(&mut self, src: &[f32]) -> usize {
        let frames = (src.len() / 2).min(self.free());
        let start = (self.wr as usize) & self.mask;
        let first = frames.min(RING_FRAMES - start);
        self.buf[start * 2..(start + first) * 2].copy_from_slice(&src[..first * 2]);
        if frames > first {
            self.buf[..(frames - first) * 2].copy_from_slice(&src[first * 2..frames * 2]);
        }
        self.wr += frames as u64;
        frames
    }

    /// Frame `back` positions before the write pointer (`back = 1`: newest frame).
    fn tail_frame(&self, back: usize) -> (f32, f32) {
        let i = (self.wr as usize).wrapping_sub(back) & self.mask;
        (self.buf[i * 2], self.buf[i * 2 + 1])
    }

    fn clear(&mut self) {
        self.rd = self.wr;
        self.phase = 0.0;
    }
}

// ---------------------------------------------------------------------------------------
// Reorder slots
// ---------------------------------------------------------------------------------------

struct Slot {
    ext: u64,
    valid: bool,
    flags: u8,
    frames: u32,
    ts_us: u64,
    data: Vec<u8>,
}

// ---------------------------------------------------------------------------------------
// Core
// ---------------------------------------------------------------------------------------

struct Core {
    cfg: ReceiverConfig,
    // --- sequence / reorder
    synced: bool,
    next_ext: u64,
    highest_ext: u64,
    slots: Vec<Slot>,
    seen: Vec<u64>,
    parked_count: usize,
    parked_frames: usize,
    stray: u32,
    // --- decoding
    dec_mono: OpusDecoder,
    dec_stereo: OpusDecoder,
    dec_buf: Vec<f32>,
    st_buf: Vec<f32>,
    last_dur: usize,
    last_was_opus: bool,
    last_mono: bool,
    dec_cold: bool,
    silence_announced: bool,
    // --- concealment
    plc_run: usize,
    plc_pos: usize,
    plc_period: Vec<f32>,
    plc_p: usize,
    plc_tail: Vec<f32>,
    plc_tail_len: usize,
    plc_mono: Vec<f32>,
    // --- ring / playout
    ring: Ring,
    state: PlayState,
    fade_gain: f32,
    xf_off: u64,
    xf_left: usize,
    ratio: f64,
    drift_i: f64,
    fill_f: f64,
    ctrl_init: bool,
    target_ms: f32,
    last_target_us: u64,
    // --- jitter / media timeline
    /// Smoothed transit time (arrival - media end) in microseconds.
    tau_bar: f64,
    tau_init: bool,
    /// Media end (sender clock, us) of the newest audio in the ring.
    end_last: i64,
    jit_avg: f32,
    jit_peak: f32,
    last_peak_us: u64,
    // --- volume / meter
    vol_cur: f32,
    meter: LevelMeter,
    // --- statistics
    packets: u64,
    plc_count: u64,
    underruns: u64,
    overruns: u64,
    duplicates: u64,
    late: u64,
    resyncs: u64,
    loss_ema: f32,
    /// Slowly smoothed loss ratio used for the target (no wobble).
    loss_slow: f32,
    /// The integrator is frozen until this time (after a step of the target).
    int_freeze_until_us: u64,
    late_ema: f32,
}

impl Core {
    fn new(cfg: ReceiverConfig) -> Result<Self> {
        let initial: f32 = if cfg.wired { 10.0 } else { 20.0 };
        let target_ms = initial.clamp(cfg.min_ms, cfg.max_ms);
        let slots = (0..SLOTS)
            .map(|_| Slot {
                ext: 0,
                valid: false,
                flags: 0,
                frames: 0,
                ts_us: 0,
                data: Vec::with_capacity(SLOT_CAP),
            })
            .collect();
        Ok(Self {
            cfg,
            synced: false,
            next_ext: 0,
            highest_ext: 0,
            slots,
            seen: vec![0; SEEN],
            parked_count: 0,
            parked_frames: 0,
            stray: 0,
            dec_mono: OpusDecoder::new(1)?,
            dec_stereo: OpusDecoder::new(2)?,
            dec_buf: vec![0.0; MAX_DECODE_FRAMES * 2],
            st_buf: vec![0.0; MAX_DECODE_FRAMES * 2 + PLC_XF * 2],
            last_dur: 240,
            last_was_opus: false,
            last_mono: false,
            dec_cold: true,
            silence_announced: false,
            plc_run: 0,
            plc_pos: 0,
            plc_period: vec![0.0; PLC_MAX_LAG * 2],
            plc_p: 0,
            plc_tail: vec![0.0; PLC_XF * 2],
            plc_tail_len: 0,
            plc_mono: vec![0.0; PLC_HIST],
            ring: Ring::new(),
            state: PlayState::Buffering,
            fade_gain: 0.0,
            xf_off: 0,
            xf_left: 0,
            ratio: 1.0,
            drift_i: 0.0,
            fill_f: 0.0,
            ctrl_init: false,
            target_ms,
            last_target_us: 0,
            tau_bar: 0.0,
            tau_init: false,
            end_last: 0,
            jit_avg: 0.0,
            jit_peak: 0.0,
            last_peak_us: 0,
            vol_cur: 1.0,
            meter: LevelMeter::new(300.0),
            packets: 0,
            plc_count: 0,
            underruns: 0,
            overruns: 0,
            duplicates: 0,
            late: 0,
            resyncs: 0,
            loss_ema: 0.0,
            loss_slow: 0.0,
            int_freeze_until_us: 0,
            late_ema: 0.0,
        })
    }

    fn reset(&mut self) {
        self.synced = false;
        for s in &mut self.slots {
            s.valid = false;
            s.data.clear();
        }
        self.seen.fill(0);
        self.parked_count = 0;
        self.parked_frames = 0;
        self.stray = 0;
        self.dec_cold = true;
        self.silence_announced = false;
        self.plc_run = 0;
        self.plc_pos = 0;
        self.plc_tail_len = 0;
        self.ring.clear();
        self.state = PlayState::Buffering;
        self.fade_gain = 0.0;
        self.xf_left = 0;
        self.ctrl_init = false;
        self.tau_init = false;
    }

    // -----------------------------------------------------------------------------------
    // Network side
    // -----------------------------------------------------------------------------------

    fn on_packet(&mut self, pkt: &AudioPacket, now: u64) {
        self.packets += 1;
        if pkt.payload.len() > SLOT_CAP {
            return;
        }
        let silence = pkt.flags & audio_flags::SILENCE != 0;
        let mono = pkt.flags & audio_flags::MONO != 0;
        let frames = if silence {
            0
        } else if pkt.flags & audio_flags::OPUS != 0 {
            match packet_frames(&pkt.payload) {
                Some(f) => f,
                None => return,
            }
        } else {
            let f = pkt.payload.len() / (if mono { 2 } else { 4 });
            if f == 0 {
                return;
            }
            f.min(MAX_PCM_FRAMES)
        };

        if !self.synced {
            self.synced = true;
            self.next_ext = (1u64 << 32) | u64::from(pkt.seq);
            self.highest_ext = self.next_ext;
        }
        let diff = i64::from(pkt.seq.wrapping_sub(self.next_ext as u16) as i16);
        let ext = (self.next_ext as i64 + diff) as u64;

        self.update_jitter(pkt, frames, now);

        let dup = {
            let s = &mut self.seen[(ext as usize) & (SEEN - 1)];
            let d = *s == ext + 1;
            *s = ext + 1;
            d
        };

        if diff < 0 {
            if dup {
                self.duplicates += 1;
            } else if diff >= -(SLOTS as i64) / 2 {
                self.late += 1;
                self.late_ema += (1.0 - self.late_ema) * RATE_ALPHA;
            } else {
                self.note_stray(pkt.seq);
            }
            return;
        }
        if diff >= SLOTS as i64 {
            self.note_stray(pkt.seq);
            return;
        }
        self.stray = 0;
        if dup {
            self.duplicates += 1;
            return;
        }
        self.late_ema -= self.late_ema * RATE_ALPHA;

        if diff == 0 {
            self.highest_ext = self.highest_ext.max(ext);
            self.consume(pkt.flags, pkt.ts_us, &pkt.payload);
            self.drain_parked();
        } else {
            let idx = (ext as usize) & SLOT_MASK;
            let slot = &mut self.slots[idx];
            if slot.valid {
                self.parked_count -= 1;
                self.parked_frames -= slot.frames as usize;
            }
            slot.valid = true;
            slot.ext = ext;
            slot.flags = pkt.flags;
            slot.frames = frames as u32;
            slot.ts_us = pkt.ts_us;
            slot.data.clear();
            slot.data.extend_from_slice(&pkt.payload);
            self.parked_count += 1;
            self.parked_frames += frames;
            self.highest_ext = self.highest_ext.max(ext);
            // Never let the window overflow: conceal what is still missing.
            while self.highest_ext.saturating_sub(self.next_ext) >= (SLOTS - 8) as u64 {
                self.conceal_one();
                self.drain_parked();
            }
        }
    }

    /// A packet far outside the window: after three in a row assume a sender restart.
    fn note_stray(&mut self, seq: u16) {
        self.stray += 1;
        if self.stray >= 3 {
            self.stray = 0;
            self.resyncs += 1;
            for s in &mut self.slots {
                s.valid = false;
            }
            self.parked_count = 0;
            self.parked_frames = 0;
            self.next_ext = (self.next_ext & !0xFFFF) + 0x1_0000 + u64::from(seq);
            self.highest_ext = self.next_ext;
            self.tau_init = false;
            self.dec_cold = true;
        }
    }

    fn update_jitter(&mut self, pkt: &AudioPacket, frames: usize, now: u64) {
        if pkt.flags & audio_flags::SILENCE != 0 {
            return;
        }
        // Opus output lags its input by the encoder look-ahead (see `append`).
        let lag = if pkt.flags & audio_flags::OPUS != 0 {
            frames_us(OPUS_PRESKIP)
        } else {
            0
        };
        let transit = (now as i64 - (pkt.ts_us as i64 + frames_us(frames) - lag)) as f64;
        if !self.tau_init {
            self.tau_init = true;
            self.tau_bar = transit;
            self.last_peak_us = now;
        }
        let dev = ((transit - self.tau_bar) * 1e-3) as f32; // ms, positive = late
        self.tau_bar += (transit - self.tau_bar) / TAU_EMA_PACKETS;
        self.jit_avg += (dev.abs() - self.jit_avg) / 16.0;
        let dt = now.saturating_sub(self.last_peak_us) as f32 * 1e-6;
        self.jit_peak = (self.jit_peak * (-dt / JITTER_PEAK_TAU_S).exp()).max(dev);
        self.last_peak_us = now;
        self.update_target(frames, now);
    }

    fn update_target(&mut self, frames: usize, now: u64) {
        // The fluid level falls by one packet duration between arrivals, then by the
        // lateness of the next packet: keep both plus a guard above zero.
        let d_ms = frames.max(240) as f32 / 48.0;
        // With measurable loss keep one more packet so a single loss can be concealed
        // (the next packet arrives one packet duration late).
        let loss_term = d_ms * (self.loss_slow * 100.0 / LOSS_FULL_PCT).min(1.0);
        let raw = (d_ms + loss_term + TARGET_BASE_MS + TARGET_JITTER_K * self.jit_peak)
            .clamp(self.cfg.min_ms, self.cfg.max_ms);
        let dt = now.saturating_sub(self.last_target_us) as f32 * 1e-6;
        self.last_target_us = now;
        if raw > self.target_ms {
            if raw > self.target_ms + 1.0 {
                // Let the level settle before the integrator learns again.
                self.int_freeze_until_us = now + INT_FREEZE_US;
            }
            self.target_ms = raw;
        } else {
            self.target_ms = raw.max(self.target_ms - TARGET_DECAY_MS_PER_S * dt.min(1.0));
        }
    }

    fn drain_parked(&mut self) {
        while self.parked_count > 0 {
            let idx = (self.next_ext as usize) & SLOT_MASK;
            if !(self.slots[idx].valid && self.slots[idx].ext == self.next_ext) {
                break;
            }
            let mut data = std::mem::take(&mut self.slots[idx].data);
            let flags = self.slots[idx].flags;
            let ts = self.slots[idx].ts_us;
            self.slots[idx].valid = false;
            self.parked_count -= 1;
            self.parked_frames -= self.slots[idx].frames as usize;
            self.consume(flags, ts, &data);
            data.clear();
            self.slots[idx].data = data;
        }
        if self.parked_count == 0 {
            self.parked_frames = 0;
        }
    }

    /// Plays the in-order packet `next_ext` and advances.
    fn consume(&mut self, flags: u8, ts_us: u64, payload: &[u8]) {
        if flags & audio_flags::SILENCE != 0 {
            self.silence_announced = true;
            self.advance_ok();
            return;
        }
        self.silence_announced = false;
        match self.decode_payload(flags, payload) {
            Some((frames, skip)) => {
                self.append(frames, skip, ts_us, flags & audio_flags::OPUS != 0);
                self.advance_ok();
            }
            None => self.conceal_one(),
        }
    }

    fn advance_ok(&mut self) {
        self.next_ext += 1;
        self.loss_ema -= self.loss_ema * RATE_ALPHA;
        self.loss_slow -= self.loss_slow * LOSS_SLOW_ALPHA;
    }

    /// Decodes into `st_buf` (stereo). Returns `(frames, frames_to_skip)`.
    fn decode_payload(&mut self, flags: u8, payload: &[u8]) -> Option<(usize, usize)> {
        let mono = flags & audio_flags::MONO != 0;
        if flags & audio_flags::OPUS != 0 {
            let dec = if mono {
                &mut self.dec_mono
            } else {
                &mut self.dec_stereo
            };
            if self.dec_cold {
                // The sender restarted its encoder for this stretch of Opus packets.
                dec.reset();
            }
            let frames = dec.decode(payload, &mut self.dec_buf).ok()?;
            if mono {
                for i in 0..frames {
                    let v = self.dec_buf[i];
                    self.st_buf[2 * i] = v;
                    self.st_buf[2 * i + 1] = v;
                }
            } else {
                self.st_buf[..frames * 2].copy_from_slice(&self.dec_buf[..frames * 2]);
            }
            let skip = if self.dec_cold {
                frames.min(OPUS_PRESKIP)
            } else {
                0
            };
            self.dec_cold = false;
            self.last_was_opus = true;
            self.last_mono = mono;
            self.last_dur = frames;
            Some((frames, skip))
        } else {
            let ch = if mono { 1 } else { 2 };
            let frames = (payload.len() / (2 * ch)).min(MAX_PCM_FRAMES);
            if frames == 0 {
                return None;
            }
            for i in 0..frames {
                let rd = |c: usize| {
                    let o = (i * ch + c) * 2;
                    f32::from(i16::from_le_bytes([payload[o], payload[o + 1]])) / 32768.0
                };
                let l = rd(0);
                let r = if mono { l } else { rd(1) };
                self.st_buf[2 * i] = l;
                self.st_buf[2 * i + 1] = r;
            }
            self.dec_cold = true;
            self.last_was_opus = false;
            self.last_mono = mono;
            self.last_dur = frames;
            Some((frames, 0))
        }
    }

    /// Appends `st_buf[skip..frames]` to the ring (cross-fading from concealment).
    fn append(&mut self, frames: usize, skip: usize, ts_us: u64, opus: bool) {
        if self.plc_tail_len > 0 {
            let x = self.plc_tail_len.min(frames - skip);
            for i in 0..x {
                let w = (i + 1) as f32 / (x + 1) as f32;
                for c in 0..2 {
                    let s = &mut self.st_buf[(skip + i) * 2 + c];
                    *s = self.plc_tail[i * 2 + c] * (1.0 - w) + *s * w;
                }
            }
            self.plc_tail_len = 0;
        }
        self.plc_run = 0;
        self.plc_pos = 0;
        let got = self.ring.push(&self.st_buf[skip * 2..frames * 2]);
        if got < frames - skip {
            self.overruns += 1;
        }
        // Media end of the ring content; Opus output lags its input by the look-ahead.
        let lag = if opus { frames_us(OPUS_PRESKIP) } else { 0 };
        self.end_last = ts_us as i64 + frames_us(frames) - lag;
    }

    // -----------------------------------------------------------------------------------
    // Concealment
    // -----------------------------------------------------------------------------------

    /// Conceals the missing packet `next_ext` and advances past it.
    fn conceal_one(&mut self) {
        self.plc_count += 1;
        self.loss_ema += (1.0 - self.loss_ema) * RATE_ALPHA;
        self.loss_slow += (1.0 - self.loss_slow) * LOSS_SLOW_ALPHA;
        let dur = self.last_dur.clamp(120, 960);
        self.end_last += frames_us(dur);
        let mut done = false;
        if self.last_was_opus && !self.dec_cold {
            let frames = (dur / 120).max(1) * 120;
            let ch = if self.last_mono { 1 } else { 2 };
            let dec = if self.last_mono {
                &mut self.dec_mono
            } else {
                &mut self.dec_stereo
            };
            if let Ok(n) = dec.conceal(&mut self.dec_buf[..frames * ch]) {
                if ch == 1 {
                    for i in 0..n {
                        let v = self.dec_buf[i];
                        self.st_buf[2 * i] = v;
                        self.st_buf[2 * i + 1] = v;
                    }
                } else {
                    self.st_buf[..n * 2].copy_from_slice(&self.dec_buf[..n * 2]);
                }
                self.ring.push(&self.st_buf[..n * 2]);
                self.plc_run += 1;
                done = true;
            }
        }
        if !done {
            self.plc_pcm(dur);
            self.ring.push(&self.st_buf[..dur * 2]);
        }
        self.next_ext += 1;
    }

    /// Pitch-synchronous repetition of the last period with a fade-out. Fills
    /// `st_buf[..dur * 2]` and prepares the cross-fade tail.
    fn plc_pcm(&mut self, dur: usize) {
        if self.plc_run == 0 {
            self.plc_analyse();
        }
        let p = self.plc_p.max(1);
        for i in 0..dur + PLC_XF {
            let pos = self.plc_pos + i;
            let g = if pos >= PLC_MUTE_AFTER {
                0.0
            } else {
                (-((pos + 1) as f32) / PLC_TAU).exp()
            };
            let k = pos % p;
            let (l, r) = (self.plc_period[2 * k] * g, self.plc_period[2 * k + 1] * g);
            if i < dur {
                self.st_buf[2 * i] = l;
                self.st_buf[2 * i + 1] = r;
            } else {
                self.plc_tail[(i - dur) * 2] = l;
                self.plc_tail[(i - dur) * 2 + 1] = r;
            }
        }
        self.plc_tail_len = PLC_XF;
        self.plc_pos += dur;
        self.plc_run += 1;
    }

    /// Finds the dominant period of the newest audio and stores one period.
    fn plc_analyse(&mut self) {
        // Already played audio stays in the ring until it is overwritten, use it as history.
        for i in 0..PLC_HIST {
            let (l, r) = self.ring.tail_frame(PLC_HIST - i);
            self.plc_mono[i] = 0.5 * (l + r);
        }
        let m = &self.plc_mono;
        let n = PLC_HIST;
        let seg = &m[n - PLC_WIN..];
        let e1: f32 = seg.iter().map(|v| v * v).sum();
        let mut best = (0.0f32, PLC_MIN_LAG);
        let mut corr = [0f32; PLC_MAX_LAG + 1];
        if e1 > 1e-9 {
            for lag in PLC_MIN_LAG..=PLC_MAX_LAG {
                let lagged = &m[n - PLC_WIN - lag..n - lag];
                let (mut c, mut e2) = (0f32, 0f32);
                for (a, b) in seg.iter().zip(lagged) {
                    c += a * b;
                    e2 += b * b;
                }
                let v = if e2 > 1e-9 { c / (e1 * e2).sqrt() } else { 0.0 };
                corr[lag] = v;
                if v > best.0 {
                    best = (v, lag);
                }
            }
            // Prefer the shortest lag that is nearly as good as the best one.
            if let Some(lag) = (PLC_MIN_LAG..=best.1).find(|&l| corr[l] >= 0.995 * best.0) {
                best.1 = lag;
            }
        }
        self.plc_p = best.1;
        for k in 0..self.plc_p {
            let (l, r) = self.ring.tail_frame(self.plc_p - k);
            self.plc_period[2 * k] = l;
            self.plc_period[2 * k + 1] = r;
        }
    }

    // -----------------------------------------------------------------------------------
    // Render side
    // -----------------------------------------------------------------------------------

    /// Real playable level in frames (including data parked behind a gap).
    fn level_frames(&self) -> f64 {
        let ring = self.ring.avail() as f64 - self.ring.phase - LOOK_OVERHEAD as f64;
        let skipped = if self.xf_left > 0 {
            self.xf_off as f64
        } else {
            0.0
        };
        let mut extra = 0.0;
        if self.parked_count > 0 {
            let span = (self.highest_ext + 1).saturating_sub(self.next_ext) as usize;
            let holes = span.saturating_sub(self.parked_count);
            extra = (self.parked_frames + holes * self.last_dur) as f64;
        }
        ring - skipped + extra
    }

    /// Fluid level in frames: ring content plus the age of the newest packet relative to
    /// its expected arrival (`end + mean transit`). This equals "playout delay minus mean
    /// network delay" and has no packet-granularity sawtooth.
    fn fluid_frames(&self, now: u64) -> f64 {
        let ring = self.ring.avail() as f64 - self.ring.phase - LOOK_OVERHEAD as f64;
        let skipped = if self.xf_left > 0 {
            self.xf_off as f64
        } else {
            0.0
        };
        let age_us = now as f64 - self.tau_bar - self.end_last as f64;
        ring - skipped + age_us * 1e-6 * f64::from(RATE)
    }

    fn start_gain_target(muted_vol: f32) -> f32 {
        muted_vol
    }

    fn pull(&mut self, out: &mut [f32], now: u64, gain_target: f32) {
        let n = out.len() / 2;
        if n == 0 {
            out.fill(0.0);
            return;
        }
        let (body, tail) = out.split_at_mut(n * 2);
        tail.fill(0.0);

        if self.state == PlayState::Buffering {
            let consume = n as f64 * self.ratio;
            let ready = self.synced
                && self.tau_init
                && self.level_frames() >= consume + (GUARD + 48) as f64
                && self.fluid_frames(now) >= consume + f64::from(self.target_ms) * FRAMES_PER_MS;
            if ready {
                self.state = PlayState::Playing;
                self.fade_gain = 0.0;
                self.ctrl_init = false;
            } else {
                body.fill(0.0);
                self.meter.process_frames(body, n);
                self.apply_volume(body, gain_target);
                return;
            }
        }
        self.play_block(body, n, now);
        self.meter.process_frames(body, n);
        self.apply_volume(body, gain_target);
    }

    /// Applies the volume ramp and a final safety clip to [-1, 1] (also maps NaN to a
    /// finite value) so corrupt Opus data can never blast the headset.
    fn apply_volume(&mut self, out: &mut [f32], target: f32) {
        let step = 1.0 / 480.0;
        for f in out.chunks_exact_mut(2) {
            if self.vol_cur < target {
                self.vol_cur = (self.vol_cur + step).min(target);
            } else if self.vol_cur > target {
                self.vol_cur = (self.vol_cur - step).max(target);
            }
            f[0] = (f[0] * self.vol_cur).max(-1.0).min(1.0);
            f[1] = (f[1] * self.vol_cur).max(-1.0).min(1.0);
        }
    }

    fn ensure_data(&mut self, want: usize) {
        while self.ring.avail() < want {
            let idx = (self.next_ext as usize) & SLOT_MASK;
            if self.parked_count > 0 {
                if self.slots[idx].valid && self.slots[idx].ext == self.next_ext {
                    self.drain_parked();
                } else {
                    self.conceal_one();
                    self.drain_parked();
                }
            } else {
                break;
            }
        }
    }

    fn check_overrun(&mut self, n: usize) {
        if self.xf_left > 0 {
            return;
        }
        let consume = n as f64 * self.ratio;
        let level = self.ring.avail() as f64 - self.ring.phase - LOOK_OVERHEAD as f64;
        let hi = f64::from(self.cfg.max_ms) * FRAMES_PER_MS + consume;
        if level > hi {
            let keep = f64::from(self.target_ms) * FRAMES_PER_MS + consume + GUARD as f64;
            let skip = (level - keep).floor();
            if skip >= XF_LEN as f64 {
                self.xf_off = skip as u64;
                self.xf_left = XF_LEN;
                self.overruns += 1;
            }
        }
    }

    fn play_block(&mut self, out: &mut [f32], n: usize, now: u64) {
        let r = self.ratio;
        self.check_overrun(n);
        let xf_off = if self.xf_left > 0 {
            self.xf_off as usize
        } else {
            0
        };
        let span = self.ring.phase + (n - 1) as f64 * r;
        let need = span.floor() as usize + LOOK_OVERHEAD + 1;
        let want = need + GUARD + xf_off;
        self.ensure_data(want);
        let avail = self.ring.avail();
        let full = avail >= want;

        let m = if full {
            n
        } else {
            let eff = avail.saturating_sub(xf_off) as f64;
            let x = (eff - (LOOK_OVERHEAD + 1) as f64 - self.ring.phase) / r;
            if x <= 0.0 {
                0
            } else {
                (x.ceil() as usize).min(n)
            }
        };
        if !full && self.xf_left > 0 {
            // Abandon the cross-fade: jump now.
            self.ring.rd = (self.ring.rd + self.xf_off).min(self.ring.wr);
            self.xf_left = 0;
        }
        let fo_len = if full { 0 } else { m.min(FADE) };
        let fo_start = m - fo_len;
        let mut fo_base = 1.0f32;
        let table = resampler::table();
        for j in 0..m {
            let (carry, row) = resampler::phase_row(self.ring.phase);
            let i0 = self.ring.rd + carry;
            let (mut l, mut rr) = resampler::interp(&self.ring.buf, self.ring.mask, i0, table, row);
            if self.xf_left > 0 {
                let (l2, r2) =
                    resampler::interp(&self.ring.buf, self.ring.mask, i0 + self.xf_off, table, row);
                let w = (XF_LEN - self.xf_left + 1) as f32 / XF_LEN as f32;
                l = l * (1.0 - w) + l2 * w;
                rr = rr * (1.0 - w) + r2 * w;
                self.xf_left -= 1;
                if self.xf_left == 0 {
                    self.ring.rd = (self.ring.rd + self.xf_off).min(self.ring.wr);
                }
            }
            let mut g = self.fade_gain;
            if j >= fo_start {
                if j == fo_start {
                    fo_base = self.fade_gain;
                }
                g = fo_base * (1.0 - (j - fo_start + 1) as f32 / fo_len as f32);
            } else if self.fade_gain < 1.0 {
                self.fade_gain = (self.fade_gain + 1.0 / FADE as f32).min(1.0);
            }
            out[2 * j] = l * g;
            out[2 * j + 1] = rr * g;
            self.ring.phase += r;
            let adv = self.ring.phase.floor();
            self.ring.phase -= adv;
            self.ring.rd += adv as u64;
        }
        out[m * 2..].fill(0.0);

        if full {
            self.update_control(now, n);
        } else {
            if !self.silence_announced {
                self.underruns += 1;
            }
            self.state = PlayState::Buffering;
            self.fade_gain = 0.0;
        }
    }

    fn update_control(&mut self, now: u64, n: usize) {
        let lvl_f = self.fluid_frames(now);
        if self.ctrl_init {
            let a = (n as f64 / (f64::from(RATE) * LEVEL_TAU_S)).min(1.0);
            self.fill_f += a * (lvl_f - self.fill_f);
        } else {
            self.fill_f = lvl_f;
            self.ctrl_init = true;
        }
        let target_f = f64::from(self.target_ms) * FRAMES_PER_MS;
        let err_ms = (self.fill_f - target_f) / FRAMES_PER_MS;
        let dt = n as f64 / f64::from(RATE);
        // Only integrate near the target: a large offset (start-up, after an underrun) is
        // pulled in by the proportional term alone, so the integrator cannot wind up.
        if err_ms.abs() <= INT_ERR_MAX_MS && now >= self.int_freeze_until_us {
            self.drift_i = (self.drift_i + KI * err_ms * dt).clamp(-I_MAX, I_MAX);
        }
        let u = (self.drift_i + KP * err_ms).clamp(-U_MAX, U_MAX);
        let max_step = SLEW_PER_S * dt;
        self.ratio += (1.0 + u - self.ratio).clamp(-max_step, max_step);
    }

    fn stats(&self) -> ReceiverStats {
        ReceiverStats {
            fill_ms: (self.fill_f / FRAMES_PER_MS) as f32,
            target_ms: self.target_ms,
            loss_pct: self.loss_ema * 100.0,
            late_pct: self.late_ema * 100.0,
            plc_count: self.plc_count,
            underruns: self.underruns,
            overruns: self.overruns,
            drift_ppm: (-self.drift_i * 1e6) as f32,
            resample_ratio: self.ratio,
            packets: self.packets,
            jitter_ms: self.jit_avg,
            jitter_peak_ms: self.jit_peak,
            duplicates: self.duplicates,
            late: self.late,
            resyncs: self.resyncs,
            state: self.state,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Public wrapper
// ---------------------------------------------------------------------------------------

struct Shared {
    core: Mutex<Core>,
    clock: Arc<dyn Clock>,
    volume: AtomicU32,
    muted: AtomicBool,
    peak: AtomicU32,
}

impl PullSource for Shared {
    fn fill(&self, out: &mut [f32]) {
        let now = self.clock.now_us();
        let gain = if self.muted.load(Ordering::Relaxed) {
            0.0
        } else {
            f32::from_bits(self.volume.load(Ordering::Relaxed))
        };
        let mut core = self.core.lock();
        core.pull(out, now, Core::start_gain_target(gain));
        self.peak
            .store(core.meter.peak().to_bits(), Ordering::Relaxed);
    }
}

/// Jitter buffer + decoder + drift compensation exposed as a [`PullSource`].
pub struct AudioReceiver {
    shared: Arc<Shared>,
}

impl AudioReceiver {
    pub fn new(cfg: ReceiverConfig) -> Result<Self> {
        Self::with_clock(cfg, Arc::new(SystemClock::new()))
    }

    /// Like [`new`](Self::new) with an explicit time source (simulations / tests).
    pub fn with_clock(cfg: ReceiverConfig, clock: Arc<dyn Clock>) -> Result<Self> {
        let core = Core::new(cfg)?;
        Ok(Self {
            shared: Arc::new(Shared {
                core: Mutex::new(core),
                clock,
                volume: AtomicU32::new(1.0f32.to_bits()),
                muted: AtomicBool::new(false),
                peak: AtomicU32::new(0),
            }),
        })
    }

    /// Thread-safe; called from the network task for every audio datagram.
    pub fn on_packet(&self, pkt: AudioPacket) {
        let now = self.shared.clock.now_us();
        self.shared.core.lock().on_packet(&pkt, now);
    }

    /// Pull source for the OS render stream (always interleaved stereo, 48 kHz).
    #[must_use]
    pub fn source(&self) -> Arc<dyn PullSource> {
        self.shared.clone()
    }

    #[must_use]
    pub fn stats(&self) -> ReceiverStats {
        self.shared.core.lock().stats()
    }

    /// Linear volume (`1.0` = unity); applied with a 10 ms ramp.
    pub fn set_volume(&self, v: f32) {
        self.shared
            .volume
            .store(v.clamp(0.0, 4.0).to_bits(), Ordering::Relaxed);
    }

    pub fn set_muted(&self, m: bool) {
        self.shared.muted.store(m, Ordering::Relaxed);
    }

    /// Decaying output peak (before volume is *not* applied: measured after).
    #[must_use]
    pub fn peak_level(&self) -> f32 {
        f32::from_bits(self.shared.peak.load(Ordering::Relaxed))
    }

    /// Drops all buffered audio and re-synchronises on the next packet.
    pub fn reset(&self) {
        self.shared.core.lock().reset();
    }
}

fn frames_us(frames: usize) -> i64 {
    (frames as u64 * 1_000_000 / u64::from(RATE)) as i64
}
