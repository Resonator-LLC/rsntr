//! Frame codec: u32 LE length prefix + one self-contained UTF-8 Turtle
//! document. Frame budget is 256 KiB. Pure byte-level code: no I/O, no
//! async; callers own the transport.

use bytes::{Buf, BufMut, BytesMut};

use crate::envelope::{EnvelopeObject, EnvelopeParser};
use crate::error::ProtocolError;

/// Maximum frame payload size in bytes (the length prefix is not counted).
pub const MAX_FRAME_LEN: usize = 256 * 1024;

/// Appends one frame (length prefix + payload) to `dst`.
///
/// Errors with [`ProtocolError::FrameTooLarge`] if the payload exceeds
/// [`MAX_FRAME_LEN`].
pub fn encode_frame(payload: &[u8], dst: &mut BytesMut) -> Result<(), ProtocolError> {
    if payload.len() > MAX_FRAME_LEN {
        return Err(ProtocolError::FrameTooLarge { len: payload.len() });
    }
    dst.reserve(4 + payload.len());
    dst.put_u32_le(payload.len() as u32);
    dst.put_slice(payload);
    Ok(())
}

/// Attempts to decode one frame payload from the front of `src`.
///
/// Returns `Ok(None)` when `src` does not yet hold a complete frame (feed
/// more bytes and call again); consumed bytes are removed from `src`.
/// Errors on an oversized length prefix or a non-UTF-8 payload. Never
/// panics on truncated or garbage input.
pub fn decode_frame(src: &mut BytesMut) -> Result<Option<String>, ProtocolError> {
    if src.len() < 4 {
        return Ok(None);
    }
    let mut len_bytes = [0u8; 4];
    len_bytes.copy_from_slice(&src[..4]);
    let len = u32::from_le_bytes(len_bytes) as usize;
    if len > MAX_FRAME_LEN {
        return Err(ProtocolError::FrameTooLarge { len });
    }
    if src.len() < 4 + len {
        return Ok(None);
    }
    src.advance(4);
    let payload = src.split_to(len);
    let text = String::from_utf8(payload.to_vec())?;
    Ok(Some(text))
}

/// Like [`decode_frame`], but the stream is known to be finished: leftover
/// bytes that do not form a complete frame are a
/// [`ProtocolError::TruncatedFrame`] instead of `Ok(None)`.
pub fn decode_frame_eof(src: &mut BytesMut) -> Result<Option<String>, ProtocolError> {
    match decode_frame(src)? {
        Some(payload) => Ok(Some(payload)),
        None if src.is_empty() => Ok(None),
        None => Err(ProtocolError::TruncatedFrame {
            remaining: src.len(),
        }),
    }
}

/// Serializes an envelope object to Turtle and appends it as one frame.
pub fn encode_envelope(obj: &EnvelopeObject, dst: &mut BytesMut) -> Result<(), ProtocolError> {
    let turtle = obj.to_turtle()?;
    encode_frame(turtle.as_bytes(), dst)
}

/// Decodes one frame from `src` and parses it as an envelope object.
///
/// Returns `Ok(None)` when `src` does not yet hold a complete frame.
pub fn decode_envelope(src: &mut BytesMut) -> Result<Option<EnvelopeObject>, ProtocolError> {
    let Some(payload) = decode_frame(src)? else {
        return Ok(None);
    };
    let mut parser = EnvelopeParser::new();
    // Feed in bounded chunks to exercise the incremental path; the payload
    // is already fully buffered by the framing layer.
    for chunk in payload.as_bytes().chunks(8 * 1024) {
        parser.push(chunk)?;
    }
    parser.finish().map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_then_decode() {
        let mut buf = BytesMut::new();
        encode_frame(b"hello", &mut buf).unwrap();
        assert_eq!(&buf[..4], &5u32.to_le_bytes());
        let got = decode_frame(&mut buf).unwrap().unwrap();
        assert_eq!(got, "hello");
        assert!(buf.is_empty());
    }

    #[test]
    fn oversized_payload_rejected_on_encode() {
        let big = vec![b'x'; MAX_FRAME_LEN + 1];
        let mut buf = BytesMut::new();
        assert!(matches!(
            encode_frame(&big, &mut buf),
            Err(ProtocolError::FrameTooLarge { .. })
        ));
        assert!(buf.is_empty());
    }

    #[test]
    fn oversized_length_prefix_rejected_on_decode() {
        let mut buf = BytesMut::new();
        buf.put_u32_le((MAX_FRAME_LEN as u32) + 1);
        buf.put_slice(b"whatever");
        assert!(matches!(
            decode_frame(&mut buf),
            Err(ProtocolError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn max_size_payload_accepted() {
        let payload = vec![b'a'; MAX_FRAME_LEN];
        let mut buf = BytesMut::new();
        encode_frame(&payload, &mut buf).unwrap();
        let got = decode_frame(&mut buf).unwrap().unwrap();
        assert_eq!(got.len(), MAX_FRAME_LEN);
    }

    #[test]
    fn incomplete_frame_yields_none_then_completes() {
        let mut full = BytesMut::new();
        encode_frame(b"turtle turtle", &mut full).unwrap();
        let mut src = BytesMut::new();
        for (i, b) in full.iter().enumerate() {
            assert!(
                decode_frame(&mut src).unwrap().is_none(),
                "premature frame at byte {i}"
            );
            src.put_u8(*b);
        }
        let got = decode_frame(&mut src).unwrap().unwrap();
        assert_eq!(got, "turtle turtle");
    }

    #[test]
    fn truncated_frame_errors_at_eof() {
        let mut full = BytesMut::new();
        encode_frame(b"abcdef", &mut full).unwrap();
        let mut src = BytesMut::from(&full[..full.len() - 2]);
        assert!(matches!(
            decode_frame_eof(&mut src),
            Err(ProtocolError::TruncatedFrame { .. })
        ));
        // A bare partial length prefix is also truncation.
        let mut src = BytesMut::from(&[1u8, 0][..]);
        assert!(matches!(
            decode_frame_eof(&mut src),
            Err(ProtocolError::TruncatedFrame { .. })
        ));
        // An empty buffer at EOF is a clean end of stream.
        let mut src = BytesMut::new();
        assert!(decode_frame_eof(&mut src).unwrap().is_none());
    }

    #[test]
    fn invalid_utf8_payload_errors() {
        let mut buf = BytesMut::new();
        buf.put_u32_le(2);
        buf.put_slice(&[0xFF, 0xFE]);
        assert!(matches!(
            decode_frame(&mut buf),
            Err(ProtocolError::InvalidUtf8(_))
        ));
    }

    #[test]
    fn garbage_payload_is_syntax_error_not_panic() {
        let mut buf = BytesMut::new();
        encode_frame(b"this is not turtle @@@", &mut buf).unwrap();
        assert!(matches!(
            decode_envelope(&mut buf),
            Err(ProtocolError::Syntax(_))
        ));
    }
}
