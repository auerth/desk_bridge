//! Turns captured audio frames into datagram payloads.
//!
//! * re-frames arbitrary capture chunks into 5 ms (PCM) or 10 ms (Opus) packets,
//! * suppresses digital silence (after 200 ms only a keep-alive every 100 ms),
//! * switches PCM <-> Opus seamlessly (the codec is signalled per packet),
//! * stamps sequence numbers (`u16`, wrapping) and timestamps derived from the sample count.
//!
//! Codec switching detail: the Opus decoder output lags the encoder input by the
//! encoder look-ahead (`L` = 120 frames for `LowDelay`). To keep the switch sample
//! accurate the sender
//! * resets the encoder before the first Opus packet after PCM while the receiver resets
//!   its decoder and drops the first `L` decoded frames of that packet, and
//! * re-sends the last `L` input frames in a small extra PCM packet when it switches
//!   from Opus back to PCM (the Opus stream ended `L` frames before the input).

use crate::error::Result;
use crate::level::{LevelMeter, SilenceDetector};
use crate::opus::{OpusApp, OpusEncoder};
use crate::pcm::pcm_encode;
use bytes::Bytes;
use deskbridge_platform::audio::RATE;
use deskbridge_proto::media::{AudioPacket, audio_flags};

/// Frames per PCM packet (5 ms).
pub const PCM_PACKET_FRAMES: usize = 240;
/// Frames per Opus packet (10 ms).
pub const OPUS_PACKET_FRAMES: usize = 480;
/// Number of history frames kept for encoder priming / codec bridging.
const HIST_FRAMES: usize = 480;
/// A capture timestamp this far ahead of the expected one re-anchors the timeline.
const GAP_REANCHOR_US: u64 = 20_000;
/// Keep-alive interval during suppressed silence.
const KEEPALIVE_FRAMES: u64 = 4_800;
/// Auto policy: loss (percent) at which the sender falls back to Opus.
const AUTO_LOSS_TO_OPUS: f32 = 0.1;
/// Auto policy: loss (percent) below which the sender may return to PCM.
const AUTO_LOSS_TO_PCM: f32 = 0.05;
/// Auto policy: RTT above which a link is no longer treated as wired/low latency.
const AUTO_RTT_TO_OPUS_US: u32 = 5_000;
/// Auto policy: RTT below which the sender may return to PCM.
const AUTO_RTT_TO_PCM_US: u32 = 3_000;
/// Auto policy: minimum time on Opus before returning to PCM.
const AUTO_MIN_OPUS_FRAMES: u64 = 5 * RATE as u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecPolicy {
    /// PCM on a clean wired link, Opus otherwise.
    Auto,
    Pcm,
    Opus,
}

#[derive(Debug, Clone)]
pub struct SenderConfig {
    pub stream_id: u8,
    /// 2 for the speaker stream, 1 for the microphone stream.
    pub channels: u16,
    pub codec: CodecPolicy,
    pub opus_bitrate_bps: u32,
    pub opus_app: OpusApp,
}

pub struct AudioSender {
    cfg: SenderConfig,
    ch: usize,
    policy: CodecPolicy,
    enc: OpusEncoder,
    /// Interleaved samples not yet packetised (`acc[acc_off..]`).
    acc: Vec<f32>,
    acc_off: usize,
    base_ts_us: Option<u64>,
    /// Frames consumed (packetised or suppressed) since `base_ts_us`.
    frames_done: u64,
    seq: u16,
    use_opus: bool,
    /// Codec of the last packet that carried audio.
    last_sent_opus: Option<bool>,
    frames_since_switch: u64,
    hist: Vec<f32>,
    silence: SilenceDetector,
    was_suppressed: bool,
    ka_frames: u64,
    meter: LevelMeter,
    loss_pct: f32,
    rtt_us: u32,
    pcm_scratch: Vec<u8>,
    opus_scratch: Vec<u8>,
}

impl AudioSender {
    pub fn new(cfg: SenderConfig) -> Result<Self> {
        let ch = usize::from(cfg.channels);
        let enc = OpusEncoder::new(cfg.channels, cfg.opus_bitrate_bps, cfg.opus_app)?;
        let policy = cfg.codec;
        Ok(Self {
            ch,
            policy,
            enc,
            acc: Vec::with_capacity(OPUS_PACKET_FRAMES * ch * 4),
            acc_off: 0,
            base_ts_us: None,
            frames_done: 0,
            seq: 0,
            use_opus: policy == CodecPolicy::Opus,
            last_sent_opus: None,
            frames_since_switch: 0,
            hist: Vec::with_capacity(HIST_FRAMES * ch * 2),
            silence: SilenceDetector::default(),
            was_suppressed: false,
            ka_frames: 0,
            meter: LevelMeter::default(),
            loss_pct: 0.0,
            rtt_us: 0,
            pcm_scratch: Vec::with_capacity(2048),
            opus_scratch: Vec::with_capacity(1500),
            cfg,
        })
    }

    /// Feed arbitrary-length interleaved `f32` frames (48 kHz). `ts_us` is the capture time
    /// of the first frame in `samples`. Returns ready-to-send datagrams.
    pub fn push(&mut self, samples: &[f32], ts_us: u64) -> Vec<Bytes> {
        let mut out = Vec::new();
        let frames_in = samples.len() / self.ch;
        if frames_in == 0 {
            return out;
        }
        let samples = &samples[..frames_in * self.ch];
        let buffered = (self.acc.len() - self.acc_off) / self.ch;
        match self.base_ts_us {
            None => self.base_ts_us = Some(ts_us),
            Some(base) => {
                let expected = base + frames_to_us(self.frames_done + buffered as u64);
                if ts_us > expected + GAP_REANCHOR_US {
                    // Capture paused (e.g. loopback without playback): follow wall time.
                    self.base_ts_us = Some(base + (ts_us - expected));
                }
            }
        }
        self.meter.process_frames(samples, frames_in);
        self.acc.extend_from_slice(samples);

        loop {
            let want_opus = self.decide_codec();
            let need = if want_opus {
                OPUS_PACKET_FRAMES
            } else {
                PCM_PACKET_FRAMES
            };
            let avail = (self.acc.len() - self.acc_off) / self.ch;
            if avail < need {
                break;
            }
            self.packetise(need, want_opus, &mut out);
        }
        if self.acc_off > 0 {
            self.acc.drain(..self.acc_off);
            self.acc_off = 0;
        }
        out
    }

    /// Network quality feedback (loss in percent, round trip time in microseconds).
    pub fn set_link(&mut self, loss_pct: f32, rtt_us: u32) {
        self.loss_pct = loss_pct;
        self.rtt_us = rtt_us;
    }

    pub fn set_codec_policy(&mut self, p: CodecPolicy) {
        self.policy = p;
    }

    pub fn set_opus_bitrate(&mut self, bps: u32) {
        self.cfg.opus_bitrate_bps = bps;
        if let Err(e) = self.enc.set_bitrate(bps) {
            tracing::warn!("opus set_bitrate failed: {e}");
        }
    }

    /// Test hook: the sequence number of the next packet.
    pub fn set_start_seq(&mut self, seq: u16) {
        self.seq = seq;
    }

    #[must_use]
    pub fn current_codec_is_opus(&self) -> bool {
        self.use_opus
    }

    /// Peak level `0..1` since the previous call.
    pub fn peak_level(&mut self) -> f32 {
        self.meter.take_peak()
    }

    fn decide_codec(&mut self) -> bool {
        let want = match self.policy {
            CodecPolicy::Pcm => false,
            CodecPolicy::Opus => true,
            CodecPolicy::Auto => {
                if self.use_opus {
                    let calm = self.loss_pct < AUTO_LOSS_TO_PCM && self.rtt_us < AUTO_RTT_TO_PCM_US;
                    !(calm && self.frames_since_switch >= AUTO_MIN_OPUS_FRAMES)
                } else {
                    self.loss_pct >= AUTO_LOSS_TO_OPUS || self.rtt_us >= AUTO_RTT_TO_OPUS_US
                }
            }
        };
        if want != self.use_opus {
            self.use_opus = want;
            self.frames_since_switch = 0;
        }
        self.use_opus
    }

    fn ts_of(&self, frames: u64) -> u64 {
        self.base_ts_us.unwrap_or(0) + frames_to_us(frames)
    }

    fn next_seq(&mut self) -> u16 {
        let s = self.seq;
        self.seq = self.seq.wrapping_add(1);
        s
    }

    fn mono_flag(&self) -> u8 {
        if self.ch == 1 { audio_flags::MONO } else { 0 }
    }

    fn emit(&mut self, flags: u8, ts_us: u64, payload: Bytes, out: &mut Vec<Bytes>) {
        let pkt = AudioPacket {
            stream_id: self.cfg.stream_id,
            flags: flags | self.mono_flag(),
            seq: self.next_seq(),
            ts_us,
            payload,
        };
        out.push(pkt.encode());
    }

    fn packetise(&mut self, frames: usize, opus: bool, out: &mut Vec<Bytes>) {
        let (a, b) = (self.acc_off, self.acc_off + frames * self.ch);
        let ts = self.ts_of(self.frames_done);
        let suppressed = self.silence.update(&self.acc[a..b], frames);

        if suppressed {
            self.ka_frames += frames as u64;
            if !self.was_suppressed || self.ka_frames >= KEEPALIVE_FRAMES {
                self.ka_frames = 0;
                self.emit(audio_flags::SILENCE, ts, Bytes::new(), out);
            }
        } else {
            self.ka_frames = 0;
            if opus {
                self.send_opus(a, b, ts, out);
            } else {
                self.send_pcm(a, b, ts, out);
            }
        }
        self.was_suppressed = suppressed;

        // Keep the last HIST_FRAMES of consumed input for priming / bridging.
        self.hist.extend_from_slice(&self.acc[a..b]);
        let keep = HIST_FRAMES * self.ch;
        if self.hist.len() > keep {
            let excess = self.hist.len() - keep;
            self.hist.drain(..excess);
        }
        self.acc_off = b;
        self.frames_done += frames as u64;
        self.frames_since_switch += frames as u64;
    }

    fn send_pcm(&mut self, a: usize, b: usize, ts: u64, out: &mut Vec<Bytes>) {
        if self.last_sent_opus == Some(true) {
            // The Opus stream ended `lookahead` frames before the input: send them again.
            let la = self.enc.lookahead().min(self.hist.len() / self.ch);
            if la > 0 {
                let mut buf = std::mem::take(&mut self.pcm_scratch);
                buf.clear();
                pcm_encode(&self.hist[self.hist.len() - la * self.ch..], &mut buf);
                let ts_bridge = ts.saturating_sub(frames_to_us(la as u64));
                self.emit(0, ts_bridge, Bytes::copy_from_slice(&buf), out);
                self.pcm_scratch = buf;
            }
        }
        let mut buf = std::mem::take(&mut self.pcm_scratch);
        buf.clear();
        pcm_encode(&self.acc[a..b], &mut buf);
        self.emit(0, ts, Bytes::copy_from_slice(&buf), out);
        self.pcm_scratch = buf;
        self.last_sent_opus = Some(false);
    }

    fn send_opus(&mut self, a: usize, b: usize, ts: u64, out: &mut Vec<Bytes>) {
        let mut buf = std::mem::take(&mut self.opus_scratch);
        if self.last_sent_opus != Some(true) {
            // Fresh start: encoder and the receiver's decoder both begin from a reset state
            // (otherwise the decoder's cold energy prediction distorts the first frames).
            self.enc.reset();
        }
        match self.enc.encode(&self.acc[a..b], &mut buf) {
            Ok(()) => {
                self.emit(audio_flags::OPUS, ts, Bytes::copy_from_slice(&buf), out);
                self.last_sent_opus = Some(true);
            }
            Err(e) => tracing::warn!("opus encode failed: {e}"),
        }
        self.opus_scratch = buf;
    }
}

/// Microseconds covered by `frames` frames at 48 kHz.
#[must_use]
pub fn frames_to_us(frames: u64) -> u64 {
    frames * 1_000_000 / u64::from(RATE)
}
