//! video-spike: measures the focus-mode video pipeline on this machine (ADR 0005).
//!
//! ```text
//! video-spike [--probe]
//!             [--synthetic | --capture]        picture source (default synthetic)
//!             [--codec hevc|av1|h264]  [--bitrate MBIT] [--fps N] [--width W --height H]
//!             [--seconds S] [--present[=SECS]] [--present-mode window|fullscreen|region] [--verify]
//!             [--preset p1..p7] [--tune ull|ll|hq] [--vbv FRAMES] [--intra-refresh] [--readback]
//!             [--bitrate-step AT_SECS:MBIT] [--adapter N]
//!             [--decode-bench FRAMES [--encoder-name libsvtav1]]
//! ```

mod pipeline;
mod stats;
mod synthetic;
mod verify;

use anyhow::{Result, bail};
use deskbridge_platform::video::VideoCodec;
use deskbridge_video::negotiate::{WantedStream, negotiate};
use deskbridge_video::probe::{probe_decoders_detailed, probe_encoders_detailed};
use deskbridge_video::{EncoderOptions, probe_decoders, probe_encoders};

use pipeline::{PresentModeArg, RunConfig, Source};

fn parse_codec(s: &str) -> Result<VideoCodec> {
    Ok(match s {
        "hevc" | "h265" => VideoCodec::Hevc,
        "av1" => VideoCodec::Av1,
        "h264" | "avc" => VideoCodec::H264,
        other => bail!("unknown codec {other}"),
    })
}

fn probe() {
    println!("== encoder probe (hardware encoders that really open) ==");
    for a in probe_encoders_detailed() {
        println!(
            "adapter #{} {}: {:?}  max size {}x{}",
            a.adapter_index, a.adapter_name, a.encoders, a.max_size.0, a.max_size.1
        );
    }
    println!("== decoder probe (D3D11 video device) ==");
    for a in probe_decoders_detailed(None) {
        println!("adapter #{} {}: {:?}  HEVC RExt 4:4:4 profile exposed: {}", a.adapter_index, a.adapter_name, a.codecs, a.hevc_rext_444_profile);
        println!("  all decoder profiles: {}", a.all_profiles.len());
    }
    let (e, d) = (probe_encoders(), probe_decoders(None));
    println!("VideoCaps encode: {e:?}\nVideoCaps decode: {d:?}");
    match negotiate(&e, &d, &WantedStream::new(5120, 1440, 120)) {
        Ok(n) => println!("negotiate(5120x1440@120) -> {n:?}"),
        Err(err) => println!("negotiate(5120x1440@120) -> {err}"),
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cfg = RunConfig {
        source: Source::Synthetic,
        codec: VideoCodec::Hevc,
        bitrate_kbps: 120_000,
        fps: 120,
        width: 5120,
        height: 1440,
        seconds: 30.0,
        present_secs: 0.0,
        opts: EncoderOptions::default(),
        verify: false,
        adapter: None,
        bitrate_step: None,
        present_mode: PresentModeArg::Window,
    };
    let mut do_probe = false;
    let mut bench: Option<u32> = None;
    let mut i = 0;
    let val = |i: &mut usize| -> Result<String> {
        *i += 1;
        args.get(*i).cloned().ok_or_else(|| anyhow::anyhow!("missing value for {}", args[*i - 1]))
    };
    while i < args.len() {
        let a = args[i].clone();
        match a.as_str() {
            "--probe" => do_probe = true,
            "--nv12-test" => {
                nv12_test()?;
                return Ok(());
            }
            "--synthetic" => cfg.source = Source::Synthetic,
            "--capture" => cfg.source = Source::Capture,
            "--codec" => cfg.codec = parse_codec(&val(&mut i)?)?,
            "--bitrate" => cfg.bitrate_kbps = (val(&mut i)?.parse::<f64>()? * 1000.0) as u32,
            "--fps" => cfg.fps = val(&mut i)?.parse()?,
            "--width" => cfg.width = val(&mut i)?.parse()?,
            "--height" => cfg.height = val(&mut i)?.parse()?,
            "--seconds" => cfg.seconds = val(&mut i)?.parse()?,
            "--present" => cfg.present_secs = 5.0,
            s if s.starts_with("--present=") => cfg.present_secs = s["--present=".len()..].parse()?,
            "--verify" => cfg.verify = true,
            "--present-mode" => {
                cfg.present_mode = match val(&mut i)?.as_str() {
                    "window" => PresentModeArg::Window,
                    "fullscreen" => PresentModeArg::Fullscreen,
                    "region" | "region-appbar" => PresentModeArg::RegionAppbar,
                    o => bail!("unknown present mode {o}"),
                };
            }
            "--preset" => cfg.opts.preset = val(&mut i)?,
            "--tune" => cfg.opts.tune = val(&mut i)?,
            "--vbv" => cfg.opts.vbv_frames = val(&mut i)?.parse()?,
            "--intra-refresh" => cfg.opts.intra_refresh = true,
            "--readback" => cfg.opts.force_readback = true,
            "--adapter" => cfg.adapter = Some(val(&mut i)?.parse()?),
            "--encoder-name" => cfg.opts.encoder_name = Some(val(&mut i)?),
            "--decode-bench" => bench = Some(val(&mut i)?.parse()?),
            "--bitrate-step" => {
                let v = val(&mut i)?;
                let (at, mbit) = v.split_once(':').ok_or_else(|| anyhow::anyhow!("--bitrate-step AT:MBIT"))?;
                cfg.bitrate_step = Some((at.parse()?, (mbit.parse::<f64>()? * 1000.0) as u32));
            }
            "-h" | "--help" => {
                println!("{}", include_str!("main.rs").lines().take(11).collect::<Vec<_>>().join("\n"));
                return Ok(());
            }
            other => bail!("unknown argument {other}"),
        }
        i += 1;
    }
    if cfg.source == Source::Capture && cfg.seconds > 10.0 {
        cfg.seconds = 10.0; // keep real-desktop runs short
    }
    if do_probe {
        probe();
        return Ok(());
    }
    let report = match bench {
        Some(n) => pipeline::decode_bench(&cfg, n)?,
        None => pipeline::run(&cfg)?,
    };
    println!("{}", report.text);
    Ok(())
}

/// Diagnostic: which NV12 texture bind-flag / array-size combinations can this device create?
fn nv12_test() -> Result<()> {
    use deskbridge_platform_windows::video::{adapter_by_index, create_device};
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
    let dev = create_device(Some(&adapter_by_index(0).map_err(|e| anyhow::anyhow!("{e}"))?)).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("device on {}", dev.adapter_name);
    // SAFETY: plain COM calls.
    let fs = unsafe { dev.device.CheckFormatSupport(DXGI_FORMAT_NV12)? };
    println!("NV12 format support bits: {fs:#x}");
    for (name, bind) in [
        ("none", 0u32),
        ("SRV", D3D11_BIND_SHADER_RESOURCE.0 as u32),
        ("RTV", D3D11_BIND_RENDER_TARGET.0 as u32),
        ("RTV|SRV", (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32),
        ("DECODER", D3D11_BIND_DECODER.0 as u32),
        ("DECODER|SRV", (D3D11_BIND_DECODER.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32),
    ] {
        for arr in [1u32, 8] {
            let d = D3D11_TEXTURE2D_DESC {
                Width: 1280, Height: 720, MipLevels: 1, ArraySize: arr, Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_DEFAULT,
                BindFlags: bind, CPUAccessFlags: 0, MiscFlags: 0,
            };
            let mut t = None;
            let r = unsafe { dev.device.CreateTexture2D(&d, None, Some(&mut t)) };
            println!("bind {name:<12} array {arr}: {}", match r { Ok(()) => "ok".to_string(), Err(e) => e.to_string() });
        }
    }
    Ok(())
}
