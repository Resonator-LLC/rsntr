//! Crate error type.

use resonator_protocol::ProtocolError;
use resonator_transport::TransportError;
use thiserror::Error;

/// Errors surfaced by the serving pipeline. Everything answerable on the
/// wire (denials, engine errors, timeouts) is answered there; this type
/// covers the cases where the pipeline itself cannot proceed.
#[derive(Debug, Error)]
pub enum NodeError {
    /// The dedicated sqlite thread is gone (channel closed).
    #[error("database thread is not running")]
    DbClosed,

    /// A database error outside statement execution (setup, DDL).
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    /// Sending or receiving a frame failed.
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),

    /// Encoding a response frame failed.
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),

    /// The SPARQL engine reported an error outside request execution.
    #[error("sparql error: {0}")]
    Sparql(String),
}

impl From<resonator_sparql::Error> for NodeError {
    fn from(e: resonator_sparql::Error) -> Self {
        NodeError::Sparql(e.0)
    }
}
