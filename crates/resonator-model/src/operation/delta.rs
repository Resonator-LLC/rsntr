//! Deltas: lists of wavelet operations by one author.

use std::fmt;
use std::ops::Deref;

use super::context::WaveletOperationContext;
use super::error::DeltaError;
use super::wave_op::WaveletOperation;
use crate::id::ParticipantId;
use crate::version::HashedVersion;

fn write_ops(f: &mut fmt::Formatter<'_>, ops: &[WaveletOperation]) -> fmt::Result {
    for (i, op) in ops.iter().enumerate() {
        if i > 0 {
            f.write_str(",")?;
        }
        write!(f, "{op}")?;
    }
    Ok(())
}

/// Operations an author wants applied to a wavelet at a target version, as
/// submitted by a client. Dereferences to the operations.
///
/// Equality compares author, target version and operations, the latter
/// without their contexts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaveletDelta {
    author: ParticipantId,
    target_version: HashedVersion,
    ops: Vec<WaveletOperation>,
}

impl WaveletDelta {
    pub fn new(
        author: ParticipantId,
        target_version: HashedVersion,
        ops: Vec<WaveletOperation>,
    ) -> Self {
        WaveletDelta {
            author,
            target_version,
            ops,
        }
    }

    pub fn author(&self) -> &ParticipantId {
        &self.author
    }

    /// The version the delta applies at.
    pub fn target_version(&self) -> &HashedVersion {
        &self.target_version
    }

    /// The version number after the delta: each operation counts for one.
    pub fn resulting_version(&self) -> u64 {
        self.target_version.version() + self.ops.len() as u64
    }

    pub fn ops(&self) -> &[WaveletOperation] {
        &self.ops
    }

    pub fn into_ops(self) -> Vec<WaveletOperation> {
        self.ops
    }
}

impl Deref for WaveletDelta {
    type Target = [WaveletOperation];

    fn deref(&self) -> &[WaveletOperation] {
        &self.ops
    }
}

impl fmt::Display for WaveletDelta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CoreWaveletDelta({}, {}, ",
            self.author, self.target_version
        )?;
        if self.ops.is_empty() {
            f.write_str("[]")?;
        } else {
            write!(f, " {} ops: [", self.ops.len())?;
            write_ops(f, &self.ops)?;
            f.write_str("]")?;
        }
        f.write_str(")")
    }
}

/// A delta as applied by the server: transformed to the version it was
/// applied at, with the resulting hashed version and the application
/// timestamp. Dereferences to the operations.
///
/// Every operation's context has the delta's author and timestamp and a
/// version increment of one; exactly the last one carries the resulting
/// hashed version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransformedWaveletDelta {
    author: ParticipantId,
    resulting_version: HashedVersion,
    application_timestamp: i64,
    ops: Vec<WaveletOperation>,
}

impl TransformedWaveletDelta {
    /// Checks that the operation contexts are consistent with the delta.
    pub fn new(
        author: ParticipantId,
        resulting_version: HashedVersion,
        application_timestamp: i64,
        ops: Vec<WaveletOperation>,
    ) -> Result<Self, DeltaError> {
        let count = ops.len();
        if (resulting_version.version() as u128) < count as u128 {
            return Err(DeltaError::VersionTooSmall {
                version: resulting_version.version(),
                count,
            });
        }
        for (i, op) in ops.iter().enumerate() {
            let c = op.context();
            let last = i == count - 1;
            if c.creator() != &author {
                return Err(DeltaError::CreatorMismatch {
                    creator: c.creator().to_string(),
                    author: author.to_string(),
                });
            }
            if c.version_increment() != 1 {
                return Err(DeltaError::VersionIncrement(c.version_increment()));
            }
            match c.hashed_version() {
                Some(version) if last => {
                    if version != &resulting_version {
                        return Err(DeltaError::HashedVersionMismatch {
                            context: version.clone(),
                            delta: resulting_version,
                        });
                    }
                }
                None if !last => {}
                _ => {
                    return Err(DeltaError::HashedVersionPresence {
                        index: i + 1,
                        count,
                    });
                }
            }
            if c.timestamp() != application_timestamp {
                return Err(DeltaError::TimestampMismatch {
                    context: c.timestamp(),
                    delta: application_timestamp,
                });
            }
        }
        Ok(TransformedWaveletDelta {
            author,
            resulting_version,
            application_timestamp,
            ops,
        })
    }

    /// Builds a transformed delta from operations in any context by cloning
    /// them into the contexts the delta requires. Fails on operations that
    /// cannot be cloned (version updates and blip submits).
    pub fn clone_operations(
        author: ParticipantId,
        resulting_version: HashedVersion,
        application_timestamp: i64,
        ops: &[WaveletOperation],
    ) -> Result<Self, DeltaError> {
        let mut transformed = Vec::with_capacity(ops.len());
        if let Some((last, initial)) = ops.split_last() {
            let initial_context =
                WaveletOperationContext::new(author.clone(), application_timestamp, 1);
            for op in initial {
                transformed.push(op.clone_op(initial_context.clone())?);
            }
            let final_context = WaveletOperationContext::with_hashed_version(
                author.clone(),
                application_timestamp,
                1,
                resulting_version.clone(),
            );
            transformed.push(last.clone_op(final_context)?);
        }
        Self::new(
            author,
            resulting_version,
            application_timestamp,
            transformed,
        )
    }

    /// [`TransformedWaveletDelta::clone_operations`] with the author and
    /// operations of `delta`.
    pub fn from_delta(
        resulting_version: HashedVersion,
        application_timestamp: i64,
        delta: &WaveletDelta,
    ) -> Result<Self, DeltaError> {
        Self::clone_operations(
            delta.author().clone(),
            resulting_version,
            application_timestamp,
            delta.ops(),
        )
    }

    pub fn author(&self) -> &ParticipantId {
        &self.author
    }

    /// The version number the delta was applied at.
    pub fn applied_at_version(&self) -> u64 {
        self.resulting_version.version() - self.ops.len() as u64
    }

    /// The version after the delta.
    pub fn resulting_version(&self) -> &HashedVersion {
        &self.resulting_version
    }

    /// Milliseconds since the epoch.
    pub fn application_timestamp(&self) -> i64 {
        self.application_timestamp
    }

    pub fn ops(&self) -> &[WaveletOperation] {
        &self.ops
    }
}

impl Deref for TransformedWaveletDelta {
    type Target = [WaveletOperation];

    fn deref(&self) -> &[WaveletOperation] {
        &self.ops
    }
}

impl fmt::Display for TransformedWaveletDelta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TransformedWaveletDelta({}, {}, {} -> ",
            self.author, self.resulting_version, self.application_timestamp
        )?;
        if self.ops.is_empty() {
            f.write_str("[]")?;
        } else {
            write!(f, "{} ops: [", self.ops.len())?;
            write_ops(f, &self.ops)?;
            f.write_str("]")?;
        }
        f.write_str(")")
    }
}
