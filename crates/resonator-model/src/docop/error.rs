//! Error types for document operations.

use thiserror::Error;

/// Failure to build, apply, compose or invert a document operation.
///
/// Covers every case where the Java original throws `OperationException`,
/// `IllegalArgumentException` or `IllegalStateException` on bad input.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OperationError {
    /// The operation violates the well-formedness rules of the automaton.
    #[error("ill-formed operation ({violation}): {op}")]
    IllFormed { violation: String, op: String },
    /// Two operations cannot be composed component-wise.
    #[error("illegal composition")]
    IllegalComposition,
    /// The output length of the first operation differs from the input length
    /// of the second.
    #[error(
        "document size mismatch: op1 resulting length={resulting}, op2 initial length={initial}"
    )]
    SizeMismatch { resulting: usize, initial: usize },
    /// An attribute or annotation update does not match the state it is
    /// applied to or composed with.
    #[error("mismatched old value: {0}")]
    MismatchedOldValue(String),
    /// A map or boundary was constructed from invalid arguments.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// A UTF-16 offset falls inside a surrogate pair.
    #[error("utf-16 offset {offset} splits a surrogate pair")]
    SurrogateSplit { offset: usize },
    /// A UTF-16 offset lies beyond the end of the string.
    #[error("utf-16 offset {offset} out of bounds for string of length {len}")]
    OffsetOutOfBounds { offset: usize, len: usize },
    /// The operation is not valid for the document it is applied to.
    #[error("validation failed: {0}")]
    Validation(String),
    /// The operation does not apply to the document.
    #[error("operation does not apply: {0}")]
    Apply(String),
    /// The operation contains components that are not allowed in a document
    /// initialization.
    #[error("initialization with unexpected component {0}")]
    NotAnInitialization(&'static str),
}

/// Failure to transform a pair of concurrent operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransformError {
    /// One operation spans more of the document than the other.
    #[error(
        "ran out of {other_size} {other} op components after {index} of {size} {this} op \
         components, with {spare} spare positions"
    )]
    RanOut {
        this: &'static str,
        other: &'static str,
        other_size: usize,
        index: usize,
        size: usize,
        spare: i64,
    },
    /// The operations disagree about the document they apply to.
    #[error("incompatible operations in transformation")]
    Incompatible,
    /// A component appeared where the decomposition forbids it.
    #[error("unexpected {component} component in {op} operation")]
    UnexpectedComponent {
        component: &'static str,
        op: &'static str,
    },
    #[error(transparent)]
    Operation(#[from] OperationError),
}
