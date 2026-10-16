//! Capability probes that feed `Hello.video`: which codecs can really be opened for hardware
//! encode on this machine, and which codecs the D3D11 video device can decode.

use deskbridge_platform::video::{EncoderConfig, VideoCodec};
use deskbridge_platform_windows::video::{adapter_by_index, create_device};
use deskbridge_proto::pb::VideoCaps;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_DECODER_PROFILE_AV1_VLD_PROFILE0, D3D11_DECODER_PROFILE_H264_VLD_NOFGT,
    D3D11_DECODER_PROFILE_HEVC_VLD_MAIN,
    D3D11_VIDEO_DECODER_DESC, ID3D11VideoDevice,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12;
use windows::core::{GUID, Interface};

use crate::encoder::{EncoderOptions, FfmpegEncoder};
use crate::negotiate::codec_to_pb;

const CODECS: [VideoCodec; 3] = [VideoCodec::Hevc, VideoCodec::Av1, VideoCodec::H264];
/// Size ladder for the maximum picture size probes (width, height).
const SIZES: [(u32, u32); 5] = [(8192, 4320), (7680, 4320), (5120, 2880), (4096, 2304), (3840, 2160)];

/// Result of probing one adapter's encoders.
#[derive(Debug, Clone, Default)]
pub struct AdapterEncoders {
    pub adapter_index: u32,
    pub adapter_name: String,
    /// `(codec, ffmpeg encoder name)` for every codec that opened.
    pub encoders: Vec<(VideoCodec, String)>,
    pub max_size: (u32, u32),
}

fn enc_cfg(codec: VideoCodec, w: u32, h: u32) -> EncoderConfig {
    EncoderConfig { codec, width: w, height: h, fps: 60, bitrate_kbps: 20_000, yuv444: false }
}

/// Probes every hardware adapter and returns the per-adapter detail.
#[must_use]
pub fn probe_encoders_detailed() -> Vec<AdapterEncoders> {
    crate::ff::init();
    let mut out = Vec::new();
    for index in 0..8u32 {
        let Ok(adapter) = adapter_by_index(index) else { break };
        // SAFETY: plain COM call.
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else { continue };
        if desc.Flags & 2 != 0 {
            continue; // software adapter
        }
        let Ok(dev) = create_device(Some(&adapter)) else { continue };
        let mut entry = AdapterEncoders {
            adapter_index: index,
            adapter_name: dev.adapter_name.clone(),
            ..Default::default()
        };
        for codec in CODECS {
            let Ok(e) = FfmpegEncoder::with_options(
                enc_cfg(codec, 1280, 720),
                Some(&dev.device),
                EncoderOptions::default(),
            ) else {
                continue;
            };
            let name = deskbridge_platform::video::VideoEncoder::name(&e);
            drop(e);
            // Maximum size: largest ladder entry that still opens.
            let max = SIZES
                .iter()
                .copied()
                .find(|&(w, h)| {
                    FfmpegEncoder::with_options(enc_cfg(codec, w, h), Some(&dev.device), EncoderOptions::default()).is_ok()
                })
                .unwrap_or((1280, 720));
            if max.0 * max.1 > entry.max_size.0 * entry.max_size.1 {
                entry.max_size = max;
            }
            entry.encoders.push((codec, name));
        }
        out.push(entry);
    }
    out
}

/// What this machine can encode in hardware (sent in `Hello.video`).
///
/// 4:4:4 is reported as unsupported: the D3D11 frame path has no 4:4:4 format (see ADR 0005).
#[must_use]
pub fn probe_encoders() -> VideoCaps {
    let detail = probe_encoders_detailed();
    // Use the adapter that supports the most codecs (the dGPU on an Optimus laptop).
    let Some(best) = detail.iter().max_by_key(|a| (a.encoders.len(), a.max_size.0 * a.max_size.1)) else {
        return VideoCaps::default();
    };
    let mut caps = VideoCaps::default();
    for (c, _) in &best.encoders {
        caps.encode.push(codec_to_pb(*c) as i32);
    }
    caps.max_width = best.max_size.0;
    caps.max_height = best.max_size.1;
    caps.max_fps = if best.encoders.is_empty() { 0 } else { 240 };
    caps.encode_yuv444 = false;
    caps.encoder_name = best
        .encoders
        .first()
        .map(|(_, n)| format!("{n} on {}", best.adapter_name))
        .unwrap_or_default();
    caps
}

/// Decode profiles of one adapter.
#[derive(Debug, Clone, Default)]
pub struct AdapterDecoders {
    pub adapter_index: u32,
    pub adapter_name: String,
    pub codecs: Vec<(VideoCodec, (u32, u32))>,
    /// All decoder profile GUIDs the driver reports (diagnostics).
    pub all_profiles: Vec<GUID>,
    /// The driver exposes an HEVC range-extension (4:4:4) profile. FFmpeg's D3D11VA path cannot use it.
    pub hevc_rext_444_profile: bool,
}

fn decoder_profile(c: VideoCodec) -> GUID {
    match c {
        VideoCodec::H264 => D3D11_DECODER_PROFILE_H264_VLD_NOFGT,
        VideoCodec::Hevc => D3D11_DECODER_PROFILE_HEVC_VLD_MAIN,
        VideoCodec::Av1 => D3D11_DECODER_PROFILE_AV1_VLD_PROFILE0,
    }
}

/// HEVC range extensions 4:4:4 (8-bit / 10-bit) as defined in `d3d12video.h`; D3D11 reports the same GUIDs.
const HEVC_MAIN_444: GUID = GUID::from_u128(0x4008018f_f537_4b36_98cf_61af8a2c1a33);
const HEVC_MAIN10_444: GUID = GUID::from_u128(0x9798634d_fe9d_48e5_b4da_dbec45b3df01);

/// Probes the decode capabilities of every adapter (or those whose name contains `adapter_hint`).
#[must_use]
pub fn probe_decoders_detailed(adapter_hint: Option<&str>) -> Vec<AdapterDecoders> {
    let mut out = Vec::new();
    for index in 0..8u32 {
        let Ok(adapter) = adapter_by_index(index) else { break };
        // SAFETY: plain COM call.
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else { continue };
        if desc.Flags & 2 != 0 {
            continue;
        }
        let Ok(dev) = create_device(Some(&adapter)) else { continue };
        if let Some(h) = adapter_hint
            && !dev.adapter_name.to_lowercase().contains(&h.to_lowercase())
        {
            continue;
        }
        let Ok(vdev) = dev.device.cast::<ID3D11VideoDevice>() else { continue };
        let mut entry = AdapterDecoders {
            adapter_index: index,
            adapter_name: dev.adapter_name.clone(),
            ..Default::default()
        };
        // SAFETY: plain COM calls on interfaces we own.
        unsafe {
            for i in 0..vdev.GetVideoDecoderProfileCount() {
                if let Ok(g) = vdev.GetVideoDecoderProfile(i) {
                    entry.all_profiles.push(g);
                }
            }
            entry.hevc_rext_444_profile =
                entry.all_profiles.iter().any(|g| *g == HEVC_MAIN_444 || *g == HEVC_MAIN10_444);
            for codec in CODECS {
                let guid = decoder_profile(codec);
                if !entry.all_profiles.contains(&guid) {
                    continue;
                }
                if !vdev.CheckVideoDecoderFormat(&guid, DXGI_FORMAT_NV12).is_ok_and(|b| b.as_bool()) {
                    continue;
                }
                let max = SIZES.iter().copied().find(|&(w, h)| {
                    let d = D3D11_VIDEO_DECODER_DESC {
                        Guid: guid,
                        SampleWidth: w,
                        SampleHeight: h,
                        OutputFormat: DXGI_FORMAT_NV12,
                    };
                    vdev.GetVideoDecoderConfigCount(&d).is_ok_and(|n| n > 0)
                });
                if let Some(m) = max.or(Some((1920, 1080))) {
                    entry.codecs.push((codec, m));
                }
            }
        }
        out.push(entry);
    }
    out
}

/// What this machine can decode through D3D11VA (sent in `Hello.video`).
///
/// `adapter_hint`: substring of the adapter name that will show the picture (`None` = best adapter).
/// `decode_yuv444` is always false: although some GPUs expose HEVC RExt profiles, FFmpeg's D3D11VA
/// hardware path does not decode 4:4:4, so the pipeline cannot use it (ADR 0005).
#[must_use]
pub fn probe_decoders(adapter_hint: Option<&str>) -> VideoCaps {
    let detail = probe_decoders_detailed(adapter_hint);
    let Some(best) = detail.iter().max_by_key(|a| a.codecs.len()) else {
        return VideoCaps::default();
    };
    let mut caps = VideoCaps::default();
    for (c, _) in &best.codecs {
        caps.decode.push(codec_to_pb(*c) as i32);
    }
    let size = best
        .codecs
        .iter()
        .find(|(c, _)| *c == VideoCodec::Hevc)
        .or_else(|| best.codecs.first())
        .map_or((0, 0), |(_, s)| *s);
    caps.max_width = size.0;
    caps.max_height = size.1;
    caps.max_fps = 0; // not measurable without decoding; the engine verifies with a test decode
    caps.decode_yuv444 = false;
    caps
}
