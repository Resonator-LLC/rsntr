//! The in-memory request model.
//!
//! The wire shape is a `rsntr:Query` or `rsntr:Execute` Turtle object; once
//! decoded, the node works with this struct. The request id is a ULID
//! carried as its 16 raw bytes in memory and as the 26-character Crockford
//! base32 string on the wire.

use ulid::Ulid;

use crate::envelope::{EnvelopeObject, Statement};
use crate::error::ProtocolError;
use crate::value::Value;

/// Whether a request reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestKind {
    /// `rsntr:Query`: a read statement.
    Query,
    /// `rsntr:Execute`: a write statement.
    Execute,
}

/// Client-side caps, clamped by the serving side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RequestOptions {
    /// Maximum rows the client wants (`rsntr:rowLimit`).
    pub row_limit: Option<i64>,
    /// Maximum response bytes the client wants (`rsntr:byteLimit`).
    pub byte_limit: Option<i64>,
    /// Wall-clock timeout hint in milliseconds (`rsntr:timeoutMs`).
    pub timeout_ms: Option<i64>,
}

/// One decoded request: the in-memory form of a `rsntr:Query` or
/// `rsntr:Execute` envelope object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// ULID bytes; idempotency key for writes, correlation key for
    /// response frames. String form via [`id_string`](Self::id_string).
    pub id: [u8; 16],
    /// Query or Execute.
    pub kind: RequestKind,
    /// Payload modulation tag, e.g. `"sql-sqlite"`.
    pub modulation: String,
    /// The single statement, verbatim.
    pub signal: String,
    /// Ordered bound parameters, never inlined into the text.
    pub params: Vec<Value>,
    /// Target database name; accepted and carried, reserved for
    /// multi-database endpoints.
    pub database: Option<String>,
    /// Row/byte/timeout caps.
    pub options: RequestOptions,
}

impl Request {
    /// Creates a request with a freshly generated ULID.
    pub fn new(
        kind: RequestKind,
        modulation: impl Into<String>,
        signal: impl Into<String>,
    ) -> Self {
        Self {
            id: Ulid::new().to_bytes(),
            kind,
            modulation: modulation.into(),
            signal: signal.into(),
            params: Vec::new(),
            database: None,
            options: RequestOptions::default(),
        }
    }

    /// The wire form of the request id: 26-character Crockford base32.
    pub fn id_string(&self) -> String {
        Ulid::from_bytes(self.id).to_string()
    }

    /// Parses a wire request id (ULID string) into its 16 raw bytes.
    pub fn parse_id(s: &str) -> Result<[u8; 16], ProtocolError> {
        Ulid::from_string(s)
            .map(|u| u.to_bytes())
            .map_err(|e| ProtocolError::malformed(format!("invalid ULID request id {s:?}: {e}")))
    }

    /// Encodes into the wire envelope object (`Query` or `Execute`).
    pub fn to_envelope(&self) -> EnvelopeObject {
        let st = Statement {
            id: self.id_string(),
            modulation: self.modulation.clone(),
            signal: self.signal.clone(),
            params: self.params.clone(),
            database: self.database.clone(),
            row_limit: self.options.row_limit,
            byte_limit: self.options.byte_limit,
            timeout_ms: self.options.timeout_ms,
        };
        match self.kind {
            RequestKind::Query => EnvelopeObject::Query(st),
            RequestKind::Execute => EnvelopeObject::Execute(st),
        }
    }

    /// Decodes a `Query` or `Execute` envelope object into the in-memory
    /// form. Any other envelope class is a protocol error. `rsntr:database`
    /// is accepted and exposed.
    pub fn from_envelope(obj: &EnvelopeObject) -> Result<Self, ProtocolError> {
        let (kind, st) = match obj {
            EnvelopeObject::Query(st) => (RequestKind::Query, st),
            EnvelopeObject::Execute(st) => (RequestKind::Execute, st),
            other => {
                return Err(ProtocolError::malformed(format!(
                    "expected a rsntr:Query or rsntr:Execute frame, got {}",
                    envelope_class_name(other)
                )));
            }
        };
        Ok(Self {
            id: Self::parse_id(&st.id)?,
            kind,
            modulation: st.modulation.clone(),
            signal: st.signal.clone(),
            params: st.params.clone(),
            database: st.database.clone(),
            options: RequestOptions {
                row_limit: st.row_limit,
                byte_limit: st.byte_limit,
                timeout_ms: st.timeout_ms,
            },
        })
    }
}

/// Human-readable class name of an envelope object, for error messages.
pub(crate) fn envelope_class_name(obj: &EnvelopeObject) -> &'static str {
    match obj {
        EnvelopeObject::Query(_) => "rsntr:Query",
        EnvelopeObject::Execute(_) => "rsntr:Execute",
        EnvelopeObject::Result(_) => "rsntr:Result",
        EnvelopeObject::Row(_) => "rsntr:Row",
        EnvelopeObject::Done(_) => "rsntr:Done",
        EnvelopeObject::Denied(_) => "rsntr:Denied",
        EnvelopeObject::Error(_) => "rsntr:Error",
        EnvelopeObject::Hello(_) => "rsntr:Hello",
        EnvelopeObject::Knock(_) => "rsntr:Knock",
        EnvelopeObject::Presence(_) => "rsntr:Presence",
        EnvelopeObject::Decision(_) => "rsntr:Decision",
        EnvelopeObject::Help(_) => "rsntr:Help",
        EnvelopeObject::Media(_) => "rsntr:Media",
        EnvelopeObject::AudioDuplex(_) => "rsntr:AudioDuplex",
        EnvelopeObject::Projection(_) => "rsntr:Projection",
        EnvelopeObject::Entrain(_) => "rsntr:Entrain",
        EnvelopeObject::Vibration(_) => "rsntr:Vibration",
        EnvelopeObject::Damp(_) => "rsntr:Damp",
        EnvelopeObject::Graph(_) => "rsntr:Graph",
        EnvelopeObject::Generic(_) => "a generic rsntr frame",
    }
}
