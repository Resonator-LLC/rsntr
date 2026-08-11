//! resonator-protocol: the `rsntr:` vocabulary and the RDF envelope frame
//! codec for the resonator network.
//!
//! This crate has no I/O and no async: it turns bytes into typed envelope
//! objects and back. It compiles for wasm32-unknown-unknown.
//!
//! - [`frame`]: u32 LE length-prefixed UTF-8 Turtle frames, 256 KiB budget.
//! - [`envelope`]: [`EnvelopeObject`] and the push-based [`EnvelopeParser`]
//!   built on oxttl's low-level Turtle API, with the implied prefix block
//!   (`rsntr:`, `xsd:`, `rdf:`, `rdfs:`) registered on parse and stripped
//!   on write.
//! - [`value`]: the engine-value-to-RDF-literal mapping.
//! - [`vocab`]: the vocabulary IRIs (`http://resonator.network/v3/rsntr#`).
//! - [`request`]: [`Request`], [`RequestKind`], [`RequestOptions`].
//! - [`response`] / [`entrainment`]: the ordered state machines over a
//!   request's response frames and an entrainment's frames.
//! - [`error::ErrorCode`]: the seven protocol-level `rsntr:code` values.

pub mod entrainment;
pub mod envelope;
pub mod error;
pub mod frame;
pub mod request;
pub mod response;
pub mod value;
pub mod vocab;

pub use entrainment::{EntrainmentEvent, EntrainmentReader};
pub use envelope::{
    AudioDuplex, Damp, Decision, Denied, Done, Entrain, EnvelopeObject, EnvelopeParser,
    ErrorEnvelope, Generic, Graph, Hello, Help, Knock, Media, Point, PointField, PointKind,
    Presence, Projection, ResultHeader, Row, Statement, Vibration, decode_column_name,
    encode_column_name,
};
pub use error::{ErrorCode, ProtocolError};
pub use frame::{
    MAX_FRAME_LEN, decode_envelope, decode_frame, decode_frame_eof, encode_envelope, encode_frame,
};
pub use request::{Request, RequestKind, RequestOptions};
pub use response::{ResponseEvent, ResponseReader};
pub use value::Value;
pub use vocab::mod_matches;

/// The iroh ALPN carrying the envelope major version.
pub const ALPN: &[u8] = b"resonator/rdf/0";
