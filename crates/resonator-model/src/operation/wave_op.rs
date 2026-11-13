//! Wavelet operation types.

use std::fmt;

use super::context::{NO_TIMESTAMP, WaveletOperationContext};
use super::error::WaveletOperationError;
use super::worthy::{is_blip_id_worthy, is_worthy};
use crate::docop::DocOp;
use crate::id::ParticipantId;
use crate::version::HashedVersion;

/// How applying a blip content operation changes the blip's contributors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateContributorMethod {
    /// Adds the operation's creator if not already present.
    Add,
    /// Removes the operation's creator if present.
    Remove,
    /// Leaves the contributors alone.
    None,
}

/// An operation on one document of a wavelet.
///
/// Equality compares content operations syntactically and ignores the
/// contributor method, as in the original.
#[derive(Debug, Clone)]
pub enum BlipOperation {
    /// Mutates the document content (`BlipContentOperation`).
    Content {
        op: DocOp,
        method: UpdateContributorMethod,
    },
    /// Marks the blip as submitted (`SubmitBlip`); changes no state.
    Submit,
}

impl BlipOperation {
    /// A content operation that adds its creator to the contributors.
    pub fn content(op: DocOp) -> Self {
        BlipOperation::Content {
            op,
            method: UpdateContributorMethod::Add,
        }
    }

    /// Whether the operation is attributed to its creator when applied to
    /// the document `blip_id`.
    pub fn is_worthy_of_attribution(&self, blip_id: &str) -> bool {
        let worthy = match self {
            BlipOperation::Content { op, .. } => is_worthy(op),
            BlipOperation::Submit => true,
        };
        worthy && is_blip_id_worthy(blip_id)
    }

    /// Whether applying the operation to the document `blip_id` updates the
    /// blip's contributors and last-modified metadata.
    pub fn updates_blip_metadata(&self, blip_id: &str) -> bool {
        match self {
            BlipOperation::Content { .. } => self.is_worthy_of_attribution(blip_id),
            BlipOperation::Submit => false,
        }
    }
}

impl PartialEq for BlipOperation {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (BlipOperation::Content { op: a, .. }, BlipOperation::Content { op: b, .. }) => a == b,
            (BlipOperation::Submit, BlipOperation::Submit) => true,
            _ => false,
        }
    }
}

impl Eq for BlipOperation {}

impl fmt::Display for BlipOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlipOperation::Content { op, .. } => write!(f, "document op: {op}"),
            BlipOperation::Submit => f.write_str("submit"),
        }
    }
}

/// What a wavelet operation does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaveletOperationKind {
    /// Changes nothing but the version.
    NoOp,
    /// Adds a participant, at the end unless a position is given. Positions
    /// only occur in reverse operations.
    AddParticipant {
        participant: ParticipantId,
        position: Option<usize>,
    },
    RemoveParticipant {
        participant: ParticipantId,
    },
    /// Applies a blip operation to a document, creating the document if it
    /// does not exist (`WaveletBlipOperation`).
    Blip {
        blip_id: String,
        op: BlipOperation,
    },
    /// Updates only version information, optionally including a document's
    /// last-modified version (`VersionUpdateOp`). `fixed_doc_version` is
    /// set on reverse operations; otherwise the document gets the new
    /// wavelet version.
    VersionUpdate {
        doc_id: Option<String>,
        fixed_doc_version: Option<u64>,
    },
}

/// An operation on a wavelet, with the context it is applied in.
///
/// Equality ignores the context, as `equals` does in the original; compare
/// [`WaveletOperation::context`] separately where it matters.
#[derive(Debug, Clone)]
pub struct WaveletOperation {
    context: WaveletOperationContext,
    kind: WaveletOperationKind,
}

impl PartialEq for WaveletOperation {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

impl Eq for WaveletOperation {}

impl WaveletOperation {
    pub fn new(context: WaveletOperationContext, kind: WaveletOperationKind) -> Self {
        WaveletOperation { context, kind }
    }

    pub fn no_op(context: WaveletOperationContext) -> Self {
        Self::new(context, WaveletOperationKind::NoOp)
    }

    pub fn add_participant(context: WaveletOperationContext, participant: ParticipantId) -> Self {
        Self::new(
            context,
            WaveletOperationKind::AddParticipant {
                participant,
                position: None,
            },
        )
    }

    pub fn remove_participant(
        context: WaveletOperationContext,
        participant: ParticipantId,
    ) -> Self {
        Self::new(
            context,
            WaveletOperationKind::RemoveParticipant { participant },
        )
    }

    pub fn blip(
        context: WaveletOperationContext,
        blip_id: impl Into<String>,
        op: BlipOperation,
    ) -> Self {
        Self::new(
            context,
            WaveletOperationKind::Blip {
                blip_id: blip_id.into(),
                op,
            },
        )
    }

    /// A content operation on a document that adds its creator to the
    /// document's contributors.
    pub fn blip_content(
        context: WaveletOperationContext,
        blip_id: impl Into<String>,
        content_op: DocOp,
    ) -> Self {
        Self::blip(context, blip_id, BlipOperation::content(content_op))
    }

    /// A version update without a timestamp.
    pub fn version_update(
        creator: ParticipantId,
        version_increment: i64,
        hashed_version: Option<HashedVersion>,
        doc_id: Option<String>,
        fixed_doc_version: Option<u64>,
    ) -> Self {
        let context = match hashed_version {
            Some(version) => WaveletOperationContext::with_hashed_version(
                creator,
                NO_TIMESTAMP,
                version_increment,
                version,
            ),
            None => WaveletOperationContext::new(creator, NO_TIMESTAMP, version_increment),
        };
        Self::new(
            context,
            WaveletOperationKind::VersionUpdate {
                doc_id,
                fixed_doc_version,
            },
        )
    }

    pub fn context(&self) -> &WaveletOperationContext {
        &self.context
    }

    pub fn kind(&self) -> &WaveletOperationKind {
        &self.kind
    }

    pub fn into_parts(self) -> (WaveletOperationContext, WaveletOperationKind) {
        (self.context, self.kind)
    }

    /// The same operation in a new context (`WaveletOperation.cloneOp`).
    ///
    /// As in the original, an add-participant position and a content
    /// operation's contributor method are reset, and version updates and
    /// blip submits cannot be cloned.
    pub fn clone_op(
        &self,
        new_context: WaveletOperationContext,
    ) -> Result<WaveletOperation, WaveletOperationError> {
        match &self.kind {
            WaveletOperationKind::NoOp => Ok(Self::no_op(new_context)),
            WaveletOperationKind::AddParticipant { participant, .. } => {
                Ok(Self::add_participant(new_context, participant.clone()))
            }
            WaveletOperationKind::RemoveParticipant { participant } => {
                Ok(Self::remove_participant(new_context, participant.clone()))
            }
            WaveletOperationKind::Blip {
                blip_id,
                op: BlipOperation::Content { op, .. },
            } => Ok(Self::blip_content(new_context, blip_id.clone(), op.clone())),
            _ => Err(WaveletOperationError::Uncloneable(self.to_string())),
        }
    }

    /// The version update that stands for this operation when its effect is
    /// already present (`createVersionUpdateOp`).
    pub fn create_version_update_op(
        &self,
        version_increment: i64,
        hashed_version: Option<HashedVersion>,
    ) -> WaveletOperation {
        let doc_id = match &self.kind {
            WaveletOperationKind::Blip { blip_id, op } if op.updates_blip_metadata(blip_id) => {
                Some(blip_id.clone())
            }
            _ => None,
        };
        Self::version_update(
            self.context.creator().clone(),
            version_increment,
            hashed_version,
            doc_id,
            None,
        )
    }

    /// Whether the operation is attributed to its creator. Only unworthy
    /// blip operations are not.
    pub fn is_worthy_of_attribution(&self) -> bool {
        match &self.kind {
            WaveletOperationKind::Blip { blip_id, op } => op.is_worthy_of_attribution(blip_id),
            _ => true,
        }
    }

    fn suffix(&self) -> String {
        let version = match self.context.hashed_version() {
            Some(version) => version.to_string(),
            None => "null".to_owned(),
        };
        format!(
            "by {} at {} version {}",
            self.context.creator(),
            self.context.timestamp(),
            version
        )
    }
}

/// Like the original `toString`, except that the timestamp is printed as a
/// number of milliseconds rather than a date.
impl fmt::Display for WaveletOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            WaveletOperationKind::NoOp => write!(f, "no-op {}", self.suffix()),
            WaveletOperationKind::AddParticipant {
                participant,
                position,
            } => {
                write!(f, "add participant {participant}")?;
                if let Some(position) = position {
                    write!(f, " at position {position}")?;
                }
                write!(f, " {}", self.suffix())
            }
            WaveletOperationKind::RemoveParticipant { participant } => {
                write!(f, "remove participant {participant} {}", self.suffix())
            }
            WaveletOperationKind::Blip { blip_id, op } => write!(
                f,
                "{op} on blip {blip_id} {} isWorthy {}",
                self.suffix(),
                self.is_worthy_of_attribution()
            ),
            WaveletOperationKind::VersionUpdate {
                doc_id,
                fixed_doc_version,
            } => write!(
                f,
                "version update op, blip id {} blipVersion {}",
                doc_id.as_deref().unwrap_or("null"),
                fixed_doc_version.map_or(-1, |v| v as i64)
            ),
        }
    }
}
