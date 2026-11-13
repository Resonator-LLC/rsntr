//! Generation of the events a robot gets for deltas (`EventGenerator`).

use std::collections::{BTreeMap, BTreeSet};

use resonator_model::conversation::Conversation;
use resonator_model::conversation::annotations::MENTION_USER;
use resonator_model::data::WaveletData;
use resonator_model::doc::{AnnotationReader, DocView};
use resonator_model::docop::{DocOp, DocOpComponent};
use resonator_model::id::ParticipantId;
use resonator_model::id::constants::MANIFEST_DOCUMENT_ID;
use resonator_model::operation::{BlipOperation, TransformedWaveletDelta, WaveletOperationKind};

use super::wavelet_and_deltas::{EventError, WaveletAndDeltas};
use crate::api::{
    Capability, Context, Element, Event, EventMessageBundle, EventProperties, EventType, RobotName,
};
use crate::convert::{EventDataConverter, resolve_context, root_blip_id};

/// Why a robot that acts only when spoken to should act on a bundle. Not
/// part of the original; computed next to the events, independently of the
/// robot's capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentTriggers {
    /// The robot was added to the wavelet by someone else.
    pub self_added: bool,
    /// Blips created by others as direct replies to a blip the robot
    /// created: the next blip after it in its thread, or the first blip
    /// of a reply thread on it.
    pub reply_blips_created: Vec<String>,
    /// Such direct replies whose content others changed.
    pub reply_blips_changed: Vec<String>,
    /// The authors (other than the robot) of a no-op wavelet operation in
    /// the deltas who did not write again after it: clients send a no-op
    /// when a person ends an edit session, so it means "they have
    /// finished". It produces no event and so is told only here. Whether
    /// an author is a robot is left to the dispatcher and the robot.
    pub finished_by: Vec<String>,
    /// Blips whose content others changed in the deltas and that, after
    /// them, mention the robot: a `mention/user` annotation with its
    /// address. Counted when the robot is a participant at the end, so a
    /// mention that comes with adding the robot (in either order) is one.
    pub mentions: Vec<MentionTrigger>,
}

/// A blip that mentions the robot, with the last person (other than the
/// robot) who changed it in the deltas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionTrigger {
    pub blip_id: String,
    pub by: String,
}

impl AgentTriggers {
    /// Whether the robot was added, directly replied to or mentioned.
    pub fn any(&self) -> bool {
        self.self_added
            || !self.reply_blips_created.is_empty()
            || !self.reply_blips_changed.is_empty()
            || !self.mentions.is_empty()
    }
}

/// Whether a blip's document has a `mention/user` annotation whose value
/// is `address`.
fn mentions(wavelet: &WaveletData, blip_id: &str, address: &str) -> bool {
    wavelet.document(blip_id).is_some_and(|blip| {
        blip.content()
            .ranged_annotations(0, usize::MAX, Some(&[MENTION_USER]))
            .iter()
            .any(|annotation| {
                annotation
                    .value
                    .as_deref()
                    .is_some_and(|value| value.trim().eq_ignore_ascii_case(address))
            })
    })
}

/// The events of some deltas for a robot, with the trigger summary.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GeneratedEvents {
    pub bundle: EventMessageBundle,
    pub triggers: AgentTriggers,
}

/// A change a document operation makes, as the original's document
/// reports it to listeners (`DocumentEvent`), with locations in the
/// document after the operation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DocChange {
    /// An element inserted outside any other inserted element.
    ContentInserted {
        location: usize,
    },
    TextInserted,
    ContentDeleted,
    TextDeleted,
    /// The attributes of the element at a location were set or updated;
    /// `old_values` has the previous value of each attribute that changed.
    Attributes {
        location: usize,
        old_values: BTreeMap<String, Option<String>>,
    },
}

/// The changes of a document operation, in the order of its components,
/// and the annotation changes: one `(key, new value)` per stretch of a
/// changed key, in the order the stretches end.
fn analyse(op: &DocOp) -> (Vec<DocChange>, Vec<(String, Option<String>)>) {
    let mut changes = Vec::new();
    let mut annotations = Vec::new();
    // The values of the annotation changes that are open.
    let mut open: Vec<(String, Option<String>)> = Vec::new();
    let mut location = 0usize;
    // How deep inside inserted or deleted elements.
    let mut depth = 0usize;
    for component in op.components() {
        match component {
            DocOpComponent::Retain(count) => location += count,
            DocOpComponent::Characters(text) => {
                if depth == 0 {
                    changes.push(DocChange::TextInserted);
                }
                location += text.encode_utf16().count();
            }
            DocOpComponent::ElementStart { .. } => {
                if depth == 0 {
                    changes.push(DocChange::ContentInserted { location });
                }
                depth += 1;
                location += 1;
            }
            DocOpComponent::ElementEnd => {
                depth = depth.saturating_sub(1);
                location += 1;
            }
            DocOpComponent::DeleteCharacters(_) => {
                if depth == 0 {
                    changes.push(DocChange::TextDeleted);
                }
            }
            DocOpComponent::DeleteElementStart { .. } => depth += 1,
            DocOpComponent::DeleteElementEnd => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    changes.push(DocChange::ContentDeleted);
                }
            }
            DocOpComponent::ReplaceAttributes {
                old_attributes,
                new_attributes,
            } => {
                let mut old_values = BTreeMap::new();
                for (name, value) in old_attributes.iter() {
                    if new_attributes.get(name) != Some(value) {
                        old_values.insert(name.to_owned(), Some(value.to_owned()));
                    }
                }
                for (name, _) in new_attributes.iter() {
                    if !old_attributes.contains_key(name) {
                        old_values.insert(name.to_owned(), None);
                    }
                }
                changes.push(DocChange::Attributes {
                    location,
                    old_values,
                });
                location += 1;
            }
            DocOpComponent::UpdateAttributes(update) => {
                let old_values = update
                    .updates()
                    .iter()
                    .filter(|u| u.old_value != u.new_value)
                    .map(|u| (u.name.clone(), u.old_value.clone()))
                    .collect();
                changes.push(DocChange::Attributes {
                    location,
                    old_values,
                });
                location += 1;
            }
            DocOpComponent::AnnotationBoundary(boundary) => {
                for key in boundary.end_keys() {
                    if let Some(position) = open.iter().position(|(k, _)| k == key) {
                        annotations.push(open.remove(position));
                    }
                }
                for change in boundary.changes() {
                    if let Some(position) = open.iter().position(|(k, _)| k == &change.key) {
                        if open[position].1 == change.new_value {
                            continue;
                        }
                        annotations.push(open.remove(position));
                    }
                    open.push((change.key.clone(), change.new_value.clone()));
                }
            }
        }
    }
    (changes, annotations)
}

/// Whether a blip is a direct reply to a blip created by `author`: the
/// blip before it in its thread, or else the blip its thread hangs off, is
/// one of theirs.
fn is_direct_reply_to(
    conversation: &Conversation<'_>,
    blip_id: &str,
    author: &ParticipantId,
) -> bool {
    let Some(thread) = conversation.thread_of(blip_id) else {
        return false;
    };
    let position = thread.blips().iter().position(|b| b.id() == blip_id);
    let replied_to = match position {
        Some(position) if position > 0 => thread.blips().get(position - 1),
        _ => conversation.parent_blip_of(thread.id()),
    };
    replied_to
        .and_then(|blip| conversation.blip_data(blip.id()))
        .is_some_and(|data| data.author() == author)
}

/// The blips of a conversation, in document order of the manifest.
fn blip_ids(wavelet: &WaveletData) -> Vec<String> {
    fn visit(thread: &resonator_model::conversation::ConversationThread, out: &mut Vec<String>) {
        for blip in thread.blips() {
            out.push(blip.id().to_owned());
            for reply in blip.reply_threads() {
                visit(reply, out);
            }
        }
    }
    let mut ids = Vec::new();
    if let Some(conversation) = Conversation::read(wavelet) {
        visit(conversation.root_thread(), &mut ids);
    }
    ids
}

/// Whether the wavelet has a conversation with a root blip, without which
/// no events are generated (`getRootConversation`).
fn has_root_blip(wavelet: &WaveletData) -> bool {
    Conversation::read(wavelet).is_some_and(|c| c.root_thread().first_blip().is_some())
}

/// The state of one run of [`EventGenerator::generate_events`].
struct Generation<'a> {
    robot_address: String,
    robot_id: ParticipantId,
    capabilities: &'a BTreeMap<EventType, Capability>,
    converter: EventDataConverter,
    bundle: EventMessageBundle,
    required_blips: BTreeMap<String, BTreeSet<Context>>,
    triggers: AgentTriggers,
    /// `isEventProcessingSuspended`: the robot is not on the wavelet.
    suspended: bool,
    /// Whether the robot is a participant, for the triggers.
    participating: bool,
    // Per delta.
    delta_author: ParticipantId,
    delta_timestamp: i64,
    participants_added: Vec<String>,
    participants_removed: Vec<String>,
    /// The blips whose documents are listened to, and whether a
    /// document-changed event was generated for them yet.
    doc_handlers: BTreeMap<String, bool>,
    /// The documents others changed, in order, with the last of them to
    /// change each: checked for mentions of the robot at the end.
    touched: Vec<(String, ParticipantId)>,
}

impl Generation<'_> {
    fn has(&self, event_type: EventType) -> bool {
        self.capabilities.contains_key(&event_type)
    }

    /// `isEventFilteredOut`: no events while the robot is not on the
    /// wavelet, except the one that says it was removed, and none caused
    /// by the robot itself.
    fn is_event_filtered_out(&mut self, event: &Event) -> bool {
        let mut suspension_overridden = false;
        match event.event_type() {
            EventType::WaveletSelfRemoved => {
                self.suspended = true;
                suspension_overridden = true;
            }
            EventType::WaveletSelfAdded => self.suspended = false,
            _ => {}
        }
        (self.suspended && !suspension_overridden)
            || event.modified_by.as_deref() == Some(self.robot_address.as_str())
    }

    /// `addEvent`.
    fn add_event(&mut self, event: Event, blip_id: &str) {
        if self.is_event_filtered_out(&event) {
            return;
        }
        if !blip_id.is_empty() {
            let contexts: Vec<Context> = match self.capabilities.get(&event.event_type()) {
                Some(capability) => capability.contexts.clone(),
                None => Capability::DEFAULT_CONTEXTS.to_vec(),
            };
            self.required_blips
                .entry(blip_id.to_owned())
                .or_default()
                .extend(contexts);
        }
        self.bundle.events.push(event);
    }

    fn event(&self, blip_id: &str, properties: EventProperties) -> Event {
        Event::new(
            self.delta_author.address(),
            self.delta_timestamp,
            Some(blip_id),
            properties,
        )
    }

    fn by_other(&self) -> bool {
        self.delta_author != self.robot_id
    }

    /// Notes a change by someone else to a document, for the mentions.
    fn note_touched(&mut self, blip_id: &str) {
        if !self.by_other() || blip_id == MANIFEST_DOCUMENT_ID {
            return;
        }
        self.touched.retain(|(id, _)| id != blip_id);
        self.touched
            .push((blip_id.to_owned(), self.delta_author.clone()));
    }

    /// A no-op by someone else means they finished editing; a change to a
    /// document by them afterwards means they are writing again.
    fn note_finished(&mut self, kind: &WaveletOperationKind) {
        if !self.by_other() {
            return;
        }
        let author = self.delta_author.address();
        let finished = &mut self.triggers.finished_by;
        match kind {
            WaveletOperationKind::NoOp => {
                if !finished.iter().any(|a| a == author) {
                    finished.push(author.to_owned());
                }
            }
            WaveletOperationKind::Blip { .. } => finished.retain(|a| a != author),
            _ => {}
        }
    }

    fn on_participant_added(&mut self, participant: &ParticipantId, wavelet: &WaveletData) {
        if self.has(EventType::WaveletParticipantsChanged) {
            let address = participant.address();
            match self.participants_removed.iter().position(|p| p == address) {
                Some(position) => {
                    self.participants_removed.remove(position);
                }
                None => self.participants_added.push(address.to_owned()),
            }
        }
        if *participant == self.robot_id {
            self.participating = true;
            if self.by_other() {
                self.triggers.self_added = true;
            }
            // Google Wave sent this event to every robot; the original
            // port only to those that ask for it.
            if self.has(EventType::WaveletSelfAdded) {
                let root = Conversation::read(wavelet)
                    .map(|c| root_blip_id(&c))
                    .unwrap_or_default();
                let event = self.event(&root, EventProperties::WaveletSelfAdded);
                self.add_event(event, &root);
            }
        }
    }

    fn on_participant_removed(&mut self, participant: &ParticipantId, wavelet: &WaveletData) {
        if self.has(EventType::WaveletParticipantsChanged) {
            self.participants_removed
                .push(participant.address().to_owned());
        }
        if *participant == self.robot_id {
            self.participating = false;
            self.triggers = AgentTriggers::default();
            self.touched.clear();
            if self.has(EventType::WaveletSelfRemoved) {
                let root = Conversation::read(wavelet)
                    .map(|c| root_blip_id(&c))
                    .unwrap_or_default();
                let event = self.event(&root, EventProperties::WaveletSelfRemoved);
                self.add_event(event, &root);
            }
        }
    }

    fn delta_end(&mut self, wavelet: &WaveletData) {
        if !self.participants_added.is_empty() || !self.participants_removed.is_empty() {
            let root = Conversation::read(wavelet)
                .map(|c| root_blip_id(&c))
                .unwrap_or_default();
            let participants_added = std::mem::take(&mut self.participants_added);
            let participants_removed = std::mem::take(&mut self.participants_removed);
            let event = self.event(
                &root,
                EventProperties::WaveletParticipantsChanged {
                    participants_added,
                    participants_removed,
                },
            );
            self.add_event(event, &root);
        }
    }

    /// The conversation events of a change to the manifest: blips that
    /// appeared and blips that went.
    fn on_manifest_changed(&mut self, before: &[String], wavelet: &WaveletData) {
        let after = blip_ids(wavelet);
        let root = Conversation::read(wavelet)
            .map(|c| root_blip_id(&c))
            .unwrap_or_default();
        for added in after.iter().filter(|id| !before.contains(id)) {
            if self.participating
                && self.by_other()
                && let Some(conversation) = Conversation::read(wavelet)
                && is_direct_reply_to(&conversation, added, &self.robot_id)
            {
                self.triggers.reply_blips_created.push(added.clone());
            }
            if self.has(EventType::WaveletBlipCreated) {
                let event = self.event(
                    &root,
                    EventProperties::WaveletBlipCreated {
                        new_blip_id: Some(added.clone()),
                    },
                );
                self.add_event(event, &root);
            }
        }
        for removed in before.iter().filter(|id| !after.contains(id)) {
            self.triggers.reply_blips_created.retain(|id| id != removed);
            self.triggers.reply_blips_changed.retain(|id| id != removed);
            if self.has(EventType::WaveletBlipRemoved) {
                let event = self.event(
                    &root,
                    EventProperties::WaveletBlipRemoved {
                        removed_blip_id: Some(removed.clone()),
                    },
                );
                self.add_event(event, &root);
            }
        }
    }

    /// The events of an operation on a blip's document
    /// (`EventGeneratingDocumentHandler.onDocumentEvents`). `wavelet` is
    /// the state after the operation.
    fn on_document_changed(&mut self, blip_id: &str, op: &DocOp, wavelet: &WaveletData) {
        let (changes, annotations) = analyse(op);
        let view = wavelet
            .document(blip_id)
            .map(|doc| DocView::new(doc.content()));
        if !changes.is_empty()
            && self.participating
            && self.by_other()
            && !self
                .triggers
                .reply_blips_changed
                .iter()
                .any(|id| id == blip_id)
            && let Some(conversation) = Conversation::read(wavelet)
            && is_direct_reply_to(&conversation, blip_id, &self.robot_id)
        {
            self.triggers.reply_blips_changed.push(blip_id.to_owned());
        }
        for change in &changes {
            let mut gadget_state_changed = false;
            if let DocChange::Attributes {
                location,
                old_values,
            } = change
                && self.has(EventType::GadgetStateChanged)
                && let Some(view) = &view
                && let Some(element) = view.element_starting_at(*location)
            {
                // A gadget's state is in <state name=".." value=".."/>
                // children of the gadget element.
                let name = view.attribute(element, "name");
                let old_value = old_values.get("value").cloned().flatten();
                let mut old_state = BTreeMap::new();
                if name.is_some() || old_value.is_some() {
                    old_state.insert(name.unwrap_or("null").to_owned(), old_value);
                }
                let gadget_ifr = view
                    .parent(element)
                    .and_then(|parent| view.attribute(parent, "ifr"));
                let index = match (gadget_ifr, Conversation::read(wavelet)) {
                    (Some(ifr), Some(conversation)) => {
                        conversation.blip(blip_id).and_then(|blip| {
                            // As in the original, converting the blip here
                            // also files its threads in the bundle.
                            let data =
                                self.converter
                                    .to_blip_data(blip, &conversation, &mut self.bundle);
                            data.elements
                                .iter()
                                .find(|(_, e)| {
                                    e.is_gadget() && e.property(Element::IFRAME) == Some(ifr)
                                })
                                .map(|(position, _)| *position)
                        })
                    }
                    _ => None,
                };
                if !old_state.is_empty()
                    && let Some(index) = index
                {
                    gadget_state_changed = true;
                    let event = self.event(
                        blip_id,
                        EventProperties::GadgetStateChanged {
                            index: Some(index),
                            old_state,
                        },
                    );
                    self.add_event(event, blip_id);
                }
            }
            if let DocChange::ContentInserted { location } = change
                && self.has(EventType::FormButtonClicked)
                && let Some(view) = &view
                && let Some(element) = view.element_starting_at(*location)
                && view.has_tag(element, "click")
            {
                // A click on a form button is recorded as
                // <click clicker=".." time=".."/> in the button's events.
                // The original fails on a click without a valid time; here
                // it is no event.
                let time = view
                    .attribute(element, "time")
                    .and_then(|time| time.trim().parse::<i64>().ok());
                if let Some(time) = time {
                    let button_name = view
                        .parent(element)
                        .and_then(|parent| view.attribute(parent, "name"))
                        .map(str::to_owned);
                    let event = Event {
                        modified_by: view.attribute(element, "clicker").map(str::to_owned),
                        timestamp: time,
                        blip_id: Some(blip_id.to_owned()),
                        properties: EventProperties::FormButtonClicked { button_name },
                    };
                    self.add_event(event, blip_id);
                }
            }
            let generated = self.doc_handlers.get(blip_id).copied().unwrap_or(false);
            if self.has(EventType::DocumentChanged) && !generated && !gadget_state_changed {
                let event = self.event(blip_id, EventProperties::DocumentChanged);
                self.add_event(event, blip_id);
                // One document-changed event per blip and bundle, whether
                // or not it passed the filter.
                self.doc_handlers.insert(blip_id.to_owned(), true);
            }
        }
        if self.has(EventType::AnnotatedTextChanged) {
            for (name, value) in annotations {
                let event = self.event(
                    blip_id,
                    EventProperties::AnnotatedTextChanged {
                        name: Some(name),
                        value,
                    },
                );
                self.add_event(event, blip_id);
            }
        }
    }
}

/// Generates the events of deltas for one robot (`EventGenerator`).
///
/// A robot, or one of its proxies (`robot+proxy@domain`, which gets
/// bundles of its own), is told what others did to a conversational
/// wavelet while it was a participant:
///
/// * `WAVELET_SELF_ADDED` and `WAVELET_SELF_REMOVED` when it is added or
///   removed;
/// * `WAVELET_PARTICIPANTS_CHANGED`, once per delta that adds or removes
///   participants;
/// * `WAVELET_BLIP_CREATED` and `WAVELET_BLIP_REMOVED` when a blip enters
///   or leaves the conversation;
/// * `DOCUMENT_CHANGED`, once per blip and bundle, when the content of a
///   blip changes;
/// * `ANNOTATED_TEXT_CHANGED` for every stretch of a blip an operation
///   sets an annotation on;
/// * `FORM_BUTTON_CLICKED` and `GADGET_STATE_CHANGED`.
///
/// Only events the robot has a capability for are generated. Like the
/// original, this never generates `BLIP_SUBMITTED`, `WAVELET_TITLE_CHANGED`,
/// `WAVELET_TAGS_CHANGED`, `BLIP_CONTRIBUTORS_CHANGED`, or anything for
/// documents that are not blips of the conversation.
///
/// Differences from the original:
///
/// * Events carry the time their delta was applied; the original's are
///   all stamped zero, for want of the time on its wave bus.
/// * The original generates nothing unless the wavelet had a conversation
///   with a root blip *before* the deltas, so a robot put on a wave by the
///   very delta that creates it never learns of it. Here events are
///   generated from the operation on that gives the wavelet its root blip.
/// * Blip creation and removal are found by comparing the conversation
///   before and after each manifest operation, where the original listens
///   to its conversation model; the events are the same, their order
///   within one operation is the manifest's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventGenerator {
    robot_name: RobotName,
}

impl EventGenerator {
    pub fn new(robot_name: RobotName) -> Self {
        EventGenerator { robot_name }
    }

    pub fn robot_name(&self) -> &RobotName {
        &self.robot_name
    }

    /// The bundle of events for the robot (`generateEvents`). Without
    /// events the bundle has no wavelet and blips either.
    pub fn generate_events(
        &self,
        wavelet_and_deltas: &WaveletAndDeltas,
        capabilities: &BTreeMap<EventType, Capability>,
        converter: EventDataConverter,
    ) -> Result<EventMessageBundle, EventError> {
        Ok(self
            .generate(
                wavelet_and_deltas.snapshot_before_deltas(),
                wavelet_and_deltas.deltas(),
                capabilities,
                converter,
            )?
            .bundle)
    }

    /// [`EventGenerator::generate_events`] from a wavelet's state before
    /// some deltas and the deltas, with the trigger summary.
    pub fn generate(
        &self,
        snapshot_before: &WaveletData,
        deltas: &[TransformedWaveletDelta],
        capabilities: &BTreeMap<EventType, Capability>,
        converter: EventDataConverter,
    ) -> Result<GeneratedEvents, EventError> {
        let robot_address = self.robot_name.to_participant_address();
        let robot_id = ParticipantId::new_unchecked(&robot_address);
        let mut bundle = EventMessageBundle::new(&self.robot_name.to_email_address(), "");
        if self.robot_name.has_proxy_for() {
            bundle.proxying_for = Some(self.robot_name.proxy_for().to_owned());
        }
        let mut snapshot = snapshot_before.clone();
        let participating = snapshot.participants().contains(&robot_id);
        let mut generation = Generation {
            robot_address,
            robot_id,
            capabilities,
            converter,
            bundle,
            required_blips: BTreeMap::new(),
            triggers: AgentTriggers::default(),
            suspended: !participating,
            participating,
            delta_author: ParticipantId::unknown(),
            delta_timestamp: 0,
            participants_added: Vec::new(),
            participants_removed: Vec::new(),
            doc_handlers: BTreeMap::new(),
            touched: Vec::new(),
        };
        // Nothing is listened to until there is a conversation with a
        // root blip.
        let mut listening = has_root_blip(&snapshot);
        // The robot was added before there was a conversation to tell it
        // about: by whom and when.
        let mut added_early: Option<(ParticipantId, i64)> = None;
        for delta in deltas {
            generation.delta_author = delta.author().clone();
            generation.delta_timestamp = delta.application_timestamp();
            for op in delta.ops() {
                generation.note_finished(op.kind());
                if let WaveletOperationKind::Blip {
                    blip_id,
                    op: BlipOperation::Content { .. },
                } = op.kind()
                {
                    generation.note_touched(blip_id);
                }
                let apply = |snapshot: &mut WaveletData| {
                    op.apply(snapshot).map_err(|e| {
                        EventError::Operation(format!(
                            "Operation failed to apply when generating events: {e}"
                        ))
                    })
                };
                if !listening {
                    apply(&mut snapshot)?;
                    // The triggers do not depend on a conversation.
                    match op.kind() {
                        WaveletOperationKind::AddParticipant { participant, .. }
                            if *participant == generation.robot_id =>
                        {
                            generation.participating = true;
                            added_early =
                                Some((delta.author().clone(), delta.application_timestamp()));
                            if generation.by_other() {
                                generation.triggers.self_added = true;
                            }
                        }
                        WaveletOperationKind::RemoveParticipant { participant }
                            if *participant == generation.robot_id =>
                        {
                            generation.participating = false;
                            added_early = None;
                            generation.triggers = AgentTriggers::default();
                            generation.touched.clear();
                        }
                        _ => {}
                    }
                    listening = has_root_blip(&snapshot);
                    if listening && let Some((author, timestamp)) = added_early.take() {
                        // Tell the robot now that it was added.
                        let current = std::mem::replace(&mut generation.delta_author, author);
                        generation.delta_timestamp = timestamp;
                        if generation.has(EventType::WaveletSelfAdded) {
                            let root = Conversation::read(&snapshot)
                                .map(|c| root_blip_id(&c))
                                .unwrap_or_default();
                            let event = generation.event(&root, EventProperties::WaveletSelfAdded);
                            generation.add_event(event, &root);
                        }
                        generation.delta_author = current;
                        generation.delta_timestamp = delta.application_timestamp();
                    }
                    continue;
                }
                match op.kind() {
                    WaveletOperationKind::AddParticipant { participant, .. } => {
                        apply(&mut snapshot)?;
                        generation.on_participant_added(participant, &snapshot);
                    }
                    WaveletOperationKind::RemoveParticipant { participant } => {
                        apply(&mut snapshot)?;
                        generation.on_participant_removed(participant, &snapshot);
                    }
                    WaveletOperationKind::Blip {
                        blip_id,
                        op: BlipOperation::Content { op: doc_op, .. },
                    } => {
                        // A document is listened to from the first
                        // operation on it that finds it in the
                        // conversation (`attachDocHandler`).
                        if !generation.doc_handlers.contains_key(blip_id)
                            && Conversation::read(&snapshot)
                                .is_some_and(|c| c.blip(blip_id).is_some())
                        {
                            generation.doc_handlers.insert(blip_id.clone(), false);
                        }
                        let blips_before =
                            (blip_id == MANIFEST_DOCUMENT_ID).then(|| blip_ids(&snapshot));
                        apply(&mut snapshot)?;
                        if let Some(before) = blips_before {
                            generation.on_manifest_changed(&before, &snapshot);
                        }
                        if generation.doc_handlers.contains_key(blip_id) {
                            generation.on_document_changed(blip_id, doc_op, &snapshot);
                        }
                    }
                    _ => apply(&mut snapshot)?,
                }
            }
            generation.delta_end(&snapshot);
        }
        let Generation {
            mut bundle,
            required_blips,
            mut triggers,
            participating,
            robot_address,
            touched,
            ..
        } = generation;
        if !participating {
            triggers = AgentTriggers::default();
        } else if let Some(conversation) = Conversation::read(&snapshot) {
            for (blip_id, by) in touched {
                if conversation.blip(&blip_id).is_some()
                    && mentions(&snapshot, &blip_id, &robot_address)
                {
                    triggers.mentions.push(MentionTrigger {
                        blip_id,
                        by: by.address().to_owned(),
                    });
                }
            }
        }
        if bundle.events.is_empty() {
            // The bundle may have threads from a gadget lookup; without
            // events there is nothing to tell.
            bundle.threads.clear();
            return Ok(GeneratedEvents { bundle, triggers });
        }
        if let Some(conversation) = Conversation::read(&snapshot) {
            resolve_context(&mut bundle, &required_blips, &conversation, converter);
        }
        Ok(GeneratedEvents { bundle, triggers })
    }
}
