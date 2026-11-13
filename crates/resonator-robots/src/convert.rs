//! Conversion of wavelets and blips to their API form, and the choice of
//! blips that accompany events.
//!
//! Derived from `com.google.wave.api.data.converter` of Apache Wave
//! (`EventDataConverterV21`, `EventDataConverterV22`, `ContextResolver`,
//! `EventDataConverterManager`), licensed under the Apache License,
//! Version 2.0.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use resonator_model::conversation::{Conversation, ConversationBlip, ConversationThread};
use resonator_model::data::WaveletData as ModelWaveletData;
use resonator_model::doc::{AnnotationReader, DocView};
use resonator_model::id::constants::ROLES_DATA_DOC_ID;
use resonator_model::id::util::{is_robot_doc_id, split};
use resonator_model::id::{DualIdSerialiser, IdSerialiser, WaveId, WaveletId};

use crate::api::{
    Annotation, BlipData, BlipThread, Context, EventMessageBundle, ProtocolVersion, Range,
    WaveletData,
};
use crate::view::{self, ApiView, AttachmentContext};

/// A wave id as the API writes it: `domain/id` (`ApiIdSerializer`).
pub fn serialise_wave_id(id: &WaveId) -> String {
    DualIdSerialiser::MODERN
        .serialise_wave_id(id)
        .unwrap_or_else(|_| format!("{}/{}", id.domain(), id.id()))
}

/// A wavelet id as the API writes it: `domain/id`.
pub fn serialise_wavelet_id(id: &WaveletId) -> String {
    DualIdSerialiser::MODERN
        .serialise_wavelet_id(id)
        .unwrap_or_else(|_| format!("{}/{}", id.domain(), id.id()))
}

/// Reads a wave id in the modern (`domain/id`) or legacy (`domain!id`)
/// form.
pub fn deserialise_wave_id(serialised: &str) -> Option<WaveId> {
    DualIdSerialiser::MODERN
        .deserialise_wave_id(serialised)
        .ok()
}

/// Reads a wavelet id in the modern or legacy form.
pub fn deserialise_wavelet_id(serialised: &str) -> Option<WaveletId> {
    DualIdSerialiser::MODERN
        .deserialise_wavelet_id(serialised)
        .ok()
}

/// How wavelets and blips are presented, which depends on the protocol
/// version (`EventDataConverter`): from 0.22 on, with threads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EventDataConverter {
    /// Protocols up to 0.21: blips form a tree by parent and children,
    /// where the next blip of a thread is a child of the one before.
    V21,
    /// Protocol 0.22: blips are in threads.
    #[default]
    V22,
}

/// The id of the first blip of the root thread, or the empty string
/// (`ConversationUtil.getRootBlipId`).
pub fn root_blip_id(conversation: &Conversation<'_>) -> String {
    conversation
        .root_thread()
        .first_blip()
        .map(|blip| blip.id().to_owned())
        .unwrap_or_default()
}

impl EventDataConverter {
    /// The converter for a protocol version
    /// (`EventDataConverterManager.getEventDataConverter`).
    pub fn for_version(version: ProtocolVersion) -> Self {
        if version >= ProtocolVersion::V2_2 {
            EventDataConverter::V22
        } else {
            EventDataConverter::V21
        }
    }

    /// The API form of a conversational wavelet (`toWaveletData`).
    ///
    /// The original fails if the conversation has no blips; here the root
    /// blip id is then absent.
    pub fn to_wavelet_data(
        &self,
        wavelet: &ModelWaveletData,
        conversation: &Conversation<'_>,
    ) -> WaveletData {
        let mut data = WaveletData {
            creation_time: wavelet.creation_time(),
            creator: Some(wavelet.creator().address().to_owned()),
            wave_id: Some(serialise_wave_id(wavelet.wave_id())),
            wavelet_id: Some(serialise_wavelet_id(wavelet.wavelet_id())),
            last_modified_time: wavelet.last_modified_time(),
            participants: addresses(wavelet.participants()),
            root_blip_id: conversation
                .root_thread()
                .first_blip()
                .map(|blip| blip.id().to_owned()),
            title: Some(conversation.title()),
            version: wavelet.version() as i64,
            ..WaveletData::default()
        };
        for document in wavelet.documents() {
            if is_robot_doc_id(document.id())
                && let Some(parts) = split(document.id())
                && parts.len() == 2
            {
                data.data_documents
                    .insert(parts[1].clone(), document.content().to_xml_string());
            }
        }
        data.tags = conversation.tags();
        // The roles document: <assign address=".." role=".."/>; the last
        // assignment of a participant counts, and the default is FULL.
        let mut assigned: BTreeMap<String, String> = BTreeMap::new();
        if let Some(roles) = wavelet.document(ROLES_DATA_DOC_ID) {
            let view = DocView::new(roles.content());
            for element in view.child_elements(view.root()) {
                if view.has_tag(element, "assign")
                    && let Some(address) = view.attribute(element, "address")
                {
                    match view.attribute(element, "role") {
                        Some(role @ ("FULL" | "READ_ONLY")) => {
                            assigned.insert(address.to_owned(), role.to_owned());
                        }
                        _ => {
                            assigned.remove(address);
                        }
                    }
                }
            }
        }
        for participant in wavelet.participants() {
            let role = assigned
                .get(participant.address())
                .map_or("FULL", String::as_str);
            data.participant_roles
                .insert(participant.address().to_owned(), role.to_owned());
        }
        if *self == EventDataConverter::V22 {
            data.root_thread = Some(BlipThread::new(
                "",
                -1,
                blip_ids(conversation.root_thread()),
            ));
        }
        data
    }

    /// The API form of a blip (`toBlipData`). With the 0.22 converter, the
    /// blip's own thread (unless it is the root thread) and its reply
    /// threads are added to the bundle if not there yet.
    pub fn to_blip_data(
        &self,
        blip: &ConversationBlip,
        conversation: &Conversation<'_>,
        bundle: &mut EventMessageBundle,
    ) -> BlipData {
        let wavelet = conversation.wavelet();
        let attachments = AttachmentContext::of(wavelet);
        let mut data = BlipData {
            blip_id: Some(blip.id().to_owned()),
            parent_blip_id: self
                .find_blip_parent(conversation, blip)
                .map(|parent| parent.id().to_owned()),
            wave_id: Some(serialise_wave_id(wavelet.wave_id())),
            wavelet_id: Some(serialise_wavelet_id(wavelet.wavelet_id())),
            child_blip_ids: self
                .find_blip_children(conversation, blip)
                .into_iter()
                .map(|child| child.id().to_owned())
                .collect(),
            ..BlipData::default()
        };
        if let Some(blip_data) = conversation.blip_data(blip.id()) {
            data.creator = Some(blip_data.author().address().to_owned());
            data.contributors = addresses(blip_data.contributors());
            data.last_modified_time = blip_data.last_modified_time();
            data.version = blip_data.last_modified_version() as i64;
            let doc = blip_data.content();
            let doc_view = DocView::new(doc);
            let api_view = ApiView::new(&doc_view, &attachments);
            data.content = api_view.api_contents();
            for annotation in doc.ranged_annotations(0, doc_view.size(), None) {
                if let Some(value) = annotation.value {
                    data.annotations.push(Annotation {
                        name: Some(annotation.key),
                        value: Some(value),
                        range: Range::new(
                            api_view.transform_to_text_offset(annotation.start) as i32,
                            api_view.transform_to_text_offset(annotation.end) as i32,
                        ),
                    });
                }
            }
            data.elements = view::serialize(&doc_view, &attachments);
        }
        if *self == EventDataConverter::V22 {
            if let Some(thread) = conversation.thread_of(blip.id()) {
                data.thread_id = Some(thread.id().to_owned());
                if !thread.id().is_empty() {
                    add_thread(bundle, conversation, thread, None, &attachments);
                }
            }
            for located in conversation.locate_reply_threads(blip) {
                data.reply_thread_ids.push(located.thread.id().to_owned());
                add_thread(
                    bundle,
                    conversation,
                    located.thread,
                    located.location,
                    &attachments,
                );
            }
        }
        data
    }

    /// The blips the API calls the children of a blip
    /// (`findBlipChildren`): with threads, all blips of all reply threads;
    /// without, the next blip of the thread and the first blip of every
    /// reply thread.
    pub fn find_blip_children<'c>(
        &self,
        conversation: &'c Conversation<'_>,
        blip: &ConversationBlip,
    ) -> Vec<&'c ConversationBlip> {
        let Some(blip) = conversation.blip(blip.id()) else {
            return Vec::new();
        };
        match self {
            EventDataConverter::V22 => conversation
                .locate_reply_threads(blip)
                .into_iter()
                .flat_map(|located| located.thread.blips())
                .collect(),
            EventDataConverter::V21 => next_sibling(conversation, blip)
                .into_iter()
                .chain(
                    blip.reply_threads()
                        .iter()
                        .filter_map(ConversationThread::first_blip),
                )
                .collect(),
        }
    }

    /// The blip the API calls the parent of a blip (`findBlipParent`):
    /// with threads, the blip the thread hangs off; without, the previous
    /// blip of the thread, or for the first blip of a reply thread the
    /// blip the thread hangs off.
    pub fn find_blip_parent<'c>(
        &self,
        conversation: &'c Conversation<'_>,
        blip: &ConversationBlip,
    ) -> Option<&'c ConversationBlip> {
        let thread = conversation.thread_of(blip.id())?;
        match self {
            EventDataConverter::V22 => conversation.parent_blip_of(thread.id()),
            EventDataConverter::V21 => {
                let is_first = thread.first_blip().is_some_and(|b| b.id() == blip.id());
                if is_first && !thread.id().is_empty() {
                    conversation.parent_blip_of(thread.id())
                } else {
                    previous_sibling(conversation, blip)
                }
            }
        }
    }
}

fn addresses(participants: &[resonator_model::id::ParticipantId]) -> Vec<String> {
    participants
        .iter()
        .map(|p| p.address().to_owned())
        .collect()
}

fn blip_ids(thread: &ConversationThread) -> Vec<String> {
    thread.blips().iter().map(|b| b.id().to_owned()).collect()
}

fn previous_sibling<'c>(
    conversation: &'c Conversation<'_>,
    blip: &ConversationBlip,
) -> Option<&'c ConversationBlip> {
    let blips = conversation.thread_of(blip.id())?.blips();
    let position = blips.iter().position(|b| b.id() == blip.id())?;
    position.checked_sub(1).and_then(|i| blips.get(i))
}

fn next_sibling<'c>(
    conversation: &'c Conversation<'_>,
    blip: &ConversationBlip,
) -> Option<&'c ConversationBlip> {
    let blips = conversation.thread_of(blip.id())?.blips();
    let position = blips.iter().position(|b| b.id() == blip.id())?;
    blips.get(position + 1)
}

/// `EventDataConverterV22.addThread`: `location` is the document location
/// of the thread's anchor in its parent blip if known.
fn add_thread(
    bundle: &mut EventMessageBundle,
    conversation: &Conversation<'_>,
    thread: &ConversationThread,
    location: Option<usize>,
    attachments: &AttachmentContext,
) {
    if bundle.has_thread_id(thread.id()) {
        return;
    }
    let parent = conversation.parent_blip_of(thread.id());
    let location = location.or_else(|| {
        conversation
            .locate_reply_threads(parent?)
            .into_iter()
            .find(|located| located.thread.id() == thread.id())?
            .location
    });
    let api_location = match (
        location,
        parent.and_then(|p| conversation.blip_content(p.id())),
    ) {
        (Some(location), Some(content)) => {
            let view = ApiView::new(&DocView::new(content), attachments);
            view.transform_to_text_offset(location) as i32
        }
        _ => -1,
    };
    bundle.threads.insert(
        thread.id().to_owned(),
        BlipThread::new(thread.id(), api_location, blip_ids(thread)),
    );
}

fn add_blip(
    bundle: &mut EventMessageBundle,
    conversation: &Conversation<'_>,
    blip: &ConversationBlip,
    converter: EventDataConverter,
) {
    if !bundle.has_blip_id(blip.id()) {
        let data = converter.to_blip_data(blip, conversation, bundle);
        bundle.blips.insert(blip.id().to_owned(), data);
    }
}

/// Adds every blip of the conversation to the bundle, breadth first
/// (`ContextResolver.addAllBlipsToEventMessages`).
pub fn add_all_blips(
    bundle: &mut EventMessageBundle,
    conversation: &Conversation<'_>,
    converter: EventDataConverter,
) {
    let mut threads = VecDeque::from([conversation.root_thread()]);
    while let Some(thread) = threads.pop_front() {
        for blip in thread.blips() {
            add_blip(bundle, conversation, blip, converter);
            threads.extend(blip.reply_threads());
        }
    }
}

/// Sets the bundle's wavelet data and adds the blips that the events
/// require, each with the surrounding blips its contexts ask for
/// (`ContextResolver.resolveContext`).
pub fn resolve_context(
    bundle: &mut EventMessageBundle,
    required_blips: &BTreeMap<String, BTreeSet<Context>>,
    conversation: &Conversation<'_>,
    converter: EventDataConverter,
) {
    bundle.wavelet = Some(converter.to_wavelet_data(conversation.wavelet(), conversation));
    for (blip_id, contexts) in required_blips {
        if contexts.contains(&Context::All) {
            add_all_blips(bundle, conversation, converter);
            break;
        }
        let required = conversation.blip(blip_id);
        if contexts.contains(&Context::Root)
            && let Some(root) = conversation.root_thread().first_blip()
            && required.is_none_or(|r| r.id() != root.id())
        {
            add_blip(bundle, conversation, root, converter);
        }
        let Some(required) = required else {
            continue;
        };
        add_blip(bundle, conversation, required, converter);
        if contexts.contains(&Context::Children) {
            for child in converter.find_blip_children(conversation, required) {
                add_blip(bundle, conversation, child, converter);
            }
        }
        if contexts.contains(&Context::Parent)
            && let Some(parent) = converter.find_blip_parent(conversation, required)
        {
            add_blip(bundle, conversation, parent, converter);
        }
        if contexts.contains(&Context::Siblings)
            && let Some(thread) = conversation.thread_of(required.id())
        {
            for sibling in thread.blips() {
                if sibling.id() != required.id() {
                    add_blip(bundle, conversation, sibling, converter);
                }
            }
        }
    }
}
