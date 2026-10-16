//! Length-delimited protobuf framing: `u32 LE length | payload`.

use bytes::{Buf, BufMut, BytesMut};
use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Upper bound for a single control/input frame (clipboard offers with inline text and cursor
/// shapes stay far below this; bulk data uses raw streams).
pub const MAX_FRAME: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame too large: {0} bytes")]
    TooLarge(usize),
    #[error("protobuf decode: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("stream closed")]
    Closed,
}

/// Encodes a message into a framed buffer.
///
/// # Panics
/// Never for messages below [`MAX_FRAME`]; larger messages are a programming error.
#[must_use]
pub fn encode<M: Message>(msg: &M) -> Vec<u8> {
    let len = msg.encoded_len();
    assert!(len <= MAX_FRAME, "frame too large");
    let mut buf = Vec::with_capacity(4 + len);
    buf.put_u32_le(len as u32);
    msg.encode(&mut buf).expect("vec has capacity");
    buf
}

/// Writes one framed message.
pub async fn write_msg<W, M>(w: &mut W, msg: &M) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    M: Message,
{
    let buf = encode(msg);
    w.write_all(&buf).await?;
    Ok(())
}

/// Reads one framed message. Returns [`FrameError::Closed`] on a clean EOF at a frame boundary.
pub async fn read_msg<R, M>(r: &mut R) -> Result<M, FrameError>
where
    R: AsyncRead + Unpin,
    M: Message + Default,
{
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(FrameError::Closed),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload).await?;
    Ok(M::decode(payload.as_slice())?)
}

/// Incremental decoder for byte buffers (used in tests and by non-async callers).
#[derive(Default)]
pub struct FrameDecoder {
    buf: BytesMut,
}

impl FrameDecoder {
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Pops the next complete message, if any.
    pub fn try_next<M: Message + Default>(&mut self) -> Result<Option<M>, FrameError> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if len > MAX_FRAME {
            return Err(FrameError::TooLarge(len));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        self.buf.advance(4);
        let payload = self.buf.split_to(len);
        Ok(Some(M::decode(payload.as_ref())?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb;

    #[test]
    fn roundtrip_via_decoder() {
        let m = pb::Control {
            msg: Some(pb::control::Msg::Ping(pb::Ping { t0_us: 42 })),
        };
        let bytes = encode(&m);
        let mut d = FrameDecoder::default();
        d.push(&bytes[..3]);
        assert!(d.try_next::<pb::Control>().unwrap().is_none());
        d.push(&bytes[3..]);
        assert_eq!(d.try_next::<pb::Control>().unwrap().unwrap(), m);
        assert!(d.try_next::<pb::Control>().unwrap().is_none());
    }

    #[tokio::test]
    async fn async_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let m = pb::InputEvent {
            ts_us: 7,
            ev: Some(pb::input_event::Ev::Key(pb::Key { hid: 4, down: true })),
        };
        write_msg(&mut a, &m).await.unwrap();
        let got: pb::InputEvent = read_msg(&mut b).await.unwrap();
        assert_eq!(got, m);
        drop(a);
        assert!(matches!(read_msg::<_, pb::InputEvent>(&mut b).await, Err(FrameError::Closed)));
    }

    #[test]
    fn old_decoder_ignores_unknown_fields() {
        // A newer peer adds field 99 to Ping; the older decoder must still parse it.
        let mut raw = pb::Ping { t0_us: 5 }.encode_to_vec();
        raw.extend_from_slice(&[0x98, 0x06, 0x01]); // field 99, varint 1
        let p = pb::Ping::decode(raw.as_slice()).unwrap();
        assert_eq!(p.t0_us, 5);
    }
}
