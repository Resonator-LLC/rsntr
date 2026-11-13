//! Frames on federation streams: a big-endian `u32` length, then that many
//! bytes of a protobuf [`FederationFrame`], at most [`MAX_FRAME_SIZE`].

use prost::Message;
use resonator_model::proto::federation_rpc::FederationFrame;
use resonator_model::proto::{WaveMessage, decode};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The largest frame accepted or sent: 4 MiB.
pub const MAX_FRAME_SIZE: usize = 4 * 1024 * 1024;

/// A frame that cannot be read or written.
#[derive(Debug, Error)]
pub enum FrameError {
    #[error("frame of {0} bytes exceeds the limit of {MAX_FRAME_SIZE}")]
    TooLarge(usize),
    #[error("invalid frame: {0}")]
    Invalid(String),
    #[error("stream ended before a complete frame")]
    Truncated,
    #[error("stream error: {0}")]
    Io(#[from] std::io::Error),
}

/// Encodes a frame with its length prefix.
pub fn encode_frame(frame: &FederationFrame) -> Result<Vec<u8>, FrameError> {
    frame
        .check_initialized()
        .map_err(|error| FrameError::Invalid(error.to_string()))?;
    let length = frame.encoded_len();
    if length > MAX_FRAME_SIZE {
        return Err(FrameError::TooLarge(length));
    }
    let mut bytes = Vec::with_capacity(4 + length);
    bytes.extend_from_slice(&(length as u32).to_be_bytes());
    frame
        .encode(&mut bytes)
        .map_err(|error| FrameError::Invalid(error.to_string()))?;
    Ok(bytes)
}

/// Writes one frame.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &FederationFrame,
) -> Result<(), FrameError> {
    let bytes = encode_frame(frame)?;
    writer.write_all(&bytes).await?;
    Ok(())
}

/// Reads one frame. The length is checked before anything else is read.
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<FederationFrame, FrameError> {
    let mut prefix = [0u8; 4];
    reader.read_exact(&mut prefix).await.map_err(eof)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length > MAX_FRAME_SIZE {
        return Err(FrameError::TooLarge(length));
    }
    let mut bytes = vec![0u8; length];
    reader.read_exact(&mut bytes).await.map_err(eof)?;
    decode(&bytes).map_err(|error| FrameError::Invalid(error.to_string()))
}

fn eof(error: std::io::Error) -> FrameError {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        FrameError::Truncated
    } else {
        FrameError::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use resonator_model::proto::federation_rpc::{
        Echo, FederationRequest, federation_frame, federation_request,
    };

    use super::*;

    fn echo(payload: Vec<u8>) -> FederationFrame {
        FederationFrame {
            body: Some(federation_frame::Body::Request(FederationRequest {
                id: Some("01J00000000000000000000000".to_owned()),
                op: Some(federation_request::Op::Echo(Echo {
                    payload: Some(payload),
                })),
            })),
        }
    }

    #[tokio::test]
    async fn frames_round_trip() {
        let frame = echo(b"hello".to_vec());
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &frame).await.unwrap();
        assert_eq!(
            u32::from_be_bytes(buffer[..4].try_into().unwrap()) as usize,
            buffer.len() - 4
        );
        let mut reader = buffer.as_slice();
        assert_eq!(read_frame(&mut reader).await.unwrap(), frame);
        assert!(matches!(
            read_frame(&mut reader).await,
            Err(FrameError::Truncated)
        ));
    }

    #[tokio::test]
    async fn limits_are_enforced() {
        // Just under and over the limit. Nested length varints grow with
        // the payload, so the largest payload that fits is found by trial.
        let mut payload = MAX_FRAME_SIZE;
        while echo(vec![7; payload]).encoded_len() > MAX_FRAME_SIZE {
            payload -= 1;
        }
        let fits = echo(vec![7; payload]);
        assert_eq!(fits.encoded_len(), MAX_FRAME_SIZE);
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &fits).await.unwrap();
        assert_eq!(read_frame(&mut buffer.as_slice()).await.unwrap(), fits);
        let too_big = echo(vec![7; MAX_FRAME_SIZE]);
        assert!(matches!(
            encode_frame(&too_big),
            Err(FrameError::TooLarge(_))
        ));

        // A length prefix over the limit is refused without reading on.
        let mut prefix = ((MAX_FRAME_SIZE + 1) as u32).to_be_bytes().to_vec();
        prefix.extend_from_slice(&[0; 16]);
        assert!(matches!(
            read_frame(&mut prefix.as_slice()).await,
            Err(FrameError::TooLarge(_))
        ));

        // Truncated body, garbage, and frames missing required parts.
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &echo(b"abc".to_vec()))
            .await
            .unwrap();
        buffer.pop();
        assert!(matches!(
            read_frame(&mut buffer.as_slice()).await,
            Err(FrameError::Truncated)
        ));
        let garbage = [0, 0, 0, 3, 0xff, 0xff, 0xff];
        assert!(matches!(
            read_frame(&mut garbage.as_slice()).await,
            Err(FrameError::Invalid(_))
        ));
        let empty = [0, 0, 0, 0];
        assert!(matches!(
            read_frame(&mut empty.as_slice()).await,
            Err(FrameError::Invalid(_))
        ));
        assert!(encode_frame(&FederationFrame { body: None }).is_err());
    }
}
