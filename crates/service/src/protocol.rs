//! Wire protocol of the two service pipes (portable, no OS calls).
//!
//! * `\\.\pipe\deskbridge-helper`  agent -> secure-desktop helper: input injection
//! * `\\.\pipe\deskbridge-service` agent -> service: `ping`, `send_sas`, `helper_status`
//!
//! Framing: `u16` little-endian payload length, then the payload; `payload[0]` is the tag and the
//! remaining bytes are fixed-size little-endian fields. Injection requests are fire-and-forget (no
//! response, see [`Request::expects_response`]) so the high-rate mouse path never waits for the
//! helper; every other request gets exactly one [`Response`], in order.

use deskbridge_platform::input::Button;

/// Bumped on incompatible changes; reported by `Pong`.
pub const PROTOCOL_VERSION: u16 = 1;
/// Pipe of the SYSTEM helper on the secure desktop.
pub const HELPER_PIPE: &str = r"\\.\pipe\deskbridge-helper";
/// Control pipe of the service.
pub const SERVICE_PIPE: &str = r"\\.\pipe\deskbridge-service";
/// Largest accepted payload; real messages are < 16 bytes (error texts are capped).
pub const MAX_PAYLOAD: usize = 256;
/// Longest error text carried by [`Response::Error`].
pub const MAX_ERROR_TEXT: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Liveness probe (both pipes).
    Ping,
    /// Generate Ctrl+Alt+Del (service pipe only).
    SendSas,
    /// State of the secure-desktop helper (service pipe only).
    HelperStatus,
    /// Key by USB HID usage (helper pipe only from here on).
    Key { hid: u16, down: bool },
    MouseRel { dx: i32, dy: i32 },
    /// Absolute position in physical virtual-desktop pixels.
    MouseAbs { x: i32, y: i32 },
    Button { button: Button, down: bool },
    Wheel { dx: i32, dy: i32 },
    /// Release every key and button the helper has pressed.
    ReleaseAll,
}

impl Request {
    /// Injection commands are fire-and-forget.
    #[must_use]
    pub fn expects_response(&self) -> bool {
        matches!(self, Self::Ping | Self::SendSas | Self::HelperStatus)
    }

    /// True for the commands that inject input (served by the helper only).
    #[must_use]
    pub fn is_injection(&self) -> bool {
        !self.expects_response()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// Request not valid on this pipe / not decodable.
    BadRequest = 1,
    /// Feature not available (e.g. `sas.dll` missing).
    Unsupported = 2,
    /// `SoftwareSASGeneration` policy does not allow services to send Ctrl+Alt+Del.
    SasNotAllowed = 3,
    /// Switched off on purpose (`--no-sas` console mode).
    Disabled = 4,
    /// The call was made but failed.
    Failed = 5,
}

impl ErrorCode {
    fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            1 => Self::BadRequest,
            2 => Self::Unsupported,
            3 => Self::SasNotAllowed,
            4 => Self::Disabled,
            5 => Self::Failed,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Pong {
        version: u16,
        pid: u32,
    },
    Ok,
    Error {
        code: ErrorCode,
        /// Short human readable detail (truncated to [`MAX_ERROR_TEXT`] bytes).
        text: String,
    },
    HelperStatus {
        running: bool,
        pid: u32,
        session: u32,
    },
}

impl Response {
    #[must_use]
    pub fn error(code: ErrorCode, text: impl Into<String>) -> Self {
        Self::Error { code, text: text.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// Length prefix larger than [`MAX_PAYLOAD`] (or zero): the stream is unusable.
    BadLength(usize),
    UnknownTag(u8),
    /// Payload shorter or longer than the tag requires.
    BadPayload(&'static str),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadLength(n) => write!(f, "bad frame length {n}"),
            Self::UnknownTag(t) => write!(f, "unknown tag {t:#x}"),
            Self::BadPayload(w) => write!(f, "bad payload: {w}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

mod tag {
    pub const PING: u8 = 0x01;
    pub const SEND_SAS: u8 = 0x02;
    pub const HELPER_STATUS: u8 = 0x03;
    pub const KEY: u8 = 0x10;
    pub const MOUSE_REL: u8 = 0x11;
    pub const MOUSE_ABS: u8 = 0x12;
    pub const BUTTON: u8 = 0x13;
    pub const WHEEL: u8 = 0x14;
    pub const RELEASE_ALL: u8 = 0x15;

    pub const R_PONG: u8 = 0x80;
    pub const R_OK: u8 = 0x81;
    pub const R_ERROR: u8 = 0x82;
    pub const R_HELPER_STATUS: u8 = 0x83;
}

/// Cursor over a payload.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        if self.0.len() < n {
            return Err(ProtocolError::BadPayload("truncated"));
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, ProtocolError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, ProtocolError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i32(&mut self) -> Result<i32, ProtocolError> {
        Ok(self.u32()? as i32)
    }
    fn bool(&mut self) -> Result<bool, ProtocolError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(ProtocolError::BadPayload("bool")),
        }
    }
    fn finish(self) -> Result<(), ProtocolError> {
        if self.0.is_empty() { Ok(()) } else { Err(ProtocolError::BadPayload("trailing bytes")) }
    }
}

/// Prepends the length prefix to `payload`.
fn framed(payload: &[u8]) -> Vec<u8> {
    debug_assert!(payload.len() <= MAX_PAYLOAD);
    let mut out = Vec::with_capacity(payload.len() + 2);
    out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

impl Request {
    /// Payload without the length prefix.
    #[must_use]
    pub fn encode_payload(&self) -> Vec<u8> {
        let mut p = Vec::with_capacity(10);
        match *self {
            Self::Ping => p.push(tag::PING),
            Self::SendSas => p.push(tag::SEND_SAS),
            Self::HelperStatus => p.push(tag::HELPER_STATUS),
            Self::Key { hid, down } => {
                p.push(tag::KEY);
                p.extend_from_slice(&hid.to_le_bytes());
                p.push(u8::from(down));
            }
            Self::MouseRel { dx, dy } => {
                p.push(tag::MOUSE_REL);
                p.extend_from_slice(&dx.to_le_bytes());
                p.extend_from_slice(&dy.to_le_bytes());
            }
            Self::MouseAbs { x, y } => {
                p.push(tag::MOUSE_ABS);
                p.extend_from_slice(&x.to_le_bytes());
                p.extend_from_slice(&y.to_le_bytes());
            }
            Self::Button { button, down } => {
                p.push(tag::BUTTON);
                p.push(button.to_wire() as u8);
                p.push(u8::from(down));
            }
            Self::Wheel { dx, dy } => {
                p.push(tag::WHEEL);
                p.extend_from_slice(&dx.to_le_bytes());
                p.extend_from_slice(&dy.to_le_bytes());
            }
            Self::ReleaseAll => p.push(tag::RELEASE_ALL),
        }
        p
    }

    /// Complete frame (length prefix + payload).
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        framed(&self.encode_payload())
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProtocolError> {
        let mut r = Reader(payload);
        let t = r.u8()?;
        let req = match t {
            tag::PING => Self::Ping,
            tag::SEND_SAS => Self::SendSas,
            tag::HELPER_STATUS => Self::HelperStatus,
            tag::KEY => Self::Key { hid: r.u16()?, down: r.bool()? },
            tag::MOUSE_REL => Self::MouseRel { dx: r.i32()?, dy: r.i32()? },
            tag::MOUSE_ABS => Self::MouseAbs { x: r.i32()?, y: r.i32()? },
            tag::BUTTON => {
                let b = Button::from_wire(u32::from(r.u8()?))
                    .ok_or(ProtocolError::BadPayload("button"))?;
                Self::Button { button: b, down: r.bool()? }
            }
            tag::WHEEL => Self::Wheel { dx: r.i32()?, dy: r.i32()? },
            tag::RELEASE_ALL => Self::ReleaseAll,
            other => return Err(ProtocolError::UnknownTag(other)),
        };
        r.finish()?;
        Ok(req)
    }
}

impl Response {
    #[must_use]
    pub fn encode_payload(&self) -> Vec<u8> {
        let mut p = Vec::with_capacity(16);
        match self {
            Self::Pong { version, pid } => {
                p.push(tag::R_PONG);
                p.extend_from_slice(&version.to_le_bytes());
                p.extend_from_slice(&pid.to_le_bytes());
            }
            Self::Ok => p.push(tag::R_OK),
            Self::Error { code, text } => {
                p.push(tag::R_ERROR);
                p.extend_from_slice(&(*code as u16).to_le_bytes());
                let t = truncate_utf8(text, MAX_ERROR_TEXT);
                p.push(t.len() as u8);
                p.extend_from_slice(t.as_bytes());
            }
            Self::HelperStatus { running, pid, session } => {
                p.push(tag::R_HELPER_STATUS);
                p.push(u8::from(*running));
                p.extend_from_slice(&pid.to_le_bytes());
                p.extend_from_slice(&session.to_le_bytes());
            }
        }
        p
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        framed(&self.encode_payload())
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProtocolError> {
        let mut r = Reader(payload);
        let t = r.u8()?;
        let resp = match t {
            tag::R_PONG => Self::Pong { version: r.u16()?, pid: r.u32()? },
            tag::R_OK => Self::Ok,
            tag::R_ERROR => {
                let code = ErrorCode::from_u16(r.u16()?).ok_or(ProtocolError::BadPayload("code"))?;
                let n = usize::from(r.u8()?);
                let text = String::from_utf8_lossy(r.take(n)?).into_owned();
                Self::Error { code, text }
            }
            tag::R_HELPER_STATUS => {
                Self::HelperStatus { running: r.bool()?, pid: r.u32()?, session: r.u32()? }
            }
            other => return Err(ProtocolError::UnknownTag(other)),
        };
        r.finish()?;
        Ok(resp)
    }
}

/// Longest prefix of `s` that is at most `max` bytes and ends on a char boundary.
fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Reassembles frames from a byte stream (pipes may deliver partial or coalesced reads).
#[derive(Debug, Default)]
pub struct FrameBuffer {
    buf: Vec<u8>,
}

impl FrameBuffer {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Next complete payload, `Ok(None)` if more bytes are needed. An error means the stream is
    /// corrupt and the connection must be closed.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, ProtocolError> {
        if self.buf.len() < 2 {
            return Ok(None);
        }
        let len = usize::from(u16::from_le_bytes([self.buf[0], self.buf[1]]));
        if len == 0 || len > MAX_PAYLOAD {
            return Err(ProtocolError::BadLength(len));
        }
        if self.buf.len() < 2 + len {
            return Ok(None);
        }
        let payload = self.buf[2..2 + len].to_vec();
        self.buf.drain(..2 + len);
        Ok(Some(payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_requests() -> Vec<Request> {
        vec![
            Request::Ping,
            Request::SendSas,
            Request::HelperStatus,
            Request::Key { hid: 0x04, down: true },
            Request::Key { hid: 0x2E4, down: false },
            Request::MouseRel { dx: -5, dy: 700 },
            Request::MouseAbs { x: -1920, y: 1439 },
            Request::Button { button: Button::X2, down: true },
            Request::Button { button: Button::Left, down: false },
            Request::Wheel { dx: 0, dy: -120 },
            Request::ReleaseAll,
        ]
    }

    #[test]
    fn request_round_trip() {
        for r in all_requests() {
            let frame = r.encode();
            assert_eq!(usize::from(u16::from_le_bytes([frame[0], frame[1]])), frame.len() - 2);
            assert_eq!(Request::decode(&frame[2..]).unwrap(), r, "{r:?}");
        }
    }

    #[test]
    fn response_round_trip() {
        let all = [
            Response::Pong { version: PROTOCOL_VERSION, pid: 4242 },
            Response::Ok,
            Response::error(ErrorCode::SasNotAllowed, "policy"),
            Response::error(ErrorCode::Failed, ""),
            Response::HelperStatus { running: true, pid: 77, session: 2 },
        ];
        for r in all {
            let f = r.encode();
            assert_eq!(Response::decode(&f[2..]).unwrap(), r);
        }
    }

    #[test]
    fn injection_is_fire_and_forget() {
        for r in all_requests() {
            let control = matches!(r, Request::Ping | Request::SendSas | Request::HelperStatus);
            assert_eq!(r.expects_response(), control, "{r:?}");
            assert_eq!(r.is_injection(), !control);
        }
    }

    #[test]
    fn decode_rejects_garbage() {
        assert_eq!(Request::decode(&[]), Err(ProtocolError::BadPayload("truncated")));
        assert_eq!(Request::decode(&[0x7F]), Err(ProtocolError::UnknownTag(0x7F)));
        // truncated key
        assert!(Request::decode(&[tag::KEY, 1]).is_err());
        // trailing bytes
        assert!(Request::decode(&[tag::PING, 0]).is_err());
        // bool out of range, button out of range
        assert!(Request::decode(&[tag::KEY, 1, 0, 2]).is_err());
        assert!(Request::decode(&[tag::BUTTON, 9, 1]).is_err());
        assert!(Response::decode(&[tag::R_ERROR, 99, 0, 0]).is_err());
        // error text longer than the payload
        assert!(Response::decode(&[tag::R_ERROR, 1, 0, 5, b'a']).is_err());
    }

    #[test]
    fn error_text_is_truncated_on_char_boundary() {
        let long = "ä".repeat(200);
        let f = Response::error(ErrorCode::Failed, long).encode();
        match Response::decode(&f[2..]).unwrap() {
            Response::Error { text, .. } => {
                assert!(text.len() <= MAX_ERROR_TEXT);
                assert!(text.chars().all(|c| c == 'ä'));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn frame_buffer_handles_partial_and_coalesced_reads() {
        let mut bytes = Vec::new();
        for r in all_requests() {
            bytes.extend(r.encode());
        }
        let mut fb = FrameBuffer::default();
        let mut got = Vec::new();
        // one byte at a time
        for b in &bytes {
            fb.push(&[*b]);
            while let Some(p) = fb.next_frame().unwrap() {
                got.push(Request::decode(&p).unwrap());
            }
        }
        assert_eq!(got, all_requests());

        // everything at once
        let mut fb = FrameBuffer::default();
        fb.push(&bytes);
        let mut n = 0;
        while fb.next_frame().unwrap().is_some() {
            n += 1;
        }
        assert_eq!(n, all_requests().len());
    }

    #[test]
    fn frame_buffer_rejects_bad_lengths() {
        let mut fb = FrameBuffer::default();
        fb.push(&[0, 0]);
        assert_eq!(fb.next_frame(), Err(ProtocolError::BadLength(0)));
        let mut fb = FrameBuffer::default();
        fb.push(&(u16::try_from(MAX_PAYLOAD + 1).unwrap()).to_le_bytes());
        assert!(matches!(fb.next_frame(), Err(ProtocolError::BadLength(_))));
    }
}
