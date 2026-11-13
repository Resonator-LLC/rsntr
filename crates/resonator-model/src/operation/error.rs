//! Errors of wavelet operations, deltas and their transformation.

use thiserror::Error;

use crate::data::DataError;
use crate::docop;
use crate::version::HashedVersion;

/// Failure to apply a wavelet operation (`OperationException`, plus the
/// runtime exceptions the original throws on inconsistent input).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WaveletOperationError {
    #[error("Attempt to add a duplicate participant {0}")]
    DuplicateParticipant(String),
    #[error("Attempt to remove non-existent participant {0}")]
    MissingParticipant(String),
    /// A version update names a document that does not exist.
    #[error("no document {0}")]
    MissingDocument(String),
    /// The operation would move a version below zero or beyond the range of
    /// a Java long.
    #[error("version out of range: {version} + {increment}")]
    VersionOutOfRange { version: u64, increment: i64 },
    #[error(transparent)]
    Document(#[from] docop::OperationError),
    #[error(transparent)]
    Data(#[from] DataError),
    /// The operation cannot be cloned with a new context.
    #[error("Un-cloneable operation: {0}")]
    Uncloneable(String),
    /// A delta does not apply at the wavelet's version.
    #[error("Delta's version {delta_version} doesn't apply to wavelet at {wavelet_version}")]
    DeltaVersionMismatch {
        delta_version: u64,
        wavelet_version: u64,
    },
    /// An operation of a delta failed and the operations before it were
    /// rolled back.
    #[error(
        "Only applied {applied} of {total} operations at version {version}, rolling back, \
         failed op was {failed_op}: {cause}"
    )]
    DeltaFailed {
        applied: usize,
        total: usize,
        version: u64,
        failed_op: String,
        cause: Box<WaveletOperationError>,
    },
    /// Rolling back a failed delta failed; the wavelet state is corrupt.
    #[error("Failed to roll back operation with inverse {inverse}: {cause}")]
    RollbackFailed {
        inverse: String,
        cause: Box<WaveletOperationError>,
    },
    #[error("{0}")]
    InvalidArgument(String),
}

/// Failure to transform a pair of concurrent wavelet operations
/// (`TransformException`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WaveTransformError {
    /// The author of the client operation was concurrently removed from the
    /// wavelet (`RemovedAuthorException`). Carries the author's address.
    #[error("{0}")]
    RemovedAuthor(String),
    /// A participant was concurrently added and removed.
    #[error("Transform error involving participant: {0}")]
    Participant(String),
    #[error(transparent)]
    Document(#[from] docop::TransformError),
}

/// A transformed delta whose operation contexts are inconsistent with it.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DeltaError {
    #[error("Context creator {creator} doesn't match delta author {author}")]
    CreatorMismatch { creator: String, author: String },
    #[error("Invalid context version increment {0}")]
    VersionIncrement(i64),
    #[error("[Un]expected hashed version on op {index} of {count}")]
    HashedVersionPresence { index: usize, count: usize },
    #[error("Context hashed version {context} doesn't match delta hashed version {delta}")]
    HashedVersionMismatch {
        context: HashedVersion,
        delta: HashedVersion,
    },
    #[error("Context timestamp {context} doesn't match delta timestamp {delta}")]
    TimestampMismatch { context: i64, delta: i64 },
    /// The resulting version is smaller than the number of operations.
    #[error("resulting version {version} is less than the operation count {count}")]
    VersionTooSmall { version: u64, count: usize },
    #[error(transparent)]
    Operation(#[from] WaveletOperationError),
}

/// Failure to transform a submitted delta to the current wavelet version.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SubmitTransformError {
    /// The delta targets the current version number with a different hash
    /// (`InvalidHashException`).
    #[error("Mismatched hash, expected {expected} but delta targets {target}")]
    InvalidHash {
        expected: HashedVersion,
        target: HashedVersion,
    },
    /// The server history given does not lead from the delta's target
    /// version to the current version.
    #[error("No deltas between valid versions {target} and {current}")]
    History {
        target: HashedVersion,
        current: HashedVersion,
    },
    #[error(transparent)]
    Transform(#[from] WaveTransformError),
}
