//! Video pipeline of Desk Bridge focus mode (plan 8.7).
//!
//! * [`negotiate`]: capability intersection (pure logic)
//! * [`abr`]: adaptive bitrate controller (pure logic)
//! * [`pacing`]: keep-alive and IDR throttling (pure logic)
//! * [`stream`]: per-frame uni-stream framing and the receiver-side late-frame filter
//! * `ffmpeg` encoder/decoder and capability probes (Windows hardware paths)

pub mod abr;
pub mod backend;
#[cfg(all(windows, feature = "ffmpeg"))]
pub mod decoder;
#[cfg(all(windows, feature = "ffmpeg"))]
pub mod encoder;
#[cfg(feature = "ffmpeg")]
pub mod ff;
pub mod negotiate;
pub mod pacing;
#[cfg(all(windows, feature = "ffmpeg"))]
pub mod probe;
pub mod stream;
#[cfg(all(windows, feature = "ffmpeg"))]
pub mod windows_backend;

pub use negotiate::{NegotiateError, Negotiated, WantedStream, negotiate};

#[cfg(all(windows, feature = "ffmpeg"))]
pub use decoder::FfmpegDecoder;
#[cfg(all(windows, feature = "ffmpeg"))]
pub use encoder::{EncodeTiming, EncoderOptions, FfmpegEncoder};
#[cfg(all(windows, feature = "ffmpeg"))]
pub use probe::{probe_decoders, probe_encoders};
#[cfg(all(windows, feature = "ffmpeg"))]
pub use windows_backend::WindowsVideoBackend;
