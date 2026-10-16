//! Video transport framing (plan 7.3/7.4): one unidirectional stream per frame,
//! `[VideoHeader (17 bytes)][u32 LE payload length][payload]`, plus the receiver-side filter that
//! drops late frames and decides when to ask for a keyframe.

use std::io;

use deskbridge_platform::video::VideoCodec;
use deskbridge_proto::media::{VIDEO_HEADER_LEN, VideoHeader, video_flags};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest accepted payload (guards against a corrupt or hostile length prefix). An 8K IDR at
/// the highest bitrate is well below this.
pub const MAX_PAYLOAD_LEN: usize = 64 * 1024 * 1024;

/// Writes one frame. Header and length are written together with the payload; the caller is
/// expected to `finish()` the stream afterwards.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    h: &VideoHeader,
    payload: &[u8],
) -> io::Result<()> {
    if payload.len() > MAX_PAYLOAD_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "video payload too large",
        ));
    }
    let mut head = [0u8; VIDEO_HEADER_LEN + 4];
    head[..VIDEO_HEADER_LEN].copy_from_slice(&h.encode());
    head[VIDEO_HEADER_LEN..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    w.write_all(&head).await?;
    if !payload.is_empty() {
        w.write_all(payload).await?;
    }
    Ok(())
}

/// Reads one frame (header + payload). An EOF before the header is `UnexpectedEof`.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<(VideoHeader, Vec<u8>)> {
    let mut head = [0u8; VIDEO_HEADER_LEN + 4];
    r.read_exact(&mut head).await?;
    let h = VideoHeader::decode(&head[..VIDEO_HEADER_LEN])
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let len = u32::from_le_bytes(head[VIDEO_HEADER_LEN..].try_into().expect("4 bytes")) as usize;
    if len > MAX_PAYLOAD_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "video payload length out of range",
        ));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload).await?;
    Ok((h, payload))
}

/// `video_flags` codec bits (4..5): 1=h264 2=hevc 3=av1.
#[must_use]
pub fn codec_flag_bits(c: VideoCodec) -> u8 {
    let v = match c {
        VideoCodec::H264 => 1,
        VideoCodec::Hevc => 2,
        VideoCodec::Av1 => 3,
    };
    v << video_flags::CODEC_SHIFT
}

#[must_use]
pub fn codec_from_flags(flags: u8) -> Option<VideoCodec> {
    match (flags >> video_flags::CODEC_SHIFT) & 0b11 {
        1 => Some(VideoCodec::H264),
        2 => Some(VideoCodec::Hevc),
        3 => Some(VideoCodec::Av1),
        _ => None,
    }
}

/// Builds the header of a real frame.
#[must_use]
pub fn frame_header(
    frame_id: u32,
    key: bool,
    codec: VideoCodec,
    width: u32,
    height: u32,
    ts_us: u64,
) -> VideoHeader {
    let flags = codec_flag_bits(codec) | if key { video_flags::KEY } else { 0 };
    VideoHeader {
        frame_id,
        flags,
        width: width as u16,
        height: height as u16,
        ts_us,
    }
}

/// Builds the header of a keep-alive frame (empty payload; the receiver re-presents the last picture).
#[must_use]
pub fn keepalive_header(
    frame_id: u32,
    codec: VideoCodec,
    width: u32,
    height: u32,
    ts_us: u64,
) -> VideoHeader {
    VideoHeader {
        frame_id,
        flags: codec_flag_bits(codec) | video_flags::KEEPALIVE,
        width: width as u16,
        height: height as u16,
        ts_us,
    }
}

/// Why the filter rejected a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// A newer frame was already accepted (stream arrived out of order).
    Late,
    /// Same frame id seen twice.
    Duplicate,
    /// Older than the configured maximum age.
    Stale,
    /// A predecessor is missing and this is a delta frame: decoding it would corrupt the picture.
    WaitingForKeyframe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Decode and present.
    Accept,
    Drop(DropReason),
}

/// Result of [`LateFrameFilter::on_frame`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilterOutcome {
    pub verdict: Verdict,
    /// The caller should send a `KeyframeRequest` now.
    pub request_keyframe: bool,
}

/// Cumulative statistics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FilterStats {
    pub received: u64,
    pub accepted: u64,
    /// Frame ids that never arrived (gaps).
    pub lost: u64,
    pub dropped_late: u64,
    pub dropped_stale: u64,
    pub dropped_waiting_key: u64,
    pub keyframe_requests: u64,
}

/// Receiver-side filter. Frame ids increase by one per sent frame (keep-alives included).
///
/// Rules:
/// * `id <= newest accepted` -> late / duplicate, dropped silently (presenting it would go back in time).
/// * `id` skips ahead (a frame was lost or is delayed on its own stream): delta frames cannot be
///   decoded correctly any more, so the filter drops everything until the next keyframe and asks
///   for one (re-asking every `retry_ms` while the keyframe has not arrived).
/// * Optional `max_age_us`: frames older than that (sender timestamp vs. local clock, supplied by
///   the caller after clock-offset correction) are dropped like a loss.
#[derive(Debug, Clone)]
pub struct LateFrameFilter {
    newest: Option<u32>,
    waiting_for_key: bool,
    last_request_ms: Option<u64>,
    retry_ms: u64,
    max_age_us: Option<u64>,
    stats: FilterStats,
    interval: (u64, u64), // (received, lost) since the last take_interval
}

impl Default for LateFrameFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl LateFrameFilter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            newest: None,
            waiting_for_key: false,
            last_request_ms: None,
            retry_ms: 250,
            max_age_us: None,
            stats: FilterStats::default(),
            interval: (0, 0),
        }
    }

    #[must_use]
    pub fn with_max_age_us(mut self, max_age_us: u64) -> Self {
        self.max_age_us = Some(max_age_us);
        self
    }

    #[must_use]
    pub fn with_retry_ms(mut self, retry_ms: u64) -> Self {
        self.retry_ms = retry_ms;
        self
    }

    /// Judges the frame whose uni stream just delivered `h`.
    ///
    /// `now_ms`: local monotonic clock. `age_us`: how old the frame is according to the sender
    /// timestamp (`None` if clocks are not synchronised yet).
    pub fn on_frame(&mut self, h: &VideoHeader, now_ms: u64, age_us: Option<u64>) -> FilterOutcome {
        self.stats.received += 1;
        self.interval.0 += 1;
        let key = h.is_key();

        let Some(newest) = self.newest else {
            // First frame of the stream: only a keyframe can start decoding.
            if key {
                self.newest = Some(h.frame_id);
                self.waiting_for_key = false;
                self.last_request_ms = None;
                self.stats.accepted += 1;
                return FilterOutcome {
                    verdict: Verdict::Accept,
                    request_keyframe: false,
                };
            }
            self.waiting_for_key = true;
            return self.drop_waiting(now_ms);
        };

        // Wrapping comparison of u32 ids.
        let delta = h.frame_id.wrapping_sub(newest) as i32;
        if delta == 0 {
            self.stats.dropped_late += 1;
            return FilterOutcome {
                verdict: Verdict::Drop(DropReason::Duplicate),
                request_keyframe: false,
            };
        }
        if delta < 0 {
            self.stats.dropped_late += 1;
            return FilterOutcome {
                verdict: Verdict::Drop(DropReason::Late),
                request_keyframe: false,
            };
        }

        let gap = (delta - 1) as u64;
        if gap > 0 {
            self.stats.lost += gap;
            self.interval.1 += gap;
        }

        let stale = matches!((self.max_age_us, age_us), (Some(max), Some(age)) if age > max);
        if stale && !key {
            // Too old: treat like a loss (the id still advances so the reference chain is known broken).
            self.newest = Some(h.frame_id);
            self.stats.dropped_stale += 1;
            self.interval.1 += 1;
            self.waiting_for_key = true;
            let request = self.should_request(now_ms);
            return FilterOutcome {
                verdict: Verdict::Drop(DropReason::Stale),
                request_keyframe: request,
            };
        }

        self.newest = Some(h.frame_id);

        if key {
            self.waiting_for_key = false;
            self.last_request_ms = None;
            self.stats.accepted += 1;
            return FilterOutcome {
                verdict: Verdict::Accept,
                request_keyframe: false,
            };
        }

        if gap > 0 {
            self.waiting_for_key = true;
        }
        if self.waiting_for_key {
            return self.drop_waiting(now_ms);
        }
        self.stats.accepted += 1;
        FilterOutcome {
            verdict: Verdict::Accept,
            request_keyframe: false,
        }
    }

    /// The decoder failed on an accepted frame: wait for a keyframe and ask for one.
    pub fn on_decode_error(&mut self, now_ms: u64) -> bool {
        self.waiting_for_key = true;
        self.should_request(now_ms)
    }

    /// Call periodically while frames are not arriving to re-send a pending keyframe request.
    pub fn poll_retry(&mut self, now_ms: u64) -> bool {
        self.waiting_for_key && self.should_request(now_ms)
    }

    fn drop_waiting(&mut self, now_ms: u64) -> FilterOutcome {
        self.stats.dropped_waiting_key += 1;
        let request = self.should_request(now_ms);
        FilterOutcome {
            verdict: Verdict::Drop(DropReason::WaitingForKeyframe),
            request_keyframe: request,
        }
    }

    fn should_request(&mut self, now_ms: u64) -> bool {
        let due = self
            .last_request_ms
            .is_none_or(|t| now_ms.saturating_sub(t) >= self.retry_ms);
        if due {
            self.last_request_ms = Some(now_ms);
            self.stats.keyframe_requests += 1;
        }
        due
    }

    #[must_use]
    pub fn is_waiting_for_keyframe(&self) -> bool {
        self.waiting_for_key
    }

    #[must_use]
    pub fn stats(&self) -> FilterStats {
        self.stats
    }

    /// Loss percentage (lost + late-dropped over expected) since the previous call; resets the
    /// interval counters. This is the `loss_pct` of `StreamStats` / the ABR input.
    pub fn take_interval_loss_pct(&mut self) -> f32 {
        let (received, lost) = std::mem::take(&mut self.interval);
        let expected = received + lost;
        if expected == 0 {
            0.0
        } else {
            lost as f32 * 100.0 / expected as f32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    fn hdr(id: u32, key: bool) -> VideoHeader {
        frame_header(id, key, VideoCodec::Hevc, 5120, 1440, u64::from(id) * 8_333)
    }

    #[tokio::test]
    async fn frame_roundtrip_over_duplex() {
        let (mut a, mut b) = duplex(1 << 20);
        let payload: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let h = hdr(42, true);
        let (h2, p2) = (h.clone(), payload.clone());
        let w = tokio::spawn(async move {
            write_frame(&mut a, &h2, &p2).await.unwrap();
            write_frame(
                &mut a,
                &keepalive_header(43, VideoCodec::Hevc, 5120, 1440, 7),
                &[],
            )
            .await
            .unwrap();
        });
        let (rh, rp) = read_frame(&mut b).await.unwrap();
        assert_eq!(rh, h);
        assert_eq!(rp, payload);
        assert!(rh.is_key());
        assert_eq!(codec_from_flags(rh.flags), Some(VideoCodec::Hevc));
        let (kh, kp) = read_frame(&mut b).await.unwrap();
        assert!(kh.is_keepalive());
        assert!(kp.is_empty());
        w.await.unwrap();
        // writer dropped -> clean EOF is reported as UnexpectedEof
        let e = read_frame(&mut b).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn wire_layout_is_header_len_payload() {
        let mut buf = Vec::new();
        let h = hdr(1, false);
        write_frame(&mut buf, &h, b"abc").await.unwrap();
        assert_eq!(buf.len(), 17 + 4 + 3);
        assert_eq!(&buf[..17], &h.encode());
        assert_eq!(&buf[17..21], &3u32.to_le_bytes());
        assert_eq!(&buf[21..], b"abc");
    }

    #[tokio::test]
    async fn rejects_oversize_length_and_truncated_payload() {
        let h = hdr(1, true);
        let mut buf = Vec::new();
        buf.extend_from_slice(&h.encode());
        buf.extend_from_slice(&u32::MAX.to_le_bytes());
        let e = read_frame(&mut buf.as_slice()).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);

        let mut buf = Vec::new();
        buf.extend_from_slice(&h.encode());
        buf.extend_from_slice(&10u32.to_le_bytes());
        buf.extend_from_slice(b"short");
        let e = read_frame(&mut buf.as_slice()).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);

        // header cut in the middle
        let e = read_frame(&mut &h.encode()[..10]).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn write_rejects_huge_payload() {
        let mut sink = tokio::io::sink();
        let big = vec![0u8; MAX_PAYLOAD_LEN + 1];
        let e = write_frame(&mut sink, &hdr(1, true), &big)
            .await
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn codec_flags_roundtrip() {
        for c in [VideoCodec::H264, VideoCodec::Hevc, VideoCodec::Av1] {
            let h = frame_header(1, true, c, 1920, 1080, 0);
            assert_eq!(codec_from_flags(h.flags), Some(c));
            assert!(h.is_key());
            assert!(!h.is_keepalive());
        }
        assert_eq!(codec_from_flags(0), None);
    }

    #[test]
    fn in_order_stream_is_accepted() {
        let mut f = LateFrameFilter::new();
        assert_eq!(f.on_frame(&hdr(0, true), 0, None).verdict, Verdict::Accept);
        for i in 1..100 {
            let o = f.on_frame(&hdr(i, false), u64::from(i) * 8, None);
            assert_eq!(o.verdict, Verdict::Accept);
            assert!(!o.request_keyframe);
        }
        assert_eq!(f.stats().accepted, 100);
        assert_eq!(f.stats().lost, 0);
        assert_eq!(f.take_interval_loss_pct(), 0.0);
    }

    #[test]
    fn first_delta_frame_waits_for_key() {
        let mut f = LateFrameFilter::new();
        let o = f.on_frame(&hdr(5, false), 0, None);
        assert_eq!(o.verdict, Verdict::Drop(DropReason::WaitingForKeyframe));
        assert!(o.request_keyframe);
        let o = f.on_frame(&hdr(6, false), 8, None);
        assert!(!o.request_keyframe, "throttled");
        let o = f.on_frame(&hdr(7, true), 16, None);
        assert_eq!(o.verdict, Verdict::Accept);
        assert!(!f.is_waiting_for_keyframe());
    }

    #[test]
    fn late_and_duplicate_frames_are_dropped_without_keyframe() {
        let mut f = LateFrameFilter::new();
        f.on_frame(&hdr(0, true), 0, None);
        f.on_frame(&hdr(1, false), 8, None);
        f.on_frame(&hdr(2, false), 16, None);
        let o = f.on_frame(&hdr(1, false), 20, None);
        assert_eq!(o.verdict, Verdict::Drop(DropReason::Late));
        assert!(!o.request_keyframe);
        let o = f.on_frame(&hdr(2, false), 21, None);
        assert_eq!(o.verdict, Verdict::Drop(DropReason::Duplicate));
        assert_eq!(f.stats().dropped_late, 2);
    }

    #[test]
    fn gap_requests_keyframe_and_drops_deltas_until_key() {
        let mut f = LateFrameFilter::new();
        f.on_frame(&hdr(0, true), 0, None);
        f.on_frame(&hdr(1, false), 8, None);
        // frame 2 and 3 never arrive
        let o = f.on_frame(&hdr(4, false), 40, None);
        assert_eq!(o.verdict, Verdict::Drop(DropReason::WaitingForKeyframe));
        assert!(o.request_keyframe);
        assert_eq!(f.stats().lost, 2);
        let o = f.on_frame(&hdr(5, false), 48, None);
        assert_eq!(o.verdict, Verdict::Drop(DropReason::WaitingForKeyframe));
        assert!(!o.request_keyframe);
        // retry after 250 ms while still waiting
        let o = f.on_frame(&hdr(6, false), 300, None);
        assert!(o.request_keyframe);
        assert_eq!(f.stats().keyframe_requests, 2);
        // a late straggler (frame 2) must not break anything
        assert_eq!(
            f.on_frame(&hdr(2, false), 301, None).verdict,
            Verdict::Drop(DropReason::Late)
        );
        // keyframe heals
        let o = f.on_frame(&hdr(7, true), 310, None);
        assert_eq!(o.verdict, Verdict::Accept);
        assert_eq!(
            f.on_frame(&hdr(8, false), 318, None).verdict,
            Verdict::Accept
        );
    }

    #[test]
    fn keyframe_after_gap_is_accepted_directly() {
        let mut f = LateFrameFilter::new();
        f.on_frame(&hdr(0, true), 0, None);
        let o = f.on_frame(&hdr(3, true), 10, None);
        assert_eq!(o.verdict, Verdict::Accept);
        assert!(!o.request_keyframe);
        assert_eq!(f.stats().lost, 2);
    }

    #[test]
    fn stale_frames_count_as_loss() {
        let mut f = LateFrameFilter::new().with_max_age_us(50_000);
        f.on_frame(&hdr(0, true), 0, Some(1_000));
        assert_eq!(
            f.on_frame(&hdr(1, false), 8, Some(10_000)).verdict,
            Verdict::Accept
        );
        let o = f.on_frame(&hdr(2, false), 16, Some(120_000));
        assert_eq!(o.verdict, Verdict::Drop(DropReason::Stale));
        assert!(o.request_keyframe);
        // next delta frame is now not decodable
        let o = f.on_frame(&hdr(3, false), 24, Some(1_000));
        assert_eq!(o.verdict, Verdict::Drop(DropReason::WaitingForKeyframe));
        // stale but key: still accepted (cheapest way to heal)
        let o = f.on_frame(&hdr(4, true), 32, Some(500_000));
        assert_eq!(o.verdict, Verdict::Accept);
    }

    #[test]
    fn decode_error_triggers_request_and_wait() {
        let mut f = LateFrameFilter::new();
        f.on_frame(&hdr(0, true), 0, None);
        assert!(f.on_decode_error(100));
        assert!(f.is_waiting_for_keyframe());
        assert!(!f.on_decode_error(150), "throttled");
        assert!(f.poll_retry(400));
        assert_eq!(
            f.on_frame(&hdr(1, false), 410, None).verdict,
            Verdict::Drop(DropReason::WaitingForKeyframe)
        );
        assert_eq!(
            f.on_frame(&hdr(2, true), 420, None).verdict,
            Verdict::Accept
        );
        assert!(!f.poll_retry(1_000));
    }

    #[test]
    fn interval_loss_percentage() {
        let mut f = LateFrameFilter::new();
        f.on_frame(&hdr(0, true), 0, None);
        for i in 1..=9 {
            f.on_frame(&hdr(i, true), 8 * u64::from(i), None);
        }
        // 10 received, then a gap of 10 frames, then 10 more
        for i in 20..30 {
            f.on_frame(&hdr(i, true), 1_000 + u64::from(i), None);
        }
        // received=20, lost=10 -> 33.3 %
        let p = f.take_interval_loss_pct();
        assert!((p - 33.333).abs() < 0.1, "{p}");
        assert_eq!(f.take_interval_loss_pct(), 0.0);
    }

    #[test]
    fn frame_id_wraparound() {
        let mut f = LateFrameFilter::new();
        f.on_frame(&hdr(u32::MAX - 1, true), 0, None);
        assert_eq!(
            f.on_frame(&hdr(u32::MAX, false), 8, None).verdict,
            Verdict::Accept
        );
        assert_eq!(
            f.on_frame(&hdr(0, false), 16, None).verdict,
            Verdict::Accept
        );
        assert_eq!(
            f.on_frame(&hdr(1, false), 24, None).verdict,
            Verdict::Accept
        );
        assert_eq!(f.stats().lost, 0);
        assert_eq!(
            f.on_frame(&hdr(u32::MAX, false), 30, None).verdict,
            Verdict::Drop(DropReason::Late)
        );
    }
}
