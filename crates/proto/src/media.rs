//! Compact little-endian binary headers for media packets.
//!
//! Audio datagram: `stream_id u8 | flags u8 | seq u16 | ts_us u64 | payload`
//! Video frame (at the start of a uni stream): audio header fields are replaced by
//! `frame_id u32 | flags u8 | width u16 | height u16 | ts_us u64 | payload`.

use bytes::{Buf, BufMut, Bytes, BytesMut};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MediaError {
    #[error("packet too short")]
    TooShort,
}

pub const AUDIO_HEADER_LEN: usize = 12;
pub const VIDEO_HEADER_LEN: usize = 17;

pub mod audio_flags {
    /// Payload is Opus (otherwise raw little-endian i16 PCM).
    pub const OPUS: u8 = 1;
    /// Keep-alive for digital silence (empty payload).
    pub const SILENCE: u8 = 2;
    /// Mono instead of stereo.
    pub const MONO: u8 = 4;
}

pub mod video_flags {
    pub const KEY: u8 = 1;
    /// Keep-alive frame (nothing changed); decoder shows the previous picture.
    pub const KEEPALIVE: u8 = 2;
    pub const CODEC_SHIFT: u8 = 4; // bits 4..5: 1=h264 2=hevc 3=av1
}

pub const STREAM_AUDIO_SPEAKER: u8 = 1; // target -> headset device
pub const STREAM_AUDIO_MIC: u8 = 2; // headset mic -> target

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPacket {
    pub stream_id: u8,
    pub flags: u8,
    pub seq: u16,
    pub ts_us: u64,
    pub payload: Bytes,
}

impl AudioPacket {
    #[must_use]
    pub fn encode(&self) -> Bytes {
        let mut b = BytesMut::with_capacity(AUDIO_HEADER_LEN + self.payload.len());
        b.put_u8(self.stream_id);
        b.put_u8(self.flags);
        b.put_u16_le(self.seq);
        b.put_u64_le(self.ts_us);
        b.extend_from_slice(&self.payload);
        b.freeze()
    }

    pub fn decode(mut data: Bytes) -> Result<Self, MediaError> {
        if data.len() < AUDIO_HEADER_LEN {
            return Err(MediaError::TooShort);
        }
        let stream_id = data.get_u8();
        let flags = data.get_u8();
        let seq = data.get_u16_le();
        let ts_us = data.get_u64_le();
        Ok(Self {
            stream_id,
            flags,
            seq,
            ts_us,
            payload: data,
        })
    }

    #[must_use]
    pub fn is_opus(&self) -> bool {
        self.flags & audio_flags::OPUS != 0
    }

    #[must_use]
    pub fn is_silence(&self) -> bool {
        self.flags & audio_flags::SILENCE != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoHeader {
    pub frame_id: u32,
    pub flags: u8,
    pub width: u16,
    pub height: u16,
    pub ts_us: u64,
}

impl VideoHeader {
    #[must_use]
    pub fn is_key(&self) -> bool {
        self.flags & video_flags::KEY != 0
    }

    #[must_use]
    pub fn is_keepalive(&self) -> bool {
        self.flags & video_flags::KEEPALIVE != 0
    }

    #[must_use]
    pub fn encode(&self) -> [u8; VIDEO_HEADER_LEN] {
        let mut out = [0u8; VIDEO_HEADER_LEN];
        out[0..4].copy_from_slice(&self.frame_id.to_le_bytes());
        out[4] = self.flags;
        out[5..7].copy_from_slice(&self.width.to_le_bytes());
        out[7..9].copy_from_slice(&self.height.to_le_bytes());
        out[9..17].copy_from_slice(&self.ts_us.to_le_bytes());
        out
    }

    pub fn decode(data: &[u8]) -> Result<Self, MediaError> {
        if data.len() < VIDEO_HEADER_LEN {
            return Err(MediaError::TooShort);
        }
        Ok(Self {
            frame_id: u32::from_le_bytes(data[0..4].try_into().unwrap()),
            flags: data[4],
            width: u16::from_le_bytes(data[5..7].try_into().unwrap()),
            height: u16::from_le_bytes(data[7..9].try_into().unwrap()),
            ts_us: u64::from_le_bytes(data[9..17].try_into().unwrap()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_roundtrip() {
        let p = AudioPacket {
            stream_id: STREAM_AUDIO_SPEAKER,
            flags: audio_flags::OPUS,
            seq: 65535,
            ts_us: 123_456_789,
            payload: Bytes::from_static(b"abc"),
        };
        let enc = p.encode();
        assert_eq!(enc.len(), AUDIO_HEADER_LEN + 3);
        assert_eq!(AudioPacket::decode(enc).unwrap(), p);
        assert_eq!(
            AudioPacket::decode(Bytes::from_static(b"x")),
            Err(MediaError::TooShort)
        );
    }

    #[test]
    fn video_roundtrip() {
        let h = VideoHeader {
            frame_id: 9,
            flags: video_flags::KEY,
            width: 5120,
            height: 1440,
            ts_us: 77,
        };
        assert_eq!(VideoHeader::decode(&h.encode()).unwrap(), h);
        assert!(h.is_key());
    }
}
