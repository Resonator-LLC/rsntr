//! Conversion between protobuf messages and model types.

use super::delta_store::ProtoTransformedWaveletDelta;
use super::federation::protocol_document_operation::Component;
use super::federation::protocol_document_operation::component::{
    AnnotationBoundary, ElementStart, KeyValuePair, KeyValueUpdate, ReplaceAttributes,
    UpdateAttributes,
};
use super::federation::protocol_wavelet_operation::MutateDocument;
use super::federation::{
    ProtocolAppliedWaveletDelta, ProtocolDocumentOperation, ProtocolHashedVersion,
    ProtocolSignedDelta, ProtocolWaveletDelta, ProtocolWaveletOperation,
};
use super::waveclient_rpc::{DocumentSnapshot, WaveletSnapshot};
use super::{ProtoError, WaveMessage, decode};
use crate::data::{BlipData, WaveletData};
use crate::docop::{
    AnnotationBoundaryMap, AnnotationChange, Attribute, Attributes, AttributesUpdate, DocOp,
    DocOpComponent, as_initialization,
};
use crate::id::{IdSerialiser, ModernIdSerialiser, ParticipantId, WaveId, WaveletName};
use crate::operation::{
    BlipOperation, NO_TIMESTAMP, TransformedWaveletDelta, WaveletDelta, WaveletOperation,
    WaveletOperationContext, WaveletOperationKind,
};
use crate::version::{HashedVersion, HashedVersionFactory};

fn required<'a, T>(field: &'a Option<T>, name: &'static str) -> Result<&'a T, ProtoError> {
    field.as_ref().ok_or(ProtoError::MissingField(name))
}

fn invalid(field: &'static str, message: impl Into<String>) -> ProtoError {
    ProtoError::InvalidValue {
        field,
        message: message.into(),
    }
}

// Hashed versions.

/// Fails if the version does not fit a Java long.
pub fn serialize_hashed_version(
    version: &HashedVersion,
) -> Result<ProtocolHashedVersion, ProtoError> {
    let number = i64::try_from(version.version()).map_err(|_| {
        invalid(
            "ProtocolHashedVersion.version",
            "exceeds the range of int64",
        )
    })?;
    Ok(ProtocolHashedVersion {
        version: Some(number),
        history_hash: Some(version.history_hash().to_vec()),
    })
}

/// Fails on a negative version or a missing field.
pub fn deserialize_hashed_version(
    version: &ProtocolHashedVersion,
) -> Result<HashedVersion, ProtoError> {
    let number = *required(&version.version, "ProtocolHashedVersion.version")?;
    let number = u64::try_from(number)
        .map_err(|_| invalid("ProtocolHashedVersion.version", "negative version"))?;
    let hash = required(&version.history_hash, "ProtocolHashedVersion.history_hash")?;
    Ok(HashedVersion::of(number, hash.clone()))
}

// Document operations.

fn key_value_pairs(attributes: &Attributes) -> Vec<KeyValuePair> {
    attributes
        .iter()
        .map(|(key, value)| KeyValuePair {
            key: Some(key.to_owned()),
            value: Some(value.to_owned()),
        })
        .collect()
}

fn key_value_update(key: &str, old_value: Option<&str>, new_value: Option<&str>) -> KeyValueUpdate {
    KeyValueUpdate {
        key: Some(key.to_owned()),
        old_value: old_value.map(str::to_owned),
        new_value: new_value.map(str::to_owned),
    }
}

fn element_start(tag: &str, attributes: &Attributes) -> ElementStart {
    ElementStart {
        r#type: Some(tag.to_owned()),
        attribute: key_value_pairs(attributes),
    }
}

/// Converts a document operation to its message, one component each.
/// Fails if a retain count exceeds the range of int32.
pub fn serialize_doc_op(op: &DocOp) -> Result<ProtocolDocumentOperation, ProtoError> {
    let mut output = ProtocolDocumentOperation::default();
    for component in op {
        let mut c = Component::default();
        match component {
            DocOpComponent::Retain(item_count) => {
                c.retain_item_count = Some(i32::try_from(*item_count).map_err(|_| {
                    invalid("Component.retain_item_count", "exceeds the range of int32")
                })?);
            }
            DocOpComponent::Characters(characters) => c.characters = Some(characters.clone()),
            DocOpComponent::DeleteCharacters(characters) => {
                c.delete_characters = Some(characters.clone());
            }
            DocOpComponent::ElementStart { tag, attributes } => {
                c.element_start = Some(element_start(tag, attributes));
            }
            DocOpComponent::DeleteElementStart { tag, attributes } => {
                c.delete_element_start = Some(element_start(tag, attributes));
            }
            DocOpComponent::ElementEnd => c.element_end = Some(true),
            DocOpComponent::DeleteElementEnd => c.delete_element_end = Some(true),
            DocOpComponent::ReplaceAttributes {
                old_attributes,
                new_attributes,
            } => {
                let mut r = ReplaceAttributes::default();
                if old_attributes.is_empty() && new_attributes.is_empty() {
                    r.empty = Some(true);
                } else {
                    r.old_attribute = key_value_pairs(old_attributes);
                    r.new_attribute = key_value_pairs(new_attributes);
                }
                c.replace_attributes = Some(r);
            }
            DocOpComponent::UpdateAttributes(update) => {
                let mut u = UpdateAttributes::default();
                if update.is_empty() {
                    u.empty = Some(true);
                } else {
                    u.attribute_update = update
                        .updates()
                        .iter()
                        .map(|update| {
                            key_value_update(
                                &update.name,
                                update.old_value.as_deref(),
                                update.new_value.as_deref(),
                            )
                        })
                        .collect();
                }
                c.update_attributes = Some(u);
            }
            DocOpComponent::AnnotationBoundary(map) => {
                let mut a = AnnotationBoundary::default();
                if map.is_empty() {
                    a.empty = Some(true);
                } else {
                    a.end = map.end_keys().to_vec();
                    a.change = map
                        .changes()
                        .iter()
                        .map(|change| {
                            key_value_update(
                                &change.key,
                                change.old_value.as_deref(),
                                change.new_value.as_deref(),
                            )
                        })
                        .collect();
                }
                c.annotation_boundary = Some(a);
            }
        }
        output.component.push(c);
    }
    Ok(output)
}

/// Builds attributes from pairs in any order; a repeated key keeps its last
/// value, as when the original collects the pairs into a hash map.
fn attributes(pairs: &[KeyValuePair]) -> Result<Attributes, ProtoError> {
    let mut entries: Vec<Attribute> = Vec::with_capacity(pairs.len());
    for pair in pairs {
        let key = required(&pair.key, "KeyValuePair.key")?;
        let value = required(&pair.value, "KeyValuePair.value")?;
        match entries.iter_mut().find(|entry| &entry.name == key) {
            Some(entry) => entry.value = value.clone(),
            None => entries.push(Attribute::new(key.clone(), value.clone())),
        }
    }
    Ok(Attributes::from_unsorted(entries)?)
}

fn update_key(update: &KeyValueUpdate) -> Result<&str, ProtoError> {
    Ok(required(&update.key, "KeyValueUpdate.key")?)
}

/// Converts a message to a document operation and checks that it is
/// well-formed.
///
/// A component with several fields set is read as the first of them in
/// field order, a boolean component counts when present even if false, and
/// a component with no field set is skipped, as in the original. A negative
/// retain count is rejected.
pub fn deserialize_doc_op(op: &ProtocolDocumentOperation) -> Result<DocOp, ProtoError> {
    let mut components = Vec::with_capacity(op.component.len());
    for c in &op.component {
        let component = if let Some(a) = &c.annotation_boundary {
            if a.empty.unwrap_or(false) {
                DocOpComponent::AnnotationBoundary(AnnotationBoundaryMap::empty())
            } else {
                let mut changes = Vec::with_capacity(a.change.len());
                for change in &a.change {
                    changes.push(AnnotationChange::new(
                        update_key(change)?,
                        change.old_value.as_deref(),
                        change.new_value.as_deref(),
                    ));
                }
                DocOpComponent::AnnotationBoundary(AnnotationBoundaryMap::new(
                    a.end.clone(),
                    changes,
                )?)
            }
        } else if let Some(characters) = &c.characters {
            DocOpComponent::Characters(characters.clone())
        } else if let Some(e) = &c.element_start {
            DocOpComponent::ElementStart {
                tag: required(&e.r#type, "ElementStart.type")?.clone(),
                attributes: attributes(&e.attribute)?,
            }
        } else if c.element_end.is_some() {
            DocOpComponent::ElementEnd
        } else if let Some(item_count) = c.retain_item_count {
            DocOpComponent::Retain(
                usize::try_from(item_count)
                    .map_err(|_| invalid("Component.retain_item_count", "negative retain count"))?,
            )
        } else if let Some(characters) = &c.delete_characters {
            DocOpComponent::DeleteCharacters(characters.clone())
        } else if let Some(e) = &c.delete_element_start {
            DocOpComponent::DeleteElementStart {
                tag: required(&e.r#type, "ElementStart.type")?.clone(),
                attributes: attributes(&e.attribute)?,
            }
        } else if c.delete_element_end.is_some() {
            DocOpComponent::DeleteElementEnd
        } else if let Some(r) = &c.replace_attributes {
            if r.empty.unwrap_or(false) {
                DocOpComponent::ReplaceAttributes {
                    old_attributes: Attributes::new(),
                    new_attributes: Attributes::new(),
                }
            } else {
                DocOpComponent::ReplaceAttributes {
                    old_attributes: attributes(&r.old_attribute)?,
                    new_attributes: attributes(&r.new_attribute)?,
                }
            }
        } else if let Some(u) = &c.update_attributes {
            if u.empty.unwrap_or(false) {
                DocOpComponent::UpdateAttributes(AttributesUpdate::new())
            } else {
                let mut triples = Vec::with_capacity(u.attribute_update.len());
                for update in &u.attribute_update {
                    triples.push((
                        update_key(update)?,
                        update.old_value.as_deref(),
                        update.new_value.as_deref(),
                    ));
                }
                DocOpComponent::UpdateAttributes(AttributesUpdate::from_triples(triples)?)
            }
        } else {
            continue;
        };
        components.push(component);
    }
    Ok(DocOp::new(components)?)
}

// Wavelet operations.

/// Converts a wavelet operation to its message. Blip submits and version
/// updates have no encoding.
pub fn serialize_op(op: &WaveletOperation) -> Result<ProtocolWaveletOperation, ProtoError> {
    let mut output = ProtocolWaveletOperation::default();
    match op.kind() {
        WaveletOperationKind::NoOp => output.no_op = Some(true),
        WaveletOperationKind::AddParticipant { participant, .. } => {
            output.add_participant = Some(participant.address().to_owned());
        }
        WaveletOperationKind::RemoveParticipant { participant } => {
            output.remove_participant = Some(participant.address().to_owned());
        }
        WaveletOperationKind::Blip {
            blip_id,
            op: BlipOperation::Content { op, .. },
        } => {
            output.mutate_document = Some(MutateDocument {
                document_id: Some(blip_id.clone()),
                document_operation: Some(serialize_doc_op(op)?),
            });
        }
        WaveletOperationKind::Blip {
            op: BlipOperation::Submit,
            ..
        } => return Err(ProtoError::Unsupported(format!("blip operation: {op}"))),
        WaveletOperationKind::VersionUpdate { .. } => {
            return Err(ProtoError::Unsupported(format!("wavelet operation: {op}")));
        }
    }
    Ok(output)
}

/// Converts a message to a wavelet operation in the given context. If
/// several fields are set the first of no-op, add participant, remove
/// participant and mutate document wins; a present `no_op` counts even if
/// false. Participant addresses are lower-cased but not validated.
pub fn deserialize_op(
    op: &ProtocolWaveletOperation,
    context: WaveletOperationContext,
) -> Result<WaveletOperation, ProtoError> {
    if op.no_op.is_some() {
        Ok(WaveletOperation::no_op(context))
    } else if let Some(address) = &op.add_participant {
        Ok(WaveletOperation::add_participant(
            context,
            ParticipantId::new_unchecked(address),
        ))
    } else if let Some(address) = &op.remove_participant {
        Ok(WaveletOperation::remove_participant(
            context,
            ParticipantId::new_unchecked(address),
        ))
    } else if let Some(mutation) = &op.mutate_document {
        let document_id = required(&mutation.document_id, "MutateDocument.document_id")?;
        let document_operation = required(
            &mutation.document_operation,
            "MutateDocument.document_operation",
        )?;
        Ok(WaveletOperation::blip_content(
            context,
            document_id.clone(),
            deserialize_doc_op(document_operation)?,
        ))
    } else {
        Err(ProtoError::Unsupported("operation: (empty)".to_owned()))
    }
}

// Deltas.

fn serialize_ops(ops: &[WaveletOperation]) -> Result<Vec<ProtocolWaveletOperation>, ProtoError> {
    ops.iter().map(serialize_op).collect()
}

/// Converts a client delta to its message.
pub fn serialize_delta(delta: &WaveletDelta) -> Result<ProtocolWaveletDelta, ProtoError> {
    Ok(ProtocolWaveletDelta {
        hashed_version: Some(serialize_hashed_version(delta.target_version())?),
        author: Some(delta.author().address().to_owned()),
        operation: serialize_ops(delta)?,
        address_path: Vec::new(),
    })
}

/// Converts a transformed delta to the message sent to clients: the
/// applied-at version without a hash, the author and the operations.
pub fn serialize_transformed_delta(
    delta: &TransformedWaveletDelta,
) -> Result<ProtocolWaveletDelta, ProtoError> {
    Ok(ProtocolWaveletDelta {
        hashed_version: Some(serialize_hashed_version(&HashedVersion::unsigned(
            delta.applied_at_version(),
        ))?),
        author: Some(delta.author().address().to_owned()),
        operation: serialize_ops(delta)?,
        address_path: Vec::new(),
    })
}

/// Converts a message to a client delta. Operation contexts have the
/// author as creator, no timestamp and a version increment of one.
///
/// As in the original, the author's address is validated only if the delta
/// has operations.
pub fn deserialize_delta(delta: &ProtocolWaveletDelta) -> Result<WaveletDelta, ProtoError> {
    let author = required(&delta.author, "ProtocolWaveletDelta.author")?;
    let mut ops = Vec::with_capacity(delta.operation.len());
    for op in &delta.operation {
        let context = WaveletOperationContext::new(ParticipantId::of(author)?, NO_TIMESTAMP, 1);
        ops.push(deserialize_op(op, context)?);
    }
    let hashed_version = deserialize_hashed_version(required(
        &delta.hashed_version,
        "ProtocolWaveletDelta.hashed_version",
    )?)?;
    Ok(WaveletDelta::new(
        ParticipantId::new_unchecked(author),
        hashed_version,
        ops,
    ))
}

fn deserialize_transformed_ops(
    ops: &[ProtocolWaveletOperation],
    author: &ParticipantId,
    resulting_version: &HashedVersion,
    application_timestamp: i64,
) -> Result<Vec<WaveletOperation>, ProtoError> {
    let last = ops.len().saturating_sub(1);
    ops.iter()
        .enumerate()
        .map(|(i, op)| {
            let context = if i == last {
                WaveletOperationContext::with_hashed_version(
                    author.clone(),
                    application_timestamp,
                    1,
                    resulting_version.clone(),
                )
            } else {
                WaveletOperationContext::new(author.clone(), application_timestamp, 1)
            };
            deserialize_op(op, context)
        })
        .collect()
}

/// Converts a non-empty delta message to a transformed delta with the
/// given resulting version and application timestamp. The message's own
/// hashed version is ignored.
pub fn deserialize_transformed_delta(
    delta: &ProtocolWaveletDelta,
    resulting_version: HashedVersion,
    application_timestamp: i64,
) -> Result<TransformedWaveletDelta, ProtoError> {
    let author = ParticipantId::of(required(&delta.author, "ProtocolWaveletDelta.author")?)?;
    if delta.operation.is_empty() {
        return Err(invalid(
            "ProtocolWaveletDelta.operation",
            "Cannot deserialize an empty delta",
        ));
    }
    let ops = deserialize_transformed_ops(
        &delta.operation,
        &author,
        &resulting_version,
        application_timestamp,
    )?;
    Ok(TransformedWaveletDelta::new(
        author,
        resulting_version,
        application_timestamp,
        ops,
    )?)
}

// Delta store.

/// Converts a transformed delta to its storage message.
pub fn serialize_stored_delta(
    delta: &TransformedWaveletDelta,
) -> Result<ProtoTransformedWaveletDelta, ProtoError> {
    Ok(ProtoTransformedWaveletDelta {
        author: Some(delta.author().address().to_owned()),
        resulting_version: Some(serialize_hashed_version(delta.resulting_version())?),
        application_timestamp: Some(delta.application_timestamp()),
        operation: serialize_ops(delta)?,
    })
}

/// Converts a storage message to a transformed delta.
pub fn deserialize_stored_delta(
    delta: &ProtoTransformedWaveletDelta,
) -> Result<TransformedWaveletDelta, ProtoError> {
    let application_timestamp = *required(
        &delta.application_timestamp,
        "ProtoTransformedWaveletDelta.application_timestamp",
    )?;
    let resulting_version = deserialize_hashed_version(required(
        &delta.resulting_version,
        "ProtoTransformedWaveletDelta.resulting_version",
    )?)?;
    let author = ParticipantId::of(required(
        &delta.author,
        "ProtoTransformedWaveletDelta.author",
    )?)?;
    let ops = deserialize_transformed_ops(
        &delta.operation,
        &author,
        &resulting_version,
        application_timestamp,
    )?;
    Ok(TransformedWaveletDelta::new(
        author,
        resulting_version,
        application_timestamp,
        ops,
    )?)
}

// Snapshots.

/// Converts a document and its metadata to a snapshot message.
pub fn serialize_document(document: &BlipData) -> Result<DocumentSnapshot, ProtoError> {
    let version = i64::try_from(document.last_modified_version()).map_err(|_| {
        invalid(
            "DocumentSnapshot.last_modified_version",
            "exceeds the range of int64",
        )
    })?;
    Ok(DocumentSnapshot {
        document_id: Some(document.id().to_owned()),
        document_operation: Some(serialize_doc_op(document.content().as_operation().as_op())?),
        author: Some(document.author().address().to_owned()),
        contributor: document
            .contributors()
            .iter()
            .map(|p| p.address().to_owned())
            .collect(),
        last_modified_version: Some(version),
        last_modified_time: Some(document.last_modified_time()),
    })
}

/// Converts a wavelet to a snapshot message at `hashed_version`. Documents
/// are listed in id order (the original lists them in hash-map order).
pub fn serialize_wavelet(
    wavelet: &WaveletData,
    hashed_version: &HashedVersion,
) -> Result<WaveletSnapshot, ProtoError> {
    Ok(WaveletSnapshot {
        wavelet_id: Some(ModernIdSerialiser.serialise_wavelet_id(wavelet.wavelet_id())?),
        participant_id: wavelet
            .participants()
            .iter()
            .map(|p| p.address().to_owned())
            .collect(),
        document: wavelet
            .documents()
            .map(serialize_document)
            .collect::<Result<_, _>>()?,
        version: Some(serialize_hashed_version(hashed_version)?),
        last_modified_time: Some(wavelet.last_modified_time()),
        creator: Some(wavelet.creator().address().to_owned()),
        creation_time: Some(wavelet.creation_time()),
    })
}

/// Converts a snapshot message of a wavelet in the wave `wave_id` to
/// wavelet data. All addresses are validated.
pub fn deserialize_wavelet(
    snapshot: &WaveletSnapshot,
    wave_id: &WaveId,
) -> Result<WaveletData, ProtoError> {
    let creator = ParticipantId::of(required(&snapshot.creator, "WaveletSnapshot.creator")?)?;
    let wavelet_id = ModernIdSerialiser.deserialise_wavelet_id(required(
        &snapshot.wavelet_id,
        "WaveletSnapshot.wavelet_id",
    )?)?;
    let creation_time = *required(&snapshot.creation_time, "WaveletSnapshot.creation_time")?;
    let version =
        deserialize_hashed_version(required(&snapshot.version, "WaveletSnapshot.version")?)?;
    let last_modified_time = *required(
        &snapshot.last_modified_time,
        "WaveletSnapshot.last_modified_time",
    )?;
    let mut wavelet = WaveletData::empty(
        &WaveletName::of(wave_id.clone(), wavelet_id),
        creator,
        version,
        creation_time,
    );
    for participant in &snapshot.participant_id {
        wavelet.add_participant(ParticipantId::of(participant)?);
    }
    for document in &snapshot.document {
        let op = deserialize_doc_op(required(
            &document.document_operation,
            "DocumentSnapshot.document_operation",
        )?)?;
        let content = as_initialization(op)?;
        let contributors = document
            .contributor
            .iter()
            .map(|p| ParticipantId::of(p))
            .collect::<Result<Vec<_>, _>>()?;
        let last_modified_version = *required(
            &document.last_modified_version,
            "DocumentSnapshot.last_modified_version",
        )?;
        wavelet.create_document(
            required(&document.document_id, "DocumentSnapshot.document_id")?,
            ParticipantId::of(required(&document.author, "DocumentSnapshot.author")?)?,
            contributors,
            &content,
            *required(
                &document.last_modified_time,
                "DocumentSnapshot.last_modified_time",
            )?,
            u64::try_from(last_modified_version).map_err(|_| {
                invalid("DocumentSnapshot.last_modified_version", "negative version")
            })?,
        )?;
    }
    wavelet.set_last_modified_time(last_modified_time);
    Ok(wavelet)
}

// Applied deltas.

/// A message together with the exact bytes it was parsed from or serialised
/// to (`ByteStringMessage`). Hashes and signatures are computed over these
/// bytes, which re-encoding the message need not reproduce. Equality
/// compares the bytes only.
#[derive(Debug, Clone)]
pub struct ByteStringMessage<M> {
    bytes: Vec<u8>,
    message: M,
}

impl<M: WaveMessage> ByteStringMessage<M> {
    /// Parses a message, keeping the bytes.
    pub fn parse_from(bytes: impl Into<Vec<u8>>) -> Result<Self, ProtoError> {
        let bytes = bytes.into();
        let message = decode(&bytes)?;
        Ok(ByteStringMessage { bytes, message })
    }

    /// Serialises a message, keeping the bytes.
    pub fn serialize_message(message: M) -> Result<Self, ProtoError> {
        let bytes = super::encode(&message)?;
        Ok(ByteStringMessage { bytes, message })
    }

    pub fn message(&self) -> &M {
        &self.message
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl<M> PartialEq for ByteStringMessage<M> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl<M> Eq for ByteStringMessage<M> {}

/// The version an applied delta was applied at: the one recorded in the
/// applied delta if present, otherwise the target version of the original
/// delta (`AppliedDeltaUtil.getHashedVersionAppliedAt`).
pub fn get_hashed_version_applied_at(
    applied_delta: &ByteStringMessage<ProtocolAppliedWaveletDelta>,
) -> Result<HashedVersion, ProtoError> {
    let message = applied_delta.message();
    if let Some(version) = &message.hashed_version_applied_at {
        return deserialize_hashed_version(version);
    }
    let signed = required(
        &message.signed_original_delta,
        "ProtocolAppliedWaveletDelta.signed_original_delta",
    )?;
    let delta: ProtocolWaveletDelta =
        decode(required(&signed.delta, "ProtocolSignedDelta.delta")?)?;
    deserialize_hashed_version(required(
        &delta.hashed_version,
        "ProtocolWaveletDelta.hashed_version",
    )?)
}

fn operations_applied(message: &ProtocolAppliedWaveletDelta) -> Result<u32, ProtoError> {
    const FIELD: &str = "ProtocolAppliedWaveletDelta.operations_applied";
    u32::try_from(*required(&message.operations_applied, FIELD)?)
        .map_err(|_| invalid(FIELD, "negative operation count"))
}

/// The hashed version after an applied delta: the applied-at version
/// advanced by the number of operations applied, with the history hash
/// extended by the applied delta's bytes
/// (`AppliedDeltaUtil.calculateResultingHashedVersion`).
pub fn calculate_resulting_hashed_version(
    applied_delta: &ByteStringMessage<ProtocolAppliedWaveletDelta>,
) -> Result<HashedVersion, ProtoError> {
    Ok(HashedVersionFactory::create(
        applied_delta.bytes(),
        &get_hashed_version_applied_at(applied_delta)?,
        operations_applied(applied_delta.message())?,
    ))
}

/// Builds and serialises the applied delta for a signed delta applied at
/// `applied_at_version` (`AppliedDeltaUtil.buildAppliedDelta`). The
/// applied-at version is always recorded.
pub fn build_applied_delta(
    signed_delta: ProtocolSignedDelta,
    applied_at_version: &HashedVersion,
    operations_applied: u32,
    application_timestamp: i64,
) -> Result<ByteStringMessage<ProtocolAppliedWaveletDelta>, ProtoError> {
    build_applied_delta_in_epoch(
        signed_delta,
        applied_at_version,
        operations_applied,
        application_timestamp,
        0,
        None,
    )
}

/// [`build_applied_delta`] in host epoch `epoch` (recorded unless 0), with
/// `rebased` operations for a rebased orphan (see
/// `ProtocolAppliedWaveletDelta::rebased_operations`).
pub fn build_applied_delta_in_epoch(
    signed_delta: ProtocolSignedDelta,
    applied_at_version: &HashedVersion,
    operations_applied: u32,
    application_timestamp: i64,
    epoch: u64,
    rebased: Option<ProtocolWaveletDelta>,
) -> Result<ByteStringMessage<ProtocolAppliedWaveletDelta>, ProtoError> {
    let operations_applied = i32::try_from(operations_applied).map_err(|_| {
        invalid(
            "ProtocolAppliedWaveletDelta.operations_applied",
            "exceeds the range of int32",
        )
    })?;
    ByteStringMessage::serialize_message(ProtocolAppliedWaveletDelta {
        signed_original_delta: Some(signed_delta),
        hashed_version_applied_at: Some(serialize_hashed_version(applied_at_version)?),
        operations_applied: Some(operations_applied),
        application_timestamp: Some(application_timestamp),
        epoch: (epoch != 0).then_some(epoch),
        rebased_operations: rebased,
    })
}

/// The host epoch an applied delta was applied in; 0 if it records none.
pub fn applied_delta_epoch(applied_delta: &ByteStringMessage<ProtocolAppliedWaveletDelta>) -> u64 {
    applied_delta.message().epoch.unwrap_or(0)
}

/// Builds the transformed delta recorded for an applied delta: the
/// operations of `transformed` with the resulting version computed from the
/// applied delta's bytes (`AppliedDeltaUtil.buildTransformedDelta`).
/// `transformed` must target the applied-at version and have as many
/// operations as were applied.
pub fn build_transformed_delta(
    applied_delta: &ByteStringMessage<ProtocolAppliedWaveletDelta>,
    transformed: &WaveletDelta,
) -> Result<TransformedWaveletDelta, ProtoError> {
    let message = applied_delta.message();
    if &get_hashed_version_applied_at(applied_delta)? != transformed.target_version() {
        return Err(invalid(
            "ProtocolAppliedWaveletDelta.hashed_version_applied_at",
            "differs from the target version of the transformed delta",
        ));
    }
    let count = operations_applied(message)?;
    if count as usize != transformed.len() {
        return Err(invalid(
            "ProtocolAppliedWaveletDelta.operations_applied",
            "differs from the size of the transformed delta",
        ));
    }
    let resulting_version =
        HashedVersionFactory::create(applied_delta.bytes(), transformed.target_version(), count);
    let application_timestamp = *required(
        &message.application_timestamp,
        "ProtocolAppliedWaveletDelta.application_timestamp",
    )?;
    Ok(TransformedWaveletDelta::from_delta(
        resulting_version,
        application_timestamp,
        transformed,
    )?)
}
