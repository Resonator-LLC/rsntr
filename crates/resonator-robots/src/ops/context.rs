//! The state shared by the operations of one request batch
//! (`OperationContextImpl`, `RobotWaveletData`).

use std::collections::{BTreeMap, HashMap};
use std::sync::MutexGuard;

use resonator_model::conversation::{
    Conversation, WaveletBasedConversation, conversation_id_for, wavelet_has_conversation,
};
use resonator_model::data::WaveletData;
use resonator_model::id::util::{build_user_data_wavelet_id, is_conversational_id};
use resonator_model::id::{IdGeneratorImpl, ParticipantId, WaveId, WaveletId, WaveletName};
use resonator_model::operation::WaveletDelta;
use resonator_model::version::{HashedVersion, HashedVersionFactory};
use resonator_model::wavelet::OpBasedWavelet;
use resonator_server::waveserver::WaveletProvider;
use serde_json::Value;

use super::OperationExecutor;
use crate::api::{
    Event, EventProperties, InvalidRequest, JsonRpcResponse, OperationRequest, ParamsProperty,
    TEMP_ID_MARKER,
};
use crate::convert::{EventDataConverter, deserialise_wave_id, deserialise_wavelet_id};

/// A wavelet opened by a batch: the state it was opened at and, per
/// author, a copy that the author's operations are applied to and recorded
/// against (`RobotWaveletData`).
///
/// As in the original, each author works on a copy of their own: within a
/// batch, an author does not see what another author (the robot itself
/// and its proxies are different authors) did to the same wavelet. Each
/// author's operations become one delta against the version the wavelet
/// was opened at; the server transforms them against each other.
#[derive(Debug, Clone)]
pub struct RobotWavelet {
    snapshot: WaveletData,
    timestamp: i64,
    /// In order of first use.
    authors: Vec<(ParticipantId, OpBasedWavelet)>,
}

impl RobotWavelet {
    /// Wraps a wavelet's state. The deltas produced target the hashed
    /// version of `snapshot`.
    ///
    /// (The original targets the wavelet's last *committed* version, which
    /// is not the version of the snapshot while a commit is pending.)
    pub fn new(snapshot: WaveletData, timestamp: i64) -> Self {
        RobotWavelet {
            snapshot,
            timestamp,
            authors: Vec::new(),
        }
    }

    /// An empty wavelet at version zero (`RobotsUtil.createEmptyRobotWavelet`).
    pub fn empty(creator: &ParticipantId, name: &WaveletName, timestamp: i64) -> Self {
        let version_zero = HashedVersionFactory::create_version_zero(name);
        Self::new(
            WaveletData::empty(name, creator.clone(), version_zero, timestamp),
            timestamp,
        )
    }

    pub fn wavelet_name(&self) -> WaveletName {
        self.snapshot.wavelet_name()
    }

    /// The state the wavelet was opened at.
    pub fn snapshot(&self) -> &WaveletData {
        &self.snapshot
    }

    pub fn snapshot_version(&self) -> &HashedVersion {
        self.snapshot.hashed_version()
    }

    /// The author's copy of the wavelet (`getOpBasedWavelet`).
    pub fn op_based_wavelet(&mut self, author: &ParticipantId) -> &mut OpBasedWavelet {
        let position = match self.authors.iter().position(|(a, _)| a == author) {
            Some(position) => position,
            None => {
                self.authors.push((
                    author.clone(),
                    OpBasedWavelet::new(self.snapshot.clone(), author.clone(), self.timestamp),
                ));
                self.authors.len() - 1
            }
        };
        &mut self.authors[position].1
    }

    fn author_wavelet(&self, author: &ParticipantId) -> Option<&OpBasedWavelet> {
        self.authors
            .iter()
            .find(|(a, _)| a == author)
            .map(|(_, wavelet)| wavelet)
    }

    /// Removes and returns the recorded operations as one delta per author
    /// that has any, in order of first use (`getDeltas`).
    pub fn take_deltas(&mut self) -> Vec<WaveletDelta> {
        self.authors
            .iter_mut()
            .filter_map(|(_, wavelet)| wavelet.take_delta())
            .collect()
    }
}

/// What a batch did to one wavelet on behalf of one author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Touch {
    pub wavelet_name: WaveletName,
    pub author: ParticipantId,
    pub operation_index: usize,
}

/// The state shared by the operations of a batch: the wavelets opened so
/// far, the ids of things created so far, and the responses
/// (`OperationContextImpl`).
pub struct OperationContext<'a> {
    pub(crate) executor: &'a OperationExecutor,
    converter: EventDataConverter,
    bound: bool,
    now: i64,
    /// One slot per operation of the batch.
    responses: Vec<Option<JsonRpcResponse>>,
    /// The index of the operation being executed.
    current: usize,
    /// In order of opening.
    opened: Vec<RobotWavelet>,
    temp_blip_ids: HashMap<String, String>,
    temp_wavelet_names: BTreeMap<WaveletName, WaveletName>,
    pub(crate) touches: Vec<Touch>,
}

/// A conversation opened for mutation, with what its operations need.
pub struct OpenConversation<'c> {
    pub wavelet: &'c mut OpBasedWavelet,
    ids: MutexGuard<'c, IdGeneratorImpl>,
    temp_blip_ids: &'c mut HashMap<String, String>,
}

impl OpenConversation<'_> {
    /// The conversation for mutation.
    pub fn conversation(&mut self) -> Result<WaveletBasedConversation<'_>, InvalidRequest> {
        WaveletBasedConversation::new(self.wavelet, &mut *self.ids)
            .map_err(|e| InvalidRequest::new(e.to_string()))
    }

    /// The current structure of the conversation.
    pub fn read(&self) -> Result<Conversation<'_>, InvalidRequest> {
        Conversation::read(self.wavelet.data())
            .ok_or_else(|| InvalidRequest::new("the wavelet has no conversation manifest"))
    }

    /// Remembers the id of a blip created for a temporary id (`putBlip`).
    pub fn put_blip(&mut self, requested_id: Option<&str>, new_blip_id: &str) {
        if let Some(requested) = requested_id.filter(|id| id.starts_with(TEMP_ID_MARKER)) {
            self.temp_blip_ids
                .insert(requested.to_owned(), new_blip_id.to_owned());
        }
    }

    /// The id of an existing blip named by a real or temporary id
    /// (`getBlip`).
    pub fn blip_id(&self, blip_id: &str) -> Result<String, InvalidRequest> {
        let actual = if blip_id.starts_with(TEMP_ID_MARKER) {
            self.temp_blip_ids.get(blip_id).map(String::as_str)
        } else {
            Some(blip_id)
        };
        match actual {
            Some(actual) if self.read()?.blip(actual).is_some() => Ok(actual.to_owned()),
            _ => Err(InvalidRequest::new(format!(
                "Blip with id {blip_id} does not exist or has been deleted"
            ))),
        }
    }
}

impl<'a> OperationContext<'a> {
    pub(crate) fn new(
        executor: &'a OperationExecutor,
        converter: EventDataConverter,
        operation_count: usize,
        bound_wavelet: Option<WaveletData>,
    ) -> Self {
        let now = executor.clock.now_millis();
        let bound = bound_wavelet.is_some();
        OperationContext {
            executor,
            converter,
            bound,
            now,
            responses: vec![None; operation_count],
            current: 0,
            opened: bound_wavelet
                .into_iter()
                .map(|wavelet| RobotWavelet::new(wavelet, now))
                .collect(),
            temp_blip_ids: HashMap::new(),
            temp_wavelet_names: BTreeMap::new(),
            touches: Vec::new(),
        }
    }

    pub(crate) fn set_current(&mut self, index: usize) {
        self.current = index;
    }

    /// The time of the batch, in milliseconds since the epoch.
    pub fn now(&self) -> i64 {
        self.now
    }

    pub fn converter(&self) -> EventDataConverter {
        self.converter
    }

    /// Whether the batch is the reply of a robot to events of a wavelet
    /// (`isBound`).
    pub fn is_bound(&self) -> bool {
        self.bound
    }

    pub fn has_response(&self) -> bool {
        self.responses[self.current].is_some()
    }

    fn set_response(&mut self, response: JsonRpcResponse) {
        // The original refuses to overwrite a response; the first one
        // stays.
        if self.responses[self.current].is_none() {
            self.responses[self.current] = Some(response);
        }
    }

    /// Sets the current operation's successful response
    /// (`constructResponse`).
    pub fn construct_response(
        &mut self,
        operation: &OperationRequest,
        data: BTreeMap<ParamsProperty, Value>,
    ) {
        self.set_response(JsonRpcResponse::result(operation.id.clone(), data));
    }

    /// Sets the current operation's error response
    /// (`constructErrorResponse`).
    pub fn construct_error_response(&mut self, operation: &OperationRequest, message: &str) {
        self.set_response(JsonRpcResponse::error(operation.id.clone(), message));
    }

    /// Sets the current operation's response from an event: its properties
    /// as data, or the message of an operation error (`processEvent`).
    pub fn process_event(&mut self, operation: &OperationRequest, event: &Event) {
        if let EventProperties::OperationError { message, .. } = &event.properties {
            self.construct_error_response(operation, message.as_deref().unwrap_or(""));
        } else {
            self.construct_response(operation, event.response_data());
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (Vec<Option<JsonRpcResponse>>, Vec<RobotWavelet>, Vec<Touch>) {
        (self.responses, self.opened, self.touches)
    }

    /// The wavelets opened so far (`getOpenWavelets`).
    pub fn open_wavelets(&self) -> &[RobotWavelet] {
        &self.opened
    }

    fn position(&self, name: &WaveletName) -> Option<usize> {
        self.opened.iter().position(|w| &w.wavelet_name() == name)
    }

    /// Adds a wavelet created by the batch under the ids the client gave
    /// it (`putWavelet`). If the client's wave id is a temporary one,
    /// later operations of the batch can use the client's ids.
    pub fn put_wavelet(
        &mut self,
        wave_id: &WaveId,
        wavelet_id: &WaveletId,
        new_wavelet: RobotWavelet,
    ) -> Result<(), InvalidRequest> {
        let name = new_wavelet.wavelet_name();
        if self.position(&name).is_some() {
            return Err(InvalidRequest::new(
                "Not allowed to put an already open wavelet in as a new wavelet",
            ));
        }
        if wave_id.id().starts_with(TEMP_ID_MARKER) {
            self.temp_wavelet_names
                .insert(WaveletName::of(wave_id.clone(), wavelet_id.clone()), name);
        }
        self.opened.push(new_wavelet);
        Ok(())
    }

    /// Records that the current operation used a wavelet as an author.
    pub(crate) fn touch(&mut self, name: &WaveletName, author: &ParticipantId) {
        self.touches.push(Touch {
            wavelet_name: name.clone(),
            author: author.clone(),
            operation_index: self.current,
        });
    }

    /// The wave and wavelet ids of an operation.
    pub fn wavelet_ids_of(
        &self,
        operation: &OperationRequest,
    ) -> Result<(WaveId, WaveletId), InvalidRequest> {
        let wave_id = operation.required_string(ParamsProperty::WaveId)?;
        let wavelet_id = operation.required_string(ParamsProperty::WaveletId)?;
        match (
            deserialise_wave_id(&wave_id),
            deserialise_wavelet_id(&wavelet_id),
        ) {
            (Some(wave_id), Some(wavelet_id)) => Ok((wave_id, wavelet_id)),
            _ => Err(InvalidRequest::new("Invalid id")),
        }
    }

    /// The real name of a wavelet named by real or temporary ids.
    fn resolve(&self, wave_id: &WaveId, wavelet_id: &WaveletId) -> Option<WaveletName> {
        let name = WaveletName::of(wave_id.clone(), wavelet_id.clone());
        if wave_id.id().starts_with(TEMP_ID_MARKER) {
            self.temp_wavelet_names.get(&name).cloned()
        } else {
            Some(name)
        }
    }

    /// Whether the wavelet exists and the participant may access it;
    /// errors of the server count as "no".
    pub async fn check_access(&self, name: &WaveletName, participant: &ParticipantId) -> bool {
        self.executor
            .server
            .check_access_permission(name, Some(participant))
            .await
            .unwrap_or(false)
    }

    /// The current state of a wavelet the participant may access
    /// (`getWaveletSnapshot`): an "Access rejected" error if they may not
    /// (or it does not exist), `None` if the server fails to provide it.
    pub async fn wavelet_snapshot(
        &self,
        name: &WaveletName,
        participant: &ParticipantId,
    ) -> Result<Option<WaveletData>, InvalidRequest> {
        if !self.check_access(name, participant).await {
            return Err(InvalidRequest::new("Access rejected"));
        }
        Ok(self
            .executor
            .server
            .get_snapshot(name)
            .await
            .ok()
            .flatten()
            .map(|snapshot| snapshot.snapshot))
    }

    /// Opens a wavelet for a participant and returns its real name
    /// (`openWavelet`). The wavelet is fetched from the server the first
    /// time the batch needs it, at its version of that moment, after
    /// checking that the participant may access it. A participant's own
    /// user-data wavelet that does not exist yet is created empty.
    pub async fn open_wavelet(
        &mut self,
        wave_id: &WaveId,
        wavelet_id: &WaveletId,
        participant: &ParticipantId,
    ) -> Result<WaveletName, InvalidRequest> {
        let unknown = || {
            InvalidRequest::new(format!(
                "Wavelet {} couldn't be retrieved",
                WaveletName::of(wave_id.clone(), wavelet_id.clone())
            ))
        };
        let name = self.resolve(wave_id, wavelet_id).ok_or_else(unknown)?;
        let position = match self.position(&name) {
            Some(position) => position,
            None => {
                let is_own_user_data =
                    build_user_data_wavelet_id(participant).is_ok_and(|id| id == name.wavelet_id);
                let exists = self
                    .executor
                    .server
                    .get_versions(&name)
                    .await
                    .is_ok_and(|versions| versions.is_some());
                let wavelet = if is_own_user_data && !exists {
                    let mut wavelet = RobotWavelet::empty(participant, &name, self.now);
                    // The owner is the wavelet's participant, as when a
                    // client creates it. (The original leaves it without
                    // participants, and so out of everyone's reach.)
                    wavelet
                        .op_based_wavelet(participant)
                        .add_participant(participant)
                        .map_err(|e| InvalidRequest::new(e.to_string()))?;
                    wavelet
                } else {
                    let snapshot = self
                        .wavelet_snapshot(&name, participant)
                        .await?
                        .ok_or_else(unknown)?;
                    RobotWavelet::new(snapshot, self.now)
                };
                self.opened.push(wavelet);
                self.opened.len() - 1
            }
        };
        // Make the author's copy now, so that it reflects the wavelet as
        // the batch first saw it.
        self.opened[position].op_based_wavelet(participant);
        self.touch(&name, participant);
        Ok(name)
    }

    /// Opens the wavelet an operation addresses (`openWavelet(operation,
    /// participant)`).
    pub async fn open_operation_wavelet(
        &mut self,
        operation: &OperationRequest,
        participant: &ParticipantId,
    ) -> Result<WaveletName, InvalidRequest> {
        let (wave_id, wavelet_id) = self.wavelet_ids_of(operation)?;
        self.open_wavelet(&wave_id, &wavelet_id, participant).await
    }

    /// The participant's copy of an opened wavelet.
    pub fn wavelet(
        &self,
        name: &WaveletName,
        participant: &ParticipantId,
    ) -> Result<&OpBasedWavelet, InvalidRequest> {
        self.position(name)
            .and_then(|position| self.opened[position].author_wavelet(participant))
            .ok_or_else(|| InvalidRequest::new(format!("Wavelet {name} couldn't be retrieved")))
    }

    /// The participant's copy of an opened wavelet, for mutation.
    pub fn wavelet_mut(
        &mut self,
        name: &WaveletName,
        participant: &ParticipantId,
    ) -> Result<&mut OpBasedWavelet, InvalidRequest> {
        let position = self
            .position(name)
            .ok_or_else(|| InvalidRequest::new(format!("Wavelet {name} couldn't be retrieved")))?;
        Ok(self.opened[position].op_based_wavelet(participant))
    }

    /// Takes the participant's copy of an opened wavelet out of the
    /// context, to use it next to another one; give it back with
    /// [`OperationContext::restore_wavelet`].
    pub(crate) fn take_wavelet(
        &mut self,
        name: &WaveletName,
        participant: &ParticipantId,
    ) -> Result<OpBasedWavelet, InvalidRequest> {
        let position = self
            .position(name)
            .ok_or_else(|| InvalidRequest::new(format!("Wavelet {name} couldn't be retrieved")))?;
        let slot = self.opened[position].op_based_wavelet(participant);
        let placeholder = OpBasedWavelet::new(slot.data().clone(), participant.clone(), self.now);
        Ok(std::mem::replace(slot, placeholder))
    }

    pub(crate) fn restore_wavelet(&mut self, wavelet: OpBasedWavelet) {
        let name = wavelet.data().wavelet_name();
        if let Some(position) = self.position(&name) {
            let author = wavelet.author().clone();
            *self.opened[position].op_based_wavelet(&author) = wavelet;
        }
    }

    /// The participant's copy of an opened wavelet as a conversation
    /// (`openConversation` and `ConversationUtil.buildConversation`). Do
    /// not hold the result across an `await`.
    pub fn conversation(
        &mut self,
        name: &WaveletName,
        participant: &ParticipantId,
    ) -> Result<OpenConversation<'_>, InvalidRequest> {
        let position = self
            .position(name)
            .ok_or_else(|| InvalidRequest::new(format!("Wavelet {name} couldn't be retrieved")))?;
        let wavelet = self.opened[position].op_based_wavelet(participant);
        check_conversational(wavelet.data())?;
        Ok(OpenConversation {
            wavelet,
            ids: self.executor.ids(),
            temp_blip_ids: &mut self.temp_blip_ids,
        })
    }

    /// Opens the wavelet an operation addresses as a conversation
    /// (`openConversation(operation, participant)`).
    pub async fn open_conversation(
        &mut self,
        operation: &OperationRequest,
        participant: &ParticipantId,
    ) -> Result<OpenConversation<'_>, InvalidRequest> {
        let name = self.open_operation_wavelet(operation, participant).await?;
        self.conversation(&name, participant)
    }

    /// The id of the conversation in a wavelet.
    pub fn conversation_id(name: &WaveletName) -> String {
        conversation_id_for(&name.wavelet_id)
    }
}

/// `ConversationUtil.buildConversation`'s preconditions.
pub(crate) fn check_conversational(wavelet: &WaveletData) -> Result<(), InvalidRequest> {
    let id = wavelet.wavelet_id();
    if !is_conversational_id(id).unwrap_or(false) {
        return Err(InvalidRequest::new(format!(
            "Expected conversational wavelet, got {id}"
        )));
    }
    if !wavelet_has_conversation(wavelet) {
        return Err(InvalidRequest::new(format!(
            "Conversation can't be build on a wavelet {id} without conversation structure"
        )));
    }
    Ok(())
}
