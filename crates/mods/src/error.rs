//! Crate error type.

use thiserror::Error;

/// Errors from the mods host: registry management, wasm loading, and the
/// per-request plumbing.
#[derive(Debug, Error)]
pub enum ModError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("node error: {0}")]
    Node(#[from] resonator_node::NodeError),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// A mod row that must not be loaded or stored (bad name, hash
    /// mismatch, missing wasm, capability shortfall, ABI mismatch).
    #[error("{0}")]
    Reject(String),

    /// The extism runtime failed (compile, instantiate, call).
    #[error("wasm error: {0}")]
    Wasm(String),
}

impl ModError {
    pub(crate) fn reject(msg: impl Into<String>) -> Self {
        ModError::Reject(msg.into())
    }

    pub(crate) fn wasm(err: impl std::fmt::Display) -> Self {
        ModError::Wasm(err.to_string())
    }
}
