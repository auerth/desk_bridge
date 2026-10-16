//! Pure capability negotiation (plan 8.7): intersect the encoder side (stream source) with the
//! decoder side (the device that shows the picture) and clamp the wanted stream to what both can do.

use deskbridge_platform::video::VideoCodec;
use deskbridge_proto::pb::{Codec, VideoCaps};

/// Default codec preference when the caller does not force one. HEVC first (best hardware coverage
/// on both ends, 4:4:4 possible), then AV1 (better quality per bit but only RTX 40/50, RDNA3+, Arc),
/// then H.264 as the universal fallback. ADR 0005 confirms the order for the owner's setup.
pub const DEFAULT_CODEC_ORDER: [VideoCodec; 3] = [VideoCodec::Hevc, VideoCodec::Av1, VideoCodec::H264];

/// Hardware H.264 encoders and decoders support at most 4096 pixels per side (measured: NVENC
/// rejects 5120 wide; ADR 0005).
pub const H264_MAX_SIDE: u32 = 4096;

/// What the user / engine would like to stream.
#[derive(Debug, Clone, PartialEq)]
pub struct WantedStream {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// A codec to try first (setting "prefer AV1"); the default order is used for the rest.
    pub prefer: Option<VideoCodec>,
    /// Ask for 4:4:4 (sharp text). Granted only if both ends support it, and only for HEVC.
    pub want_yuv444: bool,
    /// Start bitrate in kbit/s; `None` = pick a default from size and fps.
    pub bitrate_kbps: Option<u32>,
}

impl WantedStream {
    #[must_use]
    pub fn new(width: u32, height: u32, fps: u32) -> Self {
        Self { width, height, fps, prefer: None, want_yuv444: false, bitrate_kbps: None }
    }
}

/// Result of a successful negotiation; goes into `FocusStart` / `FocusReady`.
#[derive(Debug, Clone, PartialEq)]
pub struct Negotiated {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub yuv444: bool,
    pub bitrate_kbps: u32,
    /// Human-readable explanations of every deviation from the request (for logs / the UI).
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NegotiateError {
    #[error("requested stream has zero size or zero fps")]
    InvalidRequest,
    #[error("the capturing device has no usable hardware video encoder")]
    NoEncoder,
    #[error("the viewing device cannot decode any video codec")]
    NoDecoder,
    #[error("no codec is supported by both encoder and decoder")]
    NoCommonCodec,
}

/// Converts the proto enum to the platform enum.
#[must_use]
pub fn codec_from_pb(c: i32) -> Option<VideoCodec> {
    match Codec::try_from(c).ok()? {
        Codec::H264 => Some(VideoCodec::H264),
        Codec::Hevc => Some(VideoCodec::Hevc),
        Codec::Av1 => Some(VideoCodec::Av1),
        Codec::Unspecified => None,
    }
}

#[must_use]
pub fn codec_to_pb(c: VideoCodec) -> Codec {
    match c {
        VideoCodec::H264 => Codec::H264,
        VideoCodec::Hevc => Codec::Hevc,
        VideoCodec::Av1 => Codec::Av1,
    }
}

fn supports(list: &[i32], c: VideoCodec) -> bool {
    list.iter().any(|&v| codec_from_pb(v) == Some(c))
}

/// Default start bitrate: 120 Mbit/s for 5120x1440@120 HEVC, scaled with pixel rate (plan 8.7),
/// AV1 needs ~30 % less for the same quality. Clamped to the wired ABR range.
#[must_use]
pub fn default_bitrate_kbps(codec: VideoCodec, width: u32, height: u32, fps: u32) -> u32 {
    const REF_RATE: f64 = 5120.0 * 1440.0 * 120.0;
    let rate = f64::from(width) * f64::from(height) * f64::from(fps);
    let mut kbps = 120_000.0 * (rate / REF_RATE);
    kbps *= match codec {
        VideoCodec::Av1 => 0.7,
        VideoCodec::Hevc => 1.0,
        VideoCodec::H264 => 1.5,
    };
    (kbps as u32).clamp(8_000, 300_000)
}

/// Negotiates codec, size, fps and chroma format.
///
/// `limits` of 0 mean "unknown / unlimited" (the proto default).
pub fn negotiate(
    encoder_side: &VideoCaps,
    decoder_side: &VideoCaps,
    want: &WantedStream,
) -> Result<Negotiated, NegotiateError> {
    if want.width == 0 || want.height == 0 || want.fps == 0 {
        return Err(NegotiateError::InvalidRequest);
    }
    if encoder_side.encode.is_empty() {
        return Err(NegotiateError::NoEncoder);
    }
    if decoder_side.decode.is_empty() {
        return Err(NegotiateError::NoDecoder);
    }
    let mut notes = Vec::new();

    // Codec: preferred one first, then the default order.
    let mut order: Vec<VideoCodec> = Vec::with_capacity(4);
    if let Some(p) = want.prefer {
        order.push(p);
    }
    for c in DEFAULT_CODEC_ORDER {
        if !order.contains(&c) {
            order.push(c);
        }
    }
    let codec = order
        .iter()
        .copied()
        .find(|&c| supports(&encoder_side.encode, c) && supports(&decoder_side.decode, c))
        .ok_or(NegotiateError::NoCommonCodec)?;
    if let Some(p) = want.prefer.filter(|&p| p != codec) {
        notes.push(format!("preferred codec {p:?} not available on both ends, using {codec:?}"));
    }

    // 4:4:4 only for HEVC and only if both ends advertise it.
    let both_444 = encoder_side.encode_yuv444 && decoder_side.decode_yuv444;
    let yuv444 = want.want_yuv444 && both_444 && codec == VideoCodec::Hevc;
    if want.want_yuv444 && !yuv444 {
        let why = if codec != VideoCodec::Hevc {
            format!("4:4:4 is only used with HEVC (negotiated {codec:?})")
        } else if !encoder_side.encode_yuv444 {
            "the encoder cannot produce 4:4:4".to_owned()
        } else {
            "the decoder cannot decode 4:4:4".to_owned()
        };
        notes.push(format!("4:4:4 requested but not granted: {why}; using 4:2:0"));
    }

    // Size: both ends' maxima (0 = unlimited). Keep the aspect ratio, round down to even numbers.
    let mut max_w = min_nonzero(encoder_side.max_width, decoder_side.max_width);
    let mut max_h = min_nonzero(encoder_side.max_height, decoder_side.max_height);
    if codec == VideoCodec::H264 {
        // Hardware H.264 (NVENC, D3D11VA) is limited to 4096 pixels per side regardless of what
        // the caps (which hold one maximum for all codecs) say.
        max_w = Some(max_w.map_or(H264_MAX_SIDE, |m| m.min(H264_MAX_SIDE)));
        max_h = Some(max_h.map_or(H264_MAX_SIDE, |m| m.min(H264_MAX_SIDE)));
    }
    let (mut width, mut height) = (want.width, want.height);
    let mut scale = 1.0_f64;
    if let Some(mw) = max_w.filter(|&mw| width > mw) {
        scale = scale.min(f64::from(mw) / f64::from(width));
    }
    if let Some(mh) = max_h.filter(|&mh| height > mh) {
        scale = scale.min(f64::from(mh) / f64::from(height));
    }
    if scale < 1.0 {
        width = even_floor((f64::from(width) * scale) as u32);
        height = even_floor((f64::from(height) * scale) as u32);
        notes.push(format!(
            "size reduced from {}x{} to {width}x{height} (limits {}x{})",
            want.width,
            want.height,
            max_w.map_or_else(|| "-".into(), |v| v.to_string()),
            max_h.map_or_else(|| "-".into(), |v| v.to_string()),
        ));
    } else if width % 2 != 0 || height % 2 != 0 {
        width = even_floor(width);
        height = even_floor(height);
        notes.push(format!("size rounded to even {width}x{height}"));
    }
    if width == 0 || height == 0 {
        return Err(NegotiateError::InvalidRequest);
    }

    // fps
    let mut fps = want.fps;
    if let Some(mf) = min_nonzero(encoder_side.max_fps, decoder_side.max_fps).filter(|&mf| fps > mf)
    {
        notes.push(format!("fps reduced from {fps} to {mf} (encoder/decoder limit)"));
        fps = mf;
    }

    let bitrate_kbps = want
        .bitrate_kbps
        .unwrap_or_else(|| default_bitrate_kbps(codec, width, height, fps))
        .clamp(1_000, 300_000);

    Ok(Negotiated { codec, width, height, fps, yuv444, bitrate_kbps, notes })
}

fn min_nonzero(a: u32, b: u32) -> Option<u32> {
    match (a, b) {
        (0, 0) => None,
        (0, x) | (x, 0) => Some(x),
        (x, y) => Some(x.min(y)),
    }
}

const fn even_floor(v: u32) -> u32 {
    v & !1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(enc: &[Codec], dec: &[Codec]) -> VideoCaps {
        VideoCaps {
            encode: enc.iter().map(|c| *c as i32).collect(),
            decode: dec.iter().map(|c| *c as i32).collect(),
            ..Default::default()
        }
    }

    fn enc_side(codecs: &[Codec]) -> VideoCaps {
        VideoCaps { max_width: 8192, max_height: 8192, max_fps: 240, ..caps(codecs, &[]) }
    }

    fn dec_side(codecs: &[Codec]) -> VideoCaps {
        VideoCaps { max_width: 8192, max_height: 8192, max_fps: 240, ..caps(&[], codecs) }
    }

    const ALL: [Codec; 3] = [Codec::H264, Codec::Hevc, Codec::Av1];

    #[test]
    fn prefers_hevc_by_default() {
        let n = negotiate(&enc_side(&ALL), &dec_side(&ALL), &WantedStream::new(5120, 1440, 120))
            .unwrap();
        assert_eq!(n.codec, VideoCodec::Hevc);
        assert_eq!((n.width, n.height, n.fps), (5120, 1440, 120));
        assert_eq!(n.bitrate_kbps, 120_000);
        assert!(n.notes.is_empty(), "{:?}", n.notes);
        assert!(!n.yuv444);
    }

    #[test]
    fn prefer_av1_is_honoured_when_possible() {
        let mut w = WantedStream::new(2560, 1440, 60);
        w.prefer = Some(VideoCodec::Av1);
        let n = negotiate(&enc_side(&ALL), &dec_side(&ALL), &w).unwrap();
        assert_eq!(n.codec, VideoCodec::Av1);
        // AV1 starts ~30 % lower than HEVC for the same pixel rate
        let hevc = default_bitrate_kbps(VideoCodec::Hevc, 2560, 1440, 60);
        assert!(n.bitrate_kbps < hevc);
    }

    #[test]
    fn prefer_av1_falls_back_with_note() {
        let mut w = WantedStream::new(1920, 1080, 60);
        w.prefer = Some(VideoCodec::Av1);
        let n = negotiate(
            &enc_side(&[Codec::H264, Codec::Hevc, Codec::Av1]),
            &dec_side(&[Codec::H264, Codec::Hevc]),
            &w,
        )
        .unwrap();
        assert_eq!(n.codec, VideoCodec::Hevc);
        assert!(n.notes.iter().any(|s| s.contains("Av1")));
    }

    #[test]
    fn falls_back_to_h264() {
        let n = negotiate(
            &enc_side(&[Codec::H264]),
            &dec_side(&[Codec::H264, Codec::Hevc]),
            &WantedStream::new(1920, 1080, 60),
        )
        .unwrap();
        assert_eq!(n.codec, VideoCodec::H264);
    }

    #[test]
    fn errors() {
        let w = WantedStream::new(1920, 1080, 60);
        assert_eq!(
            negotiate(&enc_side(&[]), &dec_side(&ALL), &w).unwrap_err(),
            NegotiateError::NoEncoder
        );
        assert_eq!(
            negotiate(&enc_side(&ALL), &dec_side(&[]), &w).unwrap_err(),
            NegotiateError::NoDecoder
        );
        assert_eq!(
            negotiate(&enc_side(&[Codec::Av1]), &dec_side(&[Codec::H264]), &w).unwrap_err(),
            NegotiateError::NoCommonCodec
        );
        assert_eq!(
            negotiate(&enc_side(&ALL), &dec_side(&ALL), &WantedStream::new(0, 1080, 60))
                .unwrap_err(),
            NegotiateError::InvalidRequest
        );
        assert_eq!(
            negotiate(&enc_side(&ALL), &dec_side(&ALL), &WantedStream::new(100, 100, 0))
                .unwrap_err(),
            NegotiateError::InvalidRequest
        );
    }

    #[test]
    fn yuv444_needs_both_ends_and_hevc() {
        let mut w = WantedStream::new(2560, 1440, 60);
        w.want_yuv444 = true;
        let mut e = enc_side(&ALL);
        let mut d = dec_side(&ALL);
        // neither
        let n = negotiate(&e, &d, &w).unwrap();
        assert!(!n.yuv444);
        assert!(n.notes.iter().any(|s| s.contains("encoder cannot")));
        // only encoder
        e.encode_yuv444 = true;
        let n = negotiate(&e, &d, &w).unwrap();
        assert!(!n.yuv444);
        assert!(n.notes.iter().any(|s| s.contains("decoder cannot")));
        // both
        d.decode_yuv444 = true;
        let n = negotiate(&e, &d, &w).unwrap();
        assert!(n.yuv444);
        // both, but codec is not HEVC
        let d2 = VideoCaps { decode: vec![Codec::H264 as i32], ..d.clone() };
        let e2 = VideoCaps { encode: vec![Codec::H264 as i32], ..e.clone() };
        let n = negotiate(&e2, &d2, &w).unwrap();
        assert_eq!(n.codec, VideoCodec::H264);
        assert!(!n.yuv444);
        assert!(n.notes.iter().any(|s| s.contains("only used with HEVC")));
    }

    #[test]
    fn clamps_size_keeping_aspect_ratio() {
        let e = enc_side(&ALL);
        let d = VideoCaps { max_width: 4096, max_height: 4096, ..dec_side(&ALL) };
        let n = negotiate(&e, &d, &WantedStream::new(5120, 1440, 120)).unwrap();
        assert_eq!(n.width, 4096);
        assert_eq!(n.height, 1152); // 1440 * 0.8
        assert!(n.notes.iter().any(|s| s.contains("size reduced")));
    }

    #[test]
    fn h264_is_limited_to_4096_per_side() {
        let e = enc_side(&[Codec::H264]);
        let d = dec_side(&[Codec::H264]);
        let n = negotiate(&e, &d, &WantedStream::new(5120, 1440, 60)).unwrap();
        assert_eq!(n.codec, VideoCodec::H264);
        assert_eq!(n.width, 4096);
        assert_eq!(n.height, 1152);
        // HEVC available: no clamp
        let n = negotiate(&enc_side(&ALL), &dec_side(&ALL), &WantedStream::new(5120, 1440, 60)).unwrap();
        assert_eq!(n.width, 5120);
    }

    #[test]
    fn clamps_fps_and_zero_means_unlimited() {
        let e = VideoCaps { max_fps: 60, ..enc_side(&ALL) };
        let d = dec_side(&ALL);
        let n = negotiate(&e, &d, &WantedStream::new(2560, 1440, 120)).unwrap();
        assert_eq!(n.fps, 60);
        assert!(n.notes.iter().any(|s| s.contains("fps reduced")));

        // limits unknown (0) on both sides: nothing is clamped
        let e = VideoCaps { max_width: 0, max_height: 0, max_fps: 0, ..enc_side(&ALL) };
        let d = VideoCaps { max_width: 0, max_height: 0, max_fps: 0, ..dec_side(&ALL) };
        let n = negotiate(&e, &d, &WantedStream::new(7680, 2160, 144)).unwrap();
        assert_eq!((n.width, n.height, n.fps), (7680, 2160, 144));
    }

    #[test]
    fn odd_sizes_are_made_even() {
        let n = negotiate(&enc_side(&ALL), &dec_side(&ALL), &WantedStream::new(3413, 1440, 120))
            .unwrap();
        assert_eq!((n.width, n.height), (3412, 1440));
        assert!(n.notes.iter().any(|s| s.contains("even")));
    }

    #[test]
    fn explicit_bitrate_is_clamped() {
        let mut w = WantedStream::new(1920, 1080, 60);
        w.bitrate_kbps = Some(900_000);
        let n = negotiate(&enc_side(&ALL), &dec_side(&ALL), &w).unwrap();
        assert_eq!(n.bitrate_kbps, 300_000);
        w.bitrate_kbps = Some(5);
        let n = negotiate(&enc_side(&ALL), &dec_side(&ALL), &w).unwrap();
        assert_eq!(n.bitrate_kbps, 1_000);
    }

    #[test]
    fn default_bitrate_scales_with_pixel_rate() {
        let full = default_bitrate_kbps(VideoCodec::Hevc, 5120, 1440, 120);
        let half = default_bitrate_kbps(VideoCodec::Hevc, 2560, 1440, 120);
        let sixty = default_bitrate_kbps(VideoCodec::Hevc, 5120, 1440, 60);
        assert_eq!(full, 120_000);
        assert_eq!(half, 60_000);
        assert_eq!(sixty, 60_000);
        assert_eq!(default_bitrate_kbps(VideoCodec::Hevc, 320, 240, 30), 8_000);
    }

    #[test]
    fn codec_conversion_roundtrip() {
        for c in [VideoCodec::H264, VideoCodec::Hevc, VideoCodec::Av1] {
            assert_eq!(codec_from_pb(codec_to_pb(c) as i32), Some(c));
        }
        assert_eq!(codec_from_pb(0), None);
        assert_eq!(codec_from_pb(99), None);
    }
}
