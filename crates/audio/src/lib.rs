//! Platform independent audio logic of Desk Bridge: Opus/PCM codecs, packetisation with
//! silence suppression, and the receive side jitter buffer with loss concealment and
//! clock-drift compensation.

pub mod clock;
pub mod error;
pub mod level;
pub mod opus;
pub mod pcm;
pub mod receiver;
pub mod resampler;
pub mod sender;

pub use clock::{Clock, ManualClock, SystemClock};
pub use error::{Error, Result};
pub use level::{LevelMeter, SilenceDetector};
pub use opus::{OpusApp, OpusDecoder, OpusEncoder};
pub use pcm::{pcm_decode, pcm_encode};
pub use receiver::{AudioReceiver, PlayState, ReceiverConfig, ReceiverStats};
pub use sender::{AudioSender, CodecPolicy, SenderConfig};
