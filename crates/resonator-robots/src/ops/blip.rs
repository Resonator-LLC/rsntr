//! Operations that create, extend and delete blips
//! (`BlipOperationServices`).

use std::collections::BTreeMap;

use resonator_model::doc::line_containers;
use resonator_model::doc::{DocView, XmlFragment};
use resonator_model::docop::Attributes;
use resonator_model::id::ParticipantId;

use super::context::{OpenConversation, OperationContext};
use super::porting::coerce_blip_text;
use crate::api::{
    BlipData, Element, Event, EventProperties, InvalidRequest, OperationRequest, OperationType,
    ParamsProperty,
};
use crate::convert::root_blip_id;
use crate::view::{ApiView, AttachmentContext};

fn invalid(error: impl std::fmt::Display) -> InvalidRequest {
    InvalidRequest::new(error.to_string())
}

pub(super) async fn execute(
    operation: &OperationRequest,
    operation_type: OperationType,
    context: &mut OperationContext<'_>,
    participant: &ParticipantId,
) -> Result<(), InvalidRequest> {
    let name = context
        .open_operation_wavelet(operation, participant)
        .await?;
    let now = context.now();
    let mut conversation = context.conversation(&name, participant)?;
    let event = match operation_type {
        OperationType::BlipContinueThread => continue_thread(operation, &mut conversation)?,
        OperationType::BlipCreateChild => create_child(operation, &mut conversation)?,
        OperationType::WaveletAppendBlip => append_blip(operation, &mut conversation)?,
        OperationType::DocumentAppendInlineBlip => {
            append_inline_blip(operation, &mut conversation)?
        }
        OperationType::DocumentAppendMarkup => append_markup(operation, &mut conversation)?,
        OperationType::DocumentInsertInlineBlip => {
            insert_inline_blip(operation, &mut conversation)?
        }
        OperationType::DocumentInsertInlineBlipAfterElement => {
            insert_inline_blip_after_element(operation, &mut conversation)?
        }
        OperationType::BlipDelete => delete(operation, &mut conversation)?,
        other => {
            return Err(InvalidRequest::new(format!(
                "This OperationService does not implement operation of type {}",
                other.method()
            )));
        }
    };
    let response = match event {
        Some(new_blip_id) => {
            // processBlipCreatedEvent
            let root = root_blip_id(&conversation.read()?);
            Some(Event::new(
                participant.address(),
                now,
                Some(&root),
                EventProperties::WaveletBlipCreated {
                    new_blip_id: Some(new_blip_id),
                },
            ))
        }
        None => None,
    };
    drop(conversation);
    match response {
        Some(event) => context.process_event(operation, &event),
        None => context.construct_response(operation, BTreeMap::new()),
    }
    Ok(())
}

fn blip_data(operation: &OperationRequest) -> Result<BlipData, InvalidRequest> {
    operation.required(ParamsProperty::BlipData)
}

/// Registers the new blip under the id the client gave it, fills it, and
/// returns its id.
fn finish_new_blip(
    conversation: &mut OpenConversation<'_>,
    blip_data: &BlipData,
    new_blip_id: String,
) -> Result<Option<String>, InvalidRequest> {
    conversation.put_blip(blip_data.blip_id.as_deref(), &new_blip_id);
    put_content_for_new_blip(conversation, &new_blip_id, &blip_data.content)?;
    Ok(Some(new_blip_id))
}

/// `putContentForNewBlip`: the client's text, without the newline it has
/// to start with, goes at the end of the new blip's only line.
fn put_content_for_new_blip(
    conversation: &mut OpenConversation<'_>,
    blip_id: &str,
    content: &str,
) -> Result<(), InvalidRequest> {
    let content = content.strip_prefix('\n').unwrap_or(content);
    let text = XmlFragment::text(&coerce_blip_text(content));
    conversation
        .conversation()?
        .edit_blip(blip_id, |doc| {
            line_containers::append_to_last_line(doc, &text)
        })
        .map_err(invalid)
}

fn continue_thread(
    operation: &OperationRequest,
    conversation: &mut OpenConversation<'_>,
) -> Result<Option<String>, InvalidRequest> {
    let blip_data = blip_data(operation)?;
    let parent_blip_id = operation.required_string(ParamsProperty::BlipId)?;
    let parent = conversation.blip_id(&parent_blip_id)?;
    let thread_id = conversation
        .read()?
        .thread_of(&parent)
        .map(|thread| thread.id().to_owned())
        .unwrap_or_default();
    let new_blip = conversation
        .conversation()?
        .append_blip(&thread_id)
        .map_err(invalid)?;
    finish_new_blip(conversation, &blip_data, new_blip)
}

fn create_child(
    operation: &OperationRequest,
    conversation: &mut OpenConversation<'_>,
) -> Result<Option<String>, InvalidRequest> {
    let blip_data = blip_data(operation)?;
    let parent_blip_id = operation.required_string(ParamsProperty::BlipId)?;
    let parent = conversation.blip_id(&parent_blip_id)?;
    let new_blip = {
        let mut conversation = conversation.conversation()?;
        let thread_id = conversation.add_reply_thread(&parent).map_err(invalid)?;
        conversation.append_blip(&thread_id).map_err(invalid)?
    };
    finish_new_blip(conversation, &blip_data, new_blip)
}

fn append_blip(
    operation: &OperationRequest,
    conversation: &mut OpenConversation<'_>,
) -> Result<Option<String>, InvalidRequest> {
    let blip_data = blip_data(operation)?;
    let new_blip = conversation
        .conversation()?
        .append_blip("")
        .map_err(invalid)?;
    finish_new_blip(conversation, &blip_data, new_blip)
}

/// Adds an inline reply thread with one blip at a location of the parent
/// blip's document.
fn add_inline_reply(
    conversation: &mut OpenConversation<'_>,
    parent: &str,
    location: usize,
) -> Result<String, InvalidRequest> {
    let mut conversation = conversation.conversation()?;
    let thread_id = conversation
        .add_inline_reply_thread(parent, location)
        .map_err(invalid)?;
    conversation.append_blip(&thread_id).map_err(invalid)
}

fn append_inline_blip(
    operation: &OperationRequest,
    conversation: &mut OpenConversation<'_>,
) -> Result<Option<String>, InvalidRequest> {
    let blip_data = blip_data(operation)?;
    let parent_blip_id = operation.required_string(ParamsProperty::BlipId)?;
    let parent = conversation.blip_id(&parent_blip_id)?;
    // A new, empty line at the end of the blip for the anchor.
    let location = conversation
        .conversation()?
        .edit_blip(&parent, |doc| {
            let line = line_containers::append_line(doc, None, Some(&Attributes::new()))?;
            Ok(doc.view().location_after(line))
        })
        .map_err(invalid)?;
    let new_blip = add_inline_reply(conversation, &parent, location)?;
    finish_new_blip(conversation, &blip_data, new_blip)
}

fn append_markup(
    operation: &OperationRequest,
    conversation: &mut OpenConversation<'_>,
) -> Result<Option<String>, InvalidRequest> {
    let content = operation.required_string(ParamsProperty::Content)?;
    let blip_id = operation.required_string(ParamsProperty::BlipId)?;
    let blip = conversation.blip_id(&blip_id)?;
    let markup = XmlFragment::parse(&content).map_err(invalid)?;
    conversation
        .conversation()?
        .edit_blip(&blip, |doc| {
            line_containers::append_line(doc, Some(&markup), Some(&Attributes::new())).map(drop)
        })
        .map_err(invalid)?;
    Ok(None)
}

/// The API view of a blip's current content.
fn api_view(conversation: &OpenConversation<'_>, blip_id: &str) -> ApiView {
    let wavelet = conversation.wavelet.data();
    ApiView::new(
        &DocView::new(conversation.wavelet.document(blip_id)),
        &AttachmentContext::of(wavelet),
    )
}

fn insert_inline_blip(
    operation: &OperationRequest,
    conversation: &mut OpenConversation<'_>,
) -> Result<Option<String>, InvalidRequest> {
    let blip_data = blip_data(operation)?;
    let parent_blip_id = operation.required_string(ParamsProperty::BlipId)?;
    let parent = conversation.blip_id(&parent_blip_id)?;
    let index = operation.required_integer(ParamsProperty::Index)?;
    if index <= 0 {
        return Err(InvalidRequest::new(format!(
            "Can't inline a blip on position <= 0, got {index}"
        )));
    }
    let location = api_view(conversation, &parent)
        .transform_to_xml_offset(index as usize)
        .ok_or_else(|| InvalidRequest::new("The blip has no content to inline a blip in"))?;
    let new_blip = add_inline_reply(conversation, &parent, location)?;
    finish_new_blip(conversation, &blip_data, new_blip)
}

fn insert_inline_blip_after_element(
    operation: &OperationRequest,
    conversation: &mut OpenConversation<'_>,
) -> Result<Option<String>, InvalidRequest> {
    let blip_data = blip_data(operation)?;
    let parent_blip_id = operation.required_string(ParamsProperty::BlipId)?;
    let parent = conversation.blip_id(&parent_blip_id)?;
    let element: Element = operation.required(ParamsProperty::Element)?;
    let view = api_view(conversation, &parent);
    let element_location = view
        .locate_element(&element)
        .ok_or_else(|| InvalidRequest::new("Requested element not found"))?;
    // Just after the element.
    let location = view
        .transform_to_xml_offset(element_location + 1)
        .ok_or_else(|| InvalidRequest::new("Requested element not found"))?;
    let new_blip = add_inline_reply(conversation, &parent, location)?;
    finish_new_blip(conversation, &blip_data, new_blip)
}

fn delete(
    operation: &OperationRequest,
    conversation: &mut OpenConversation<'_>,
) -> Result<Option<String>, InvalidRequest> {
    let blip_id = operation.required_string(ParamsProperty::BlipId)?;
    let blip = conversation.blip_id(&blip_id)?;
    conversation
        .conversation()?
        .delete_blip(&blip)
        .map_err(invalid)?;
    Ok(None)
}
