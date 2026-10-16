//! Desk Bridge wire protocol.
//!
//! * [`pb`]: generated protobuf messages (control, input, clipboard, pairing ...)
//! * [`frame`]: length-delimited framing for QUIC streams
//! * [`media`]: compact binary headers for audio datagrams and video frames

pub mod frame;
pub mod media;

#[allow(clippy::all, clippy::pedantic, missing_docs)]
pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/deskbridge.v1.rs"));
}

/// Major protocol version. Peers with a different major version refuse to talk (UI shows
/// "Bitte beide Geräte aktualisieren").
pub const PROTO_MAJOR: u32 = 1;
/// Minor protocol version, informational; capabilities decide what is actually used.
pub const PROTO_MINOR: u32 = 0;
/// `proto_version` field value in [`pb::Hello`]: `major << 16 | minor`.
pub const PROTO_VERSION: u32 = (PROTO_MAJOR << 16) | PROTO_MINOR;

/// ALPN of the normal, mutually pinned session connection.
pub const ALPN_SESSION: &[u8] = b"deskbridge/1";
/// ALPN of the pairing connection (certificates are not yet trusted).
pub const ALPN_PAIR: &[u8] = b"deskbridge-pair/1";
/// Default UDP port (QUIC) and mDNS service name.
pub const DEFAULT_PORT: u16 = 47820;
/// DNS-SD service type.
pub const MDNS_SERVICE: &str = "_deskbridge._udp.local.";

/// Extracts the major part of a `proto_version` value.
#[must_use]
pub const fn major_of(version: u32) -> u32 {
    version >> 16
}

/// Returns true if two protocol versions can talk to each other.
#[must_use]
pub const fn compatible(a: u32, b: u32) -> bool {
    major_of(a) == major_of(b)
}

/// Well-known capability names exchanged in [`pb::Hello`].
pub mod caps {
    pub const INPUT: &str = "input";
    pub const CLIPBOARD_TEXT: &str = "clipboard.text";
    pub const CLIPBOARD_RICH: &str = "clipboard.rich";
    pub const FILES: &str = "files";
    pub const FILES_VIRTUAL: &str = "files.virtual";
    pub const AUDIO_PCM: &str = "audio.pcm";
    pub const AUDIO_OPUS: &str = "audio.opus";
    pub const AUDIO_SPEAKER_SOURCE: &str = "audio.speaker-source";
    pub const AUDIO_MIC_SINK: &str = "audio.mic-sink";
    pub const AUDIO_HEADSET: &str = "audio.headset";
    pub const FOCUS_SOURCE: &str = "focus.source";
    pub const FOCUS_VIEWER: &str = "focus.viewer";
    pub const UPDATE: &str = "update";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compat() {
        assert!(compatible(PROTO_VERSION, PROTO_VERSION + 5));
        assert!(!compatible(PROTO_VERSION, 2 << 16));
    }
}
