//! resonator-transport: the transport abstraction and its iroh implementation.
//!
//! Normative sources: docs/connection-protocol.md (handshake, plain-text
//! probe, refusal) and docs/rdf-envelope-protocol.md (hello, mod gate).
//!
//! The [`Transport`] trait is what the node pipeline programs against: dial
//! is lazy, connections are cached per peer, and one request is one fresh
//! bidirectional stream carrying length-prefixed Turtle frames. The iroh
//! implementation lives in [`iroh_transport`]; nothing in this module names
//! iroh, so alternative transports (bluetooth, radio, test doubles) stay
//! possible behind this seam.
//!
//! Choreography per connection:
//!
//! 1. The dialer opens the first bi stream and sends one `rsntr:Hello`;
//!    the acceptor answers with its own hello on the same stream, or with
//!    `rsntr:Refused` (and closes) when the version or encoding cannot be
//!    satisfied.
//! 2. Every subsequent bi stream is one request: the dialer writes the
//!    request frame(s) and half-closes; the acceptor streams response
//!    frames back and half-closes.
//! 3. A `Query`/`Execute` in a modulation the acceptor does not serve is
//!    answered with `rsntr:Error` code `mod-unsupported` by the transport
//!    layer itself, before the request ever reaches the node.

pub mod error;
pub mod iroh_transport;

use std::future::Future;

use resonator_protocol::{EnvelopeObject, Hello};

pub use error::TransportError;
pub use iroh_transport::{
    AddrSink, AddrSource, BlobGate, BlobsConfig, IrohConfig, IrohRequestStream, IrohTransport,
    PLAINTEXT_BANNER, blob_import, endpoint_id_from_secret, mint_ticket, parse_blob_hash,
    parse_ticket,
};

/// The envelope minor version this transport speaks (`rsntr:ver`); the major
/// is fixed by the ALPN `resonator/rdf/0`.
pub const ENVELOPE_VERSION: &str = "0.1";

/// Builds a well-formed hello for this transport: version [`ENVELOPE_VERSION`],
/// mandatory `turtle` encoding, the given mods list and hint.
pub fn basic_hello(mods: &[&str], hint: Option<&str>) -> Hello {
    Hello {
        envelope_version: ENVELOPE_VERSION.to_string(),
        encodings: vec!["turtle".to_string()],
        mods: mods.iter().map(|m| m.to_string()).collect(),
        hint: hint.map(|h| h.to_string()),
    }
}

/// A peer identity: the 32 bytes of an ed25519 public key (iroh's
/// `EndpointId`), kept as a plain byte newtype so trait consumers never
/// depend on iroh.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerId(pub [u8; 32]);

impl PeerId {
    /// The raw key bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Display for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PeerId({self})")
    }
}

impl std::str::FromStr for PeerId {
    type Err = TransportError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 64 || !s.is_ascii() {
            return Err(TransportError::InvalidPeerId(format!(
                "expected 64 hex chars, got {s:?}"
            )));
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            let pair = &s[2 * i..2 * i + 2];
            *byte = u8::from_str_radix(pair, 16).map_err(|_| {
                TransportError::InvalidPeerId(format!("non-hex characters in {pair:?}"))
            })?;
        }
        Ok(PeerId(out))
    }
}

/// One request's bidirectional frame stream.
///
/// `send` writes one envelope object as one frame; `finish` half-closes the
/// send direction (signalling "no more frames from me"); `recv` yields the
/// peer's frames in order, `Ok(None)` on the peer's clean half-close. The
/// raw variants are the escape hatch for the media modulation: after an
/// `rsntr:Media` header frame the rest of the stream is an unframed byte
/// feed.
pub trait RequestStream: Send {
    /// Sends one envelope object as one frame.
    fn send(
        &mut self,
        obj: &EnvelopeObject,
    ) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Receives the next envelope object, or `None` on clean end of stream.
    fn recv(
        &mut self,
    ) -> impl Future<Output = Result<Option<EnvelopeObject>, TransportError>> + Send;

    /// Sends raw bytes outside the frame protocol. Transports without a raw
    /// byte side channel report an error.
    fn send_raw(
        &mut self,
        _bytes: &[u8],
    ) -> impl Future<Output = Result<(), TransportError>> + Send {
        async {
            Err(TransportError::Stream(
                "raw byte streaming is not supported by this transport".into(),
            ))
        }
    }

    /// Receives the next raw chunk outside the frame protocol, `None` on
    /// clean end of stream. Transports without a raw byte side channel
    /// report an error.
    fn recv_raw(&mut self) -> impl Future<Output = Result<Option<Vec<u8>>, TransportError>> + Send {
        async {
            Err(TransportError::Stream(
                "raw byte streaming is not supported by this transport".into(),
            ))
        }
    }

    /// Resolves when the peer stops reading this stream (stop/reset or the
    /// connection dying). Long-running producers (the media feed) select on
    /// this so a silent source does not mask a departed client. The default
    /// never resolves, which keeps write-error detection the only signal on
    /// transports without closure notification.
    fn closed(&mut self) -> impl Future<Output = ()> + Send {
        std::future::pending()
    }

    /// Half-closes the send direction. Idempotent.
    fn finish(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send;
}

/// An incoming request surfaced to the node: the proven peer identity, the
/// hello that peer sent on this connection, the already-read first frame
/// (modulation-gated by the transport), and the open stream for the response.
#[derive(Debug)]
pub struct IncomingRequest<S> {
    /// The remote's proven identity (authenticated by the QUIC handshake).
    pub peer: PeerId,
    /// The hello the peer sent when the connection was established.
    pub peer_hello: Hello,
    /// The first frame of the request stream.
    pub first: EnvelopeObject,
    /// The stream, positioned after `first`; responses go back on it.
    pub stream: S,
}

/// The client-side transport surface the node pipeline programs against.
///
/// Dialing is lazy: `open` reuses a cached connection to the peer when one
/// is alive and dials otherwise. Every `open` call yields a fresh
/// bidirectional stream (one request = one stream) plus the hello the peer
/// sent on the underlying connection.
pub trait Transport: Send + Sync + 'static {
    /// The stream type produced by [`open`](Self::open).
    type Stream: RequestStream;

    /// This node's own identity.
    fn local_id(&self) -> PeerId;

    /// Opens a new request stream to `peer`, dialing lazily.
    fn open(
        &self,
        peer: PeerId,
    ) -> impl Future<Output = Result<(Self::Stream, Hello), TransportError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_id_hex_round_trip() {
        let id = PeerId([0xab; 32]);
        let s = id.to_string();
        assert_eq!(s.len(), 64);
        assert_eq!(s.parse::<PeerId>().expect("parse"), id);
    }

    #[test]
    fn peer_id_rejects_bad_input() {
        assert!("too-short".parse::<PeerId>().is_err());
        assert!("zz".repeat(32).parse::<PeerId>().is_err());
        // 64 bytes of multi-byte UTF-8 must not panic the slicing.
        assert!("\u{00e9}".repeat(32).parse::<PeerId>().is_err());
    }

    #[test]
    fn basic_hello_shape() {
        let h = basic_hello(&["help", "sql-sqlite"], Some("hi"));
        assert_eq!(h.envelope_version, ENVELOPE_VERSION);
        assert_eq!(h.encodings, vec!["turtle"]);
        assert_eq!(h.mods, vec!["help", "sql-sqlite"]);
        assert_eq!(h.hint.as_deref(), Some("hi"));
    }
}
