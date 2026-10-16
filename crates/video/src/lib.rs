//! Video pipeline of Desk Bridge focus mode (plan 8.7).
//!
//! * [`negotiate`]: capability intersection (pure logic)
//! * [`abr`]: adaptive bitrate controller (pure logic)
//! * [`pacing`]: keep-alive and IDR throttling (pure logic)
//! * [`stream`]: per-frame uni-stream framing and the receiver-side late-frame filter
//! * `ffmpeg` encoder/decoder and capability probes (Windows hardware paths)

pub mod abr;
pub mod backend;
pub mod negotiate;
pub mod pacing;
pub mod stream;

pub use negotiate::{NegotiateError, Negotiated, WantedStream, negotiate};
