//! Export and import of wavelets for migration (`ExportSnapshotService`,
//! `ExportDeltasService`, `ExportAttachmentService`, `ImportDeltasService`,
//! `ImportAttachmentService`), and text coercion.

use std::collections::BTreeMap;

use resonator_model::docop::utf16::{
    BlipCodePointResult, REPLACEMENT_CHARACTER, is_code_point_good_for_blip,
};
use resonator_model::id::{ParticipantId, WaveId, WaveletId, WaveletName};
use resonator_model::proto::federation::{ProtocolHashedVersion, ProtocolWaveletDelta};
use resonator_model::proto::{
    decode, deserialize_hashed_version, encode, json::to_json, serialize_hashed_version,
    serialize_transformed_delta, serialize_wavelet,
};
use resonator_model::version::{HashedVersion, HashedVersionFactory};
use resonator_server::waveserver::WaveletProvider;
use serde_json::json;

use super::context::OperationContext;
use crate::api::{
    ByteArrays, InvalidRequest, OperationRequest, ParamsProperty, RawAttachmentData, base64_bytes,
};

/// Text as it may appear in a blip (`PermittedCharacters.BLIP_TEXT
/// .coerceString`): a tab becomes four spaces, a line break a space, and
/// any other character not allowed in a blip the replacement character.
/// Unlike the original this keeps characters outside the basic
/// multilingual plane.
pub fn coerce_blip_text(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\t' => result.push_str("    "),
            '\n' | '\r' => result.push(' '),
            c if is_code_point_good_for_blip(c) == BlipCodePointResult::Ok => result.push(c),
            _ => result.push(REPLACEMENT_CHARACTER),
        }
    }
    result
}

/// The server's maximum for the size of an export of deltas, after which
/// the client has to ask again from the version it got to
/// (`GET_HISTORY_BYTES_LENGTH_LIMIT`).
const GET_HISTORY_BYTES_LENGTH_LIMIT: usize = 1_000_000;

fn wavelet_name_of(
    context: &OperationContext<'_>,
    operation: &OperationRequest,
) -> Result<WaveletName, InvalidRequest> {
    let (wave_id, wavelet_id) = context.wavelet_ids_of(operation)?;
    Ok(WaveletName::of(wave_id, wavelet_id))
}

/// `robot.exportSnapshot`: the wavelet's current state as the JSON of a
/// `WaveletSnapshot` message, in a string.
pub(super) async fn export_snapshot(
    operation: &OperationRequest,
    context: &mut OperationContext<'_>,
    participant: &ParticipantId,
) -> Result<(), InvalidRequest> {
    let name = wavelet_name_of(context, operation)?;
    let snapshot = context
        .wavelet_snapshot(&name, participant)
        .await?
        .ok_or_else(|| InvalidRequest::new(format!("Wavelet {name} couldn't be retrieved")))?;
    let message = serialize_wavelet(&snapshot, snapshot.hashed_version())
        .and_then(|message| to_json(&message))
        .map_err(|e| InvalidRequest::new(e.to_string()))?;
    let data = BTreeMap::from([(ParamsProperty::RawSnapshot, json!(message))]);
    context.construct_response(operation, data);
    Ok(())
}

fn version_parameter(
    operation: &OperationRequest,
    property: ParamsProperty,
) -> Result<HashedVersion, InvalidRequest> {
    let bytes = operation.required_bytes(property)?;
    decode::<ProtocolHashedVersion>(&bytes)
        .and_then(|version| deserialize_hashed_version(&version))
        .map_err(|_| InvalidRequest::new(format!("Invalid version {}", property.key())))
}

fn version_bytes(version: &HashedVersion) -> Result<Vec<u8>, InvalidRequest> {
    serialize_hashed_version(version)
        .and_then(|message| encode(&message))
        .map_err(|e| InvalidRequest::new(e.to_string()))
}

/// `robot.exportDeltas`: the deltas between two versions as serialized
/// `ProtocolWaveletDelta` messages, up to a size limit, and the version
/// they lead to.
///
/// The original fails when both versions are the same or the server
/// cannot provide the history; here the first gives no deltas and the
/// start version, and the second is the operation's error.
pub(super) async fn export_deltas(
    operation: &OperationRequest,
    context: &mut OperationContext<'_>,
    participant: &ParticipantId,
) -> Result<(), InvalidRequest> {
    let name = wavelet_name_of(context, operation)?;
    let from_version = version_parameter(operation, ParamsProperty::FromVersion)?;
    let to_version = version_parameter(operation, ParamsProperty::ToVersion)?;
    if !context.check_access(&name, participant).await {
        return Err(InvalidRequest::new("Access rejected"));
    }
    if from_version.version() > to_version.version() {
        return Err(InvalidRequest::new("Invalid version range"));
    }
    let mut raw_deltas = Vec::new();
    let mut target_version = from_version.clone();
    if from_version != to_version {
        let deltas = context
            .executor
            .server
            .get_history(&name, &from_version, &to_version)
            .await
            .map_err(|e| InvalidRequest::new(e.to_string()))?;
        let mut length = 0;
        for delta in &deltas {
            let bytes = serialize_transformed_delta(delta)
                .and_then(|message| encode(&message))
                .map_err(|e| InvalidRequest::new(e.to_string()))?;
            length += bytes.len();
            raw_deltas.push(bytes);
            target_version = delta.resulting_version().clone();
            if length >= GET_HISTORY_BYTES_LENGTH_LIMIT {
                break;
            }
        }
    }
    let data = BTreeMap::from([
        (ParamsProperty::RawDeltas, json!(ByteArrays(raw_deltas))),
        (
            ParamsProperty::TargetVersion,
            json!(base64_bytes::encode(&version_bytes(&target_version)?)),
        ),
    ]);
    context.construct_response(operation, data);
    Ok(())
}

/// Checks the form of an attachment id: `<domain>/<id>` or `<id>`
/// (`AttachmentId.deserialise`).
fn attachment_id(operation: &OperationRequest) -> Result<String, InvalidRequest> {
    let id = operation.required_string(ParamsProperty::AttachmentId)?;
    if id.is_empty() || id.split('/').count() > 2 {
        return Err(InvalidRequest::new("Invalid id"));
    }
    Ok(id)
}

/// `robot.exportAttachment`.
pub(super) async fn export_attachment(
    operation: &OperationRequest,
    context: &mut OperationContext<'_>,
) -> Result<(), InvalidRequest> {
    let id = attachment_id(operation)?;
    match context.executor.attachments.export_attachment(&id).await {
        Ok(attachment) => {
            let data = BTreeMap::from([(ParamsProperty::AttachmentData, json!(attachment))]);
            context.construct_response(operation, data);
        }
        Err(message) => context.construct_error_response(operation, &message),
    }
    Ok(())
}

fn check_import_enabled(
    context: &OperationContext<'_>,
    operation: &OperationRequest,
) -> Result<(), InvalidRequest> {
    if context.executor.import_enabled {
        Ok(())
    } else {
        Err(InvalidRequest::new(format!(
            "{} is not enabled",
            operation.operation_type().method()
        )))
    }
}

/// The wavelet an import goes to: the ids of the request moved into this
/// server's domain.
fn local_wavelet_name(
    context: &OperationContext<'_>,
    operation: &OperationRequest,
) -> Result<WaveletName, InvalidRequest> {
    let (wave_id, wavelet_id) = context.wavelet_ids_of(operation)?;
    let domain = context.executor.server.domain();
    match (
        WaveId::of(domain, wave_id.id()),
        WaveletId::of(domain, wavelet_id.id()),
    ) {
        (Ok(wave_id), Ok(wavelet_id)) => Ok(WaveletName::of(wave_id, wavelet_id)),
        _ => Err(InvalidRequest::new("Invalid id")),
    }
}

/// `robot.importDeltas`: submits exported deltas, under their authors, to
/// the wavelet of the same id in this server's domain, starting with the
/// first delta that applies at the wavelet's current version. Deltas for
/// earlier versions are skipped, so that an interrupted import can be
/// repeated. Answers with the version the import started at, or -1 if
/// nothing was imported.
///
/// Only available if the executor allows imports.
pub(super) async fn import_deltas(
    operation: &OperationRequest,
    context: &mut OperationContext<'_>,
) -> Result<(), InvalidRequest> {
    check_import_enabled(context, operation)?;
    let name = local_wavelet_name(context, operation)?;
    let ByteArrays(history) = operation.required(ParamsProperty::RawDeltas)?;
    let server = &context.executor.server;
    let mut imported_from_version: i64 = -1;
    if !history.is_empty() {
        let mut current = match server.get_snapshot(&name).await {
            Ok(Some(snapshot)) => snapshot.snapshot.hashed_version().clone(),
            Ok(None) => HashedVersionFactory::create_version_zero(&name),
            Err(error) => {
                context.construct_error_response(operation, &error.to_string());
                return Ok(());
            }
        };
        for delta_bytes in &history {
            let mut delta: ProtocolWaveletDelta =
                decode(delta_bytes).map_err(|_| InvalidRequest::new("Parse delta"))?;
            let delta_version = delta
                .hashed_version
                .as_ref()
                .and_then(|version| version.version)
                .unwrap_or(0);
            let current_version = current.version() as i64;
            if current_version == delta_version {
                if imported_from_version == -1 {
                    imported_from_version = current_version;
                }
                // setVersionHash: the delta as exported has no hash.
                delta.hashed_version = Some(ProtocolHashedVersion {
                    version: Some(delta_version),
                    history_hash: Some(current.history_hash().to_vec()),
                });
                match server.submit_request(&name, delta).await {
                    Ok(result) => current = result.hashed_version_after_application,
                    Err(error) => {
                        context.construct_error_response(operation, &error.to_string());
                        return Ok(());
                    }
                }
            } else if imported_from_version != -1 {
                let message = format!(
                    "Expected wavelet version {delta_version}, but current version is \
                     {current_version}.Possibly wavelet is modified during import."
                );
                context.construct_error_response(operation, &message);
                return Ok(());
            }
        }
    }
    let data = BTreeMap::from([(
        ParamsProperty::ImportedFromVersion,
        json!(imported_from_version),
    )]);
    context.construct_response(operation, data);
    Ok(())
}

/// `robot.importAttachment`. Only available if the executor allows
/// imports.
pub(super) async fn import_attachment(
    operation: &OperationRequest,
    context: &mut OperationContext<'_>,
) -> Result<(), InvalidRequest> {
    check_import_enabled(context, operation)?;
    let (wave_id, wavelet_id) = context.wavelet_ids_of(operation)?;
    let name = WaveletName::of(wave_id, wavelet_id);
    let id = attachment_id(operation)?;
    let attachment: RawAttachmentData = operation.required(ParamsProperty::AttachmentData)?;
    let creator_address = attachment.creator.as_deref().unwrap_or("");
    let creator = ParticipantId::of(creator_address)
        .map_err(|_| InvalidRequest::new(format!("Invalid participant {creator_address}")))?;
    let stored = context
        .executor
        .attachments
        .import_attachment(
            &id,
            &name,
            attachment.file_name.as_deref().unwrap_or(""),
            &creator,
            &attachment.data,
        )
        .await;
    match stored {
        Ok(()) => context.construct_response(operation, BTreeMap::new()),
        Err(message) => context.construct_error_response(operation, &message),
    }
    Ok(())
}
