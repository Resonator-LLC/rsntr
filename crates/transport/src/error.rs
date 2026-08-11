//! Transport-level errors.
//!
//! iroh's own error types are deliberately stringified here so that crates
//! consuming the [`Transport`](crate::Transport) trait never name iroh.

use resonator_protocol::ProtocolError;

/// Errors produced while dialing, accepting, or streaming frames.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// A frame failed to encode or decode.
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),

    /// A 32-byte peer id is not a valid ed25519 public key, or a peer id
    /// string failed to parse.
    #[error("invalid peer id: {0}")]
    InvalidPeerId(String),

    /// A dialing ticket string failed to parse.
    #[error("invalid ticket: {0}")]
    InvalidTicket(String),

    /// Binding the local endpoint failed.
    #[error("bind failed: {0}")]
    Bind(String),

    /// Establishing a connection to the peer failed.
    #[error("dial failed: {0}")]
    Dial(String),

    /// Opening or using a request stream failed.
    #[error("stream error: {0}")]
    Stream(String),

    /// The hello exchange on a new connection went wrong (missing hello,
    /// wrong object type, connection closed mid-handshake).
    #[error("hello handshake failed: {0}")]
    Handshake(String),

    /// The peer answered the hello with `rsntr:Refused` and closed
    /// (connection protocol sec 5): version or encoding unsatisfiable.
    #[error("hello refused by peer ({code}): {reason}")]
    Refused { code: String, reason: String },

    /// A blob store or blob transfer operation failed (iroh-blobs).
    #[error("blob error: {0}")]
    Blobs(String),
}
