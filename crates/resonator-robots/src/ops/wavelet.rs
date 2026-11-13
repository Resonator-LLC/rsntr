//! Operations on a wavelet as a whole (`ParticipantServices`,
//! `CreateWaveletService`, `WaveletSetTitleService`).

use resonator_model::conversation::{WaveletBasedConversation, make_wavelet_conversational};
use resonator_model::id::{IdGenerator, ParticipantId, WaveletName};

use super::context::{OperationContext, RobotWavelet};
use crate::api::{
    Event, EventProperties, InvalidRequest, OperationRequest, OperationType, ParamsProperty,
    WaveletData,
};
use crate::convert::{
    deserialise_wave_id, deserialise_wavelet_id, root_blip_id, serialise_wave_id,
    serialise_wavelet_id,
};

fn invalid(error: impl std::fmt::Display) -> InvalidRequest {
    InvalidRequest::new(error.to_string())
}

/// `wavelet.addParticipant` and `wavelet.removeParticipant`.
pub(super) async fn participant(
    operation: &OperationRequest,
    operation_type: OperationType,
    context: &mut OperationContext<'_>,
    participant: &ParticipantId,
) -> Result<(), InvalidRequest> {
    let name = context
        .open_operation_wavelet(operation, participant)
        .await?;
    let now = context.now();
    let event = {
        let mut conversation = context.conversation(&name, participant)?;
        let target_address = operation.required_string(ParamsProperty::ParticipantId)?;
        let target = ParticipantId::of(&target_address).map_err(|_| {
            InvalidRequest::new(format!(
                "Target ParticipantId {target_address} is not valid"
            ))
        })?;
        let root_blip_id = root_blip_id(&conversation.read()?);
        let is_member = conversation.wavelet.participants().contains(&target);
        let mut added = Vec::new();
        let mut removed = Vec::new();
        match operation_type {
            OperationType::WaveletAddParticipantNewsyntax => {
                if is_member {
                    return Err(InvalidRequest::new(format!(
                        "{} is already a member of wavelet",
                        target.address()
                    )));
                }
                conversation
                    .conversation()?
                    .add_participant(&target)
                    .map_err(invalid)?;
                added.push(target.address().to_owned());
            }
            OperationType::WaveletRemoveParticipantNewsyntax => {
                if !is_member {
                    return Err(InvalidRequest::new(format!(
                        "{} is not a member of wavelet",
                        target.address()
                    )));
                }
                conversation
                    .conversation()?
                    .remove_participant(&target)
                    .map_err(invalid)?;
                removed.push(target.address().to_owned());
            }
            other => {
                return Err(InvalidRequest::new(format!(
                    "This OperationService does not implement operation of type {}",
                    other.method()
                )));
            }
        }
        Event::new(
            participant.address(),
            now,
            Some(&root_blip_id),
            EventProperties::WaveletParticipantsChanged {
                participants_added: added,
                participants_removed: removed,
            },
        )
    };
    context.process_event(operation, &event);
    Ok(())
}

/// `robot.createWavelet` (and `wavelet.create` for passive robots): a new
/// wave with a conversation, an empty root blip, and the creator and the
/// given participants.
pub(super) async fn create(
    operation: &OperationRequest,
    context: &mut OperationContext<'_>,
    participant: &ParticipantId,
) -> Result<(), InvalidRequest> {
    let wavelet_data: WaveletData = operation.required(ParamsProperty::WaveletData)?;
    let mut participants = vec![participant.clone()];
    for address in &wavelet_data.participants {
        participants.push(ParticipantId::of(address).map_err(|_| {
            InvalidRequest::new(format!("{address} is not a valid participant address"))
        })?);
    }
    // The ids the client uses for the new wavelet, usually temporary ones.
    let client_ids = match (
        wavelet_data
            .wave_id
            .as_deref()
            .and_then(deserialise_wave_id),
        wavelet_data
            .wavelet_id
            .as_deref()
            .and_then(deserialise_wavelet_id),
    ) {
        (Some(wave_id), Some(wavelet_id)) => (wave_id, wavelet_id),
        _ => return Err(InvalidRequest::new("Invalid id")),
    };
    let now = context.now();
    let (new_wavelet, name, root_blip) = {
        let mut ids = context.executor.ids();
        // ConversationUtil.generateWaveletName
        let name = WaveletName::of(
            ids.new_wave_id().map_err(invalid)?,
            ids.new_conversation_root_wavelet_id().map_err(invalid)?,
        );
        let mut new_wavelet = RobotWavelet::empty(participant, &name, now);
        let wavelet = new_wavelet.op_based_wavelet(participant);
        make_wavelet_conversational(wavelet).map_err(invalid)?;
        let root_blip = WaveletBasedConversation::new(wavelet, &mut *ids)
            .map_err(invalid)?
            .append_blip("")
            .map_err(invalid)?;
        for new_participant in &participants {
            wavelet.add_participant(new_participant).map_err(invalid)?;
        }
        (new_wavelet, name, root_blip)
    };
    context.put_wavelet(&client_ids.0, &client_ids.1, new_wavelet)?;
    context.touch(&name, participant);
    // The new wavelet is in the context now; remember the root blip under
    // the client's id for it.
    context
        .conversation(&name, participant)?
        .put_blip(wavelet_data.root_blip_id.as_deref(), &root_blip);
    let event = Event::new(
        participant.address(),
        now,
        Some(&root_blip),
        EventProperties::WaveletCreated {
            message: operation.string(ParamsProperty::Message),
            wave_id: Some(serialise_wave_id(&name.wave_id)),
            wavelet_id: Some(serialise_wavelet_id(&name.wavelet_id)),
        },
    );
    context.process_event(operation, &event);
    Ok(())
}

/// `wavelet.setTitle`: an explicit title on the root blip.
pub(super) async fn set_title(
    operation: &OperationRequest,
    context: &mut OperationContext<'_>,
    participant: &ParticipantId,
) -> Result<(), InvalidRequest> {
    let title = operation.required_string(ParamsProperty::WaveletTitle)?;
    let name = context
        .open_operation_wavelet(operation, participant)
        .await?;
    let mut conversation = context.conversation(&name, participant)?;
    if conversation.read()?.root_thread().first_blip().is_none() {
        return Err(InvalidRequest::new(
            "The wavelet has no root blip to carry a title",
        ));
    }
    conversation
        .conversation()?
        .set_explicit_title(&title)
        .map_err(invalid)
}
