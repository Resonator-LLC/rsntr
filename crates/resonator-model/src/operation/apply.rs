//! Application of wavelet operations to wavelet data.

use super::context::WaveletOperationContext;
use super::error::WaveletOperationError;
use super::wave_op::{
    BlipOperation, UpdateContributorMethod, WaveletOperation, WaveletOperationKind,
};
use crate::data::{BlipData, WaveletData};
use crate::docop::{DocInitialization, invert};
use crate::id::ParticipantId;

fn add_version(version: u64, increment: i64) -> Result<u64, WaveletOperationError> {
    version
        .checked_add_signed(increment)
        .filter(|v| i64::try_from(*v).is_ok())
        .ok_or(WaveletOperationError::VersionOutOfRange { version, increment })
}

/// `BlipOperation.update`: updates contributors and last-modified metadata
/// of a blip, if the operation is worthy. Returns the method that reverses
/// the contributor change.
fn update_blip(
    blip: &mut BlipData,
    blip_op: &BlipOperation,
    method: UpdateContributorMethod,
    context: &WaveletOperationContext,
    new_wavelet_version: u64,
) -> UpdateContributorMethod {
    if !blip_op.updates_blip_metadata(blip.id()) {
        return UpdateContributorMethod::None;
    }
    let creator = context.creator();
    let present = blip.contributors().contains(creator);
    let reverse = match method {
        UpdateContributorMethod::Add if !present => {
            blip.add_contributor(creator.clone());
            UpdateContributorMethod::Remove
        }
        UpdateContributorMethod::Remove if present => {
            blip.remove_contributor(creator);
            UpdateContributorMethod::Add
        }
        _ => UpdateContributorMethod::None,
    };
    blip.set_last_modified_version(new_wavelet_version);
    if context.has_timestamp() {
        blip.set_last_modified_time(context.timestamp());
    }
    reverse
}

impl WaveletOperation {
    /// The document a blip operation targets, created empty if absent with
    /// the operation's creator as author and only contributor.
    fn target_blip<'a>(
        &self,
        target: &'a mut WaveletData,
        blip_id: &str,
        new_version: u64,
    ) -> Result<&'a mut BlipData, WaveletOperationError> {
        if target.document(blip_id).is_none() {
            let creator: &ParticipantId = self.context().creator();
            target.create_document(
                blip_id,
                creator.clone(),
                [creator.clone()],
                &DocInitialization::default(),
                self.context().timestamp(),
                new_version,
            )?;
        }
        target
            .document_mut(blip_id)
            .ok_or_else(|| WaveletOperationError::MissingDocument(blip_id.to_owned()))
    }

    /// `WaveletOperation.update`: moves the wavelet's timestamp, version and
    /// hashed version as the context says.
    fn update(&self, target: &mut WaveletData, new_version: u64) {
        let context = self.context();
        if context.has_timestamp() {
            target.set_last_modified_time(context.timestamp());
        }
        target.set_version(new_version);
        if let Some(version) = context.hashed_version() {
            target.set_hashed_version(version.clone());
        }
    }

    fn reverse_context(&self, target: &WaveletData) -> WaveletOperationContext {
        WaveletOperationContext::with_hashed_version(
            self.context().creator().clone(),
            target.last_modified_time(),
            -self.context().version_increment(),
            target.hashed_version().clone(),
        )
    }

    /// Applies the operation to a wavelet and then updates the wavelet's
    /// last-modified time, version and hashed version from the context.
    ///
    /// On error the wavelet is unchanged, with one exception kept from the
    /// original: a blip operation on a missing document creates the
    /// document before its content operation is validated, so a failed
    /// content operation leaves an empty document behind.
    pub fn apply(&self, target: &mut WaveletData) -> Result<(), WaveletOperationError> {
        let context = self.context();
        let new_version = add_version(target.version(), context.version_increment())?;
        match self.kind() {
            WaveletOperationKind::NoOp => {}
            WaveletOperationKind::AddParticipant {
                participant,
                position,
            } => {
                let added = match position {
                    None => target.add_participant(participant.clone()),
                    Some(position) => target.add_participant_at(participant.clone(), *position)?,
                };
                if !added {
                    return Err(WaveletOperationError::DuplicateParticipant(
                        participant.to_string(),
                    ));
                }
            }
            WaveletOperationKind::RemoveParticipant { participant } => {
                if !target.remove_participant(participant) {
                    return Err(WaveletOperationError::MissingParticipant(
                        participant.to_string(),
                    ));
                }
            }
            WaveletOperationKind::Blip { blip_id, op } => {
                let blip = self.target_blip(target, blip_id, new_version)?;
                if let BlipOperation::Content {
                    op: content_op,
                    method,
                } = op
                {
                    blip.content_mut().consume(content_op)?;
                    update_blip(blip, op, *method, context, new_version);
                }
            }
            WaveletOperationKind::VersionUpdate {
                doc_id,
                fixed_doc_version,
            } => {
                if let Some(doc_id) = doc_id {
                    let blip = target
                        .document_mut(doc_id)
                        .ok_or_else(|| WaveletOperationError::MissingDocument(doc_id.clone()))?;
                    blip.set_last_modified_version(fixed_doc_version.unwrap_or(new_version));
                }
            }
        }
        self.update(target, new_version);
        Ok(())
    }

    /// Applies the operation like [`WaveletOperation::apply`] and returns
    /// the operations that reverse it.
    ///
    /// Quirks kept from the original: the reverse of a blip operation
    /// restores neither the wavelet's hashed version nor the blip's previous
    /// last-modified version, and sets the wavelet's last-modified time to
    /// the blip's previous one; a content operation that fails validation
    /// has already updated the blip's metadata.
    pub fn apply_and_return_reverse(
        &self,
        target: &mut WaveletData,
    ) -> Result<Vec<WaveletOperation>, WaveletOperationError> {
        let context = self.context();
        let increment = context.version_increment();
        let new_version = add_version(target.version(), increment)?;
        let reverse = match self.kind() {
            WaveletOperationKind::NoOp => {
                let reverse = WaveletOperation::no_op(self.reverse_context(target));
                self.apply(target)?;
                return Ok(vec![reverse]);
            }
            WaveletOperationKind::AddParticipant { participant, .. } => {
                let reverse = WaveletOperation::remove_participant(
                    self.reverse_context(target),
                    participant.clone(),
                );
                self.apply(target)?;
                return Ok(vec![reverse]);
            }
            WaveletOperationKind::RemoveParticipant { participant } => {
                let position = target
                    .participants()
                    .iter()
                    .position(|p| p == participant)
                    .ok_or_else(|| {
                        WaveletOperationError::MissingParticipant(participant.to_string())
                    })?;
                let reverse = WaveletOperation::new(
                    self.reverse_context(target),
                    WaveletOperationKind::AddParticipant {
                        participant: participant.clone(),
                        position: Some(position),
                    },
                );
                self.apply(target)?;
                return Ok(vec![reverse]);
            }
            WaveletOperationKind::Blip { blip_id, op } => {
                let blip = self.target_blip(target, blip_id, new_version)?;
                // No hashed version in the reverse context of a blip op.
                let reverse_context = WaveletOperationContext::new(
                    context.creator().clone(),
                    blip.last_modified_time(),
                    -increment,
                );
                let reverse_op = match op {
                    BlipOperation::Content {
                        op: content_op,
                        method,
                    } => {
                        // Metadata first, then content.
                        let reverse_method = update_blip(blip, op, *method, context, new_version);
                        blip.content_mut().consume(content_op)?;
                        BlipOperation::Content {
                            op: invert(content_op)?,
                            method: reverse_method,
                        }
                    }
                    BlipOperation::Submit => BlipOperation::Submit,
                };
                WaveletOperation::blip(reverse_context, blip_id.clone(), reverse_op)
            }
            WaveletOperationKind::VersionUpdate {
                doc_id,
                fixed_doc_version,
            } => {
                let old_hashed_version = target.hashed_version().clone();
                let old_doc_version =
                    match doc_id {
                        Some(doc_id) => {
                            let blip = target.document_mut(doc_id).ok_or_else(|| {
                                WaveletOperationError::MissingDocument(doc_id.clone())
                            })?;
                            Some(blip.set_last_modified_version(
                                fixed_doc_version.unwrap_or(new_version),
                            ))
                        }
                        None => None,
                    };
                WaveletOperation::version_update(
                    context.creator().clone(),
                    -increment,
                    Some(old_hashed_version),
                    doc_id.clone(),
                    old_doc_version,
                )
            }
        };
        self.update(target, new_version);
        Ok(vec![reverse])
    }
}
