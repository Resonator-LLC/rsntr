//! Errors of the wave server.
//!
//! Display strings of client-visible errors follow the messages of the
//! original (`WaveServerImpl`, `WaveletContainerImpl`,
//! `InvalidHashException`, `AccessControlException`,
//! `WaveletStateException`).

use resonator_model::id::{InvalidParticipantAddress, WaveletName};
use resonator_model::operation::{WaveTransformError, WaveletOperationError};
use resonator_model::proto::ProtoError;
use resonator_model::version::HashedVersion;
use thiserror::Error;

use crate::storage::StorageError;

/// A wavelet that cannot be used (`WaveletStateException`).
#[derive(Debug, Error)]
pub enum WaveletStateError {
    /// The in-memory wavelet no longer matches storage, e.g. because
    /// persisting a delta failed. It is reloaded on a later access.
    #[error("The wavelet is in an unusable state: CORRUPTED")]
    Corrupted,
    #[error("Failed to load wavelet {wavelet}: {source}")]
    Load {
        wavelet: String,
        #[source]
        source: StorageError,
    },
    #[error("Storage access failed for wavelet {wavelet}: {source}")]
    Storage {
        wavelet: String,
        #[source]
        source: StorageError,
    },
}

/// Failure of a read request to the wave server (`WaveServerException`).
#[derive(Debug, Error)]
pub enum WaveServerError {
    /// The request names a wavelet or version the caller may not see or
    /// that does not exist (`AccessControlException`).
    #[error("{0}")]
    AccessControl(String),
    #[error(transparent)]
    State(#[from] WaveletStateError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// Whether a failed submission is the client's fault or the server's
/// (`FederationErrors.badRequest` and `internalServerError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitErrorKind {
    BadRequest,
    InternalServerError,
}

/// Why a delta submission was rejected. The display string is the message
/// for the client.
#[derive(Debug, Error)]
pub enum SubmitError {
    #[error("Empty delta at version {0}")]
    EmptyDelta(u64),
    /// The delta has no version or author, or an operation the server does
    /// not understand.
    #[error("Invalid delta: {0}")]
    InvalidDelta(#[source] ProtoError),
    /// Only wavelets of the local domain are hosted here; there is no
    /// federation.
    #[error("Wavelet {wavelet} is not hosted here: {domain} is not a local domain")]
    NotLocal {
        wavelet: WaveletName,
        domain: String,
    },
    #[error("Invalid author address: {0}")]
    InvalidAuthor(#[source] InvalidParticipantAddress),
    #[error("Author field on delta must match logged in user")]
    AuthorMismatch,
    #[error("{author} is not a participant of {wavelet}")]
    NotParticipant {
        author: String,
        wavelet: WaveletName,
    },
    /// The delta targets the current version number with another hash
    /// (`InvalidHashException`). The expected hash is not in the message.
    #[error("Mismatched hash at version {}: {target}", expected.version())]
    InvalidHash {
        expected: HashedVersion,
        target: HashedVersion,
    },
    /// The delta targets a version that is not a delta boundary of the
    /// wavelet's history, or carries the wrong hash for it.
    #[error("Unrecognized target version at version {0}")]
    UnknownTargetVersion(u64),
    /// The delta conflicts with concurrent deltas (`TransformException`).
    #[error("{0}")]
    Transform(#[source] WaveTransformError),
    /// The delta does not apply to the wavelet (`OperationException`).
    #[error("{0}")]
    Operation(#[source] WaveletOperationError),
    #[error(transparent)]
    State(#[from] WaveletStateError),
    /// The remote host of the wavelet rejected the delta, or could not be
    /// reached (federation).
    #[error("{message}")]
    Remote {
        domain: String,
        message: String,
        /// Whether the delta itself was at fault (rather than the host or
        /// the connection).
        bad_request: bool,
    },
    /// The host of the wavelet could not be reached (federation); the
    /// delta was not applied and may be sent again.
    #[error("{domain} cannot be reached: {message}")]
    Unreachable { domain: String, message: String },
    /// A broken invariant in the server.
    #[error("Internal error: {0}")]
    Internal(String),
}

impl SubmitError {
    pub fn kind(&self) -> SubmitErrorKind {
        match self {
            SubmitError::State(_) | SubmitError::Internal(_) | SubmitError::Unreachable { .. } => {
                SubmitErrorKind::InternalServerError
            }
            SubmitError::Remote { bad_request, .. } => {
                if *bad_request {
                    SubmitErrorKind::BadRequest
                } else {
                    SubmitErrorKind::InternalServerError
                }
            }
            _ => SubmitErrorKind::BadRequest,
        }
    }
}
