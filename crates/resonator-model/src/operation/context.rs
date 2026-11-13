//! The context in which a wavelet operation is applied.

use std::fmt;

use crate::id::ParticipantId;
use crate::version::HashedVersion;

/// The timestamp of a context without one (`Constants.NO_TIMESTAMP`).
pub const NO_TIMESTAMP: i64 = -1;

/// Who performed an operation and when, and how the operation moves the
/// wavelet version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaveletOperationContext {
    creator: ParticipantId,
    timestamp: i64,
    version_increment: i64,
    hashed_version: Option<HashedVersion>,
}

impl WaveletOperationContext {
    /// A context without a hashed version. `timestamp` is in milliseconds
    /// since the epoch, or [`NO_TIMESTAMP`]. `version_increment` is negative
    /// for reverse operations.
    pub fn new(creator: ParticipantId, timestamp: i64, version_increment: i64) -> Self {
        WaveletOperationContext {
            creator,
            timestamp,
            version_increment,
            hashed_version: None,
        }
    }

    /// A context that also sets the wavelet's hashed version, as the last
    /// operation of a delta does.
    pub fn with_hashed_version(
        creator: ParticipantId,
        timestamp: i64,
        version_increment: i64,
        hashed_version: HashedVersion,
    ) -> Self {
        WaveletOperationContext {
            creator,
            timestamp,
            version_increment,
            hashed_version: Some(hashed_version),
        }
    }

    pub fn has_timestamp(&self) -> bool {
        self.timestamp != NO_TIMESTAMP
    }

    pub fn timestamp(&self) -> i64 {
        self.timestamp
    }

    pub fn creator(&self) -> &ParticipantId {
        &self.creator
    }

    pub fn version_increment(&self) -> i64 {
        self.version_increment
    }

    pub fn has_hashed_version(&self) -> bool {
        self.hashed_version.is_some()
    }

    pub fn hashed_version(&self) -> Option<&HashedVersion> {
        self.hashed_version.as_ref()
    }
}

impl fmt::Display for WaveletOperationContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "WaveletOperationContext(creator: {}, timestamp: {}, version increment: {},hashedVersion: ",
            self.creator, self.timestamp, self.version_increment
        )?;
        match &self.hashed_version {
            Some(version) => write!(f, "{version})"),
            None => f.write_str("null)"),
        }
    }
}
