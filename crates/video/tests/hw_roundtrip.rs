//! Hardware smoke tests (need a GPU with a hardware encoder and the FFmpeg DLLs next to the test
//! executable): `cargo test -p deskbridge-video --features ffmpeg -- --ignored`
#![cfg(all(windows, feature = "ffmpeg"))]

use deskbridge_platform::video::{
    EncoderConfig, Frame, FrameData, VideoCodec, VideoDecoder, VideoEncoder,
};
use deskbridge_video::{FfmpegDecoder, FfmpegEncoder};

fn bgra_frame(w: u32, h: u32, shade: u8, ts: u64) -> Frame {
    let mut data = vec![0u8; (w * h * 4) as usize];
    for (i, px) in data.chunks_exact_mut(4).enumerate() {
        let x = (i as u32 % w) as u8;
        px.copy_from_slice(&[x.wrapping_add(shade), 128, 255 - x, 255]);
    }
    Frame {
        width: w,
        height: h,
        ts_us: ts,
        data: FrameData::Bgra {
            data,
            stride: (w * 4) as usize,
        },
    }
}

#[test]
#[ignore = "needs a GPU with a hardware encoder"]
fn cpu_frames_roundtrip_through_hardware_encoder_and_software_decoder() {
    let cfg = EncoderConfig {
        codec: VideoCodec::Hevc,
        width: 1280,
        height: 720,
        fps: 60,
        bitrate_kbps: 20_000,
        yuv444: false,
    };
    let mut enc = FfmpegEncoder::new(cfg, None).expect("encoder");
    let mut dec = FfmpegDecoder::new_software(VideoCodec::Hevc).expect("decoder");
    let mut decoded = 0;
    for i in 0..10u64 {
        let pk = enc
            .encode(&bgra_frame(1280, 720, i as u8, i * 16_666), i == 0)
            .expect("encode");
        for p in pk {
            assert_eq!(p.key, i == 0, "only the first frame is a keyframe");
            if let Some(f) = dec.decode_with_ts(&p.data, p.key, p.ts_us).expect("decode") {
                assert_eq!((f.width, f.height), (1280, 720));
                decoded += 1;
            }
        }
    }
    assert!(decoded >= 8, "decoded {decoded}");
}

#[test]
#[ignore = "needs a GPU with a hardware encoder"]
fn force_key_produces_an_idr() {
    let cfg = EncoderConfig {
        codec: VideoCodec::Hevc,
        width: 1280,
        height: 720,
        fps: 60,
        bitrate_kbps: 20_000,
        yuv444: false,
    };
    let mut enc = FfmpegEncoder::new(cfg, None).expect("encoder");
    let mut keys = Vec::new();
    for i in 0..6u64 {
        for p in enc
            .encode(
                &bgra_frame(1280, 720, i as u8, i * 16_666),
                i == 0 || i == 4,
            )
            .expect("encode")
        {
            keys.push(p.key);
        }
    }
    assert_eq!(keys, vec![true, false, false, false, true, false]);
}
