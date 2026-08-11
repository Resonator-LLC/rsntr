//! Protocol-level errors for framing and envelope decoding.

use crate::frame::MAX_FRAME_LEN;

/// Errors produced by the frame codec, the envelope parser, and the readers.
///
/// None of these are ever panics: malformed, truncated, or oversized input
/// from the wire must always surface as a value of this type.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// A frame length prefix (or a payload to encode) exceeds [`MAX_FRAME_LEN`].
    #[error("frame of {len} bytes exceeds the {MAX_FRAME_LEN} byte frame budget")]
    FrameTooLarge { len: usize },

    /// The byte stream ended in the middle of a frame.
    #[error("truncated frame: {remaining} trailing bytes do not form a complete frame")]
    TruncatedFrame { remaining: usize },

    /// A frame payload is not valid UTF-8 (frames carry Turtle text).
    #[error("frame payload is not valid UTF-8: {0}")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),

    /// The Turtle document inside a frame failed to parse.
    #[error("turtle syntax error: {0}")]
    Syntax(#[from] oxttl::TurtleSyntaxError),

    /// The Turtle parsed but does not form a well-shaped envelope object.
    #[error("malformed envelope: {0}")]
    Malformed(String),

    /// A well-formed frame arrived out of order in a response or
    /// entrainment stream (e.g. a Row before the Result header).
    #[error("response choreography error: {0}")]
    Choreography(String),
}

impl ProtocolError {
    pub(crate) fn malformed(msg: impl Into<String>) -> Self {
        Self::Malformed(msg.into())
    }

    pub(crate) fn choreography(msg: impl Into<String>) -> Self {
        Self::Choreography(msg.into())
    }
}

/// The protocol-level error codes carried by `rsntr:code` on `rsntr:Error`
/// frames. Exactly these seven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// The authenticator refused the request.
    AuthDenied,
    /// The wall-clock deadline elapsed.
    Timeout,
    /// A row, byte, or resource limit was exceeded.
    LimitExceeded,
    /// The server does not serve the request's modulation.
    ModUnsupported,
    /// The engine failed executing the statement.
    EngineError,
    /// The frame or envelope itself was malformed or out of order.
    ProtocolError,
    /// A resonance point or projection path outside the caller's projection.
    PointUnknown,
}

impl ErrorCode {
    /// Every defined code, in spec order.
    pub const ALL: [ErrorCode; 7] = [
        ErrorCode::AuthDenied,
        ErrorCode::Timeout,
        ErrorCode::LimitExceeded,
        ErrorCode::ModUnsupported,
        ErrorCode::EngineError,
        ErrorCode::ProtocolError,
        ErrorCode::PointUnknown,
    ];

    /// The wire form of the code.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::AuthDenied => "auth-denied",
            ErrorCode::Timeout => "timeout",
            ErrorCode::LimitExceeded => "limit-exceeded",
            ErrorCode::ModUnsupported => "mod-unsupported",
            ErrorCode::EngineError => "engine-error",
            ErrorCode::ProtocolError => "protocol-error",
            ErrorCode::PointUnknown => "point-unknown",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ErrorCode {
    type Err = ProtocolError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| ProtocolError::malformed(format!("unknown error code {s:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip_their_wire_form() {
        for code in ErrorCode::ALL {
            assert_eq!(code.as_str().parse::<ErrorCode>().unwrap(), code);
        }
        assert!("no-such-code".parse::<ErrorCode>().is_err());
    }
}
