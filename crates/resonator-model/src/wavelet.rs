//! A wavelet whose mutations are applied locally and recorded as wavelet
//! operations for submission (`OpBasedWavelet`).
//!
//! Derived from `org.waveprotocol.wave.model.wave.opbased.OpBasedWavelet`,
//! `OpBasedBlip`, `ParticipationHelper` and
//! `operation.wave.AbstractWaveletOperationContextFactory` of Apache Wave,
//! licensed under the Apache License, Version 2.0. Listeners are not ported.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::LazyLock;

use thiserror::Error;

use crate::data::{BlipData, WaveletData};
use crate::doc::{DocError, DocView, DocumentSink, MutableDocument};
use crate::docop::{BootstrapDocument, DocOp};
use crate::id::constants::CONVERSATION_ROOT_WAVELET;
use crate::id::util::{build_user_data_wavelet_id, is_user_data_wavelet_of};
use crate::id::{IdGenerator, ParticipantId, WaveId, WaveletId, WaveletName};
use crate::operation::{
    WaveletDelta, WaveletOperation, WaveletOperationContext, WaveletOperationError,
};
use crate::version::{HashedVersion, HashedVersionFactory};

static EMPTY_DOCUMENT: LazyLock<BootstrapDocument> = LazyLock::new(BootstrapDocument::new);

/// What happens when the author of an operation is not a participant of a
/// wavelet that has participants (`ParticipationHelper`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParticipationHelper {
    /// The author first adds themself as a participant (`AUTO`, the
    /// original's default).
    #[default]
    Auto,
    /// Nothing: the operation is emitted as it is (`IGNORANT`).
    Ignorant,
    /// The mutation fails (`READONLY`).
    ReadOnly,
}

/// Failure of a wavelet mutation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WaveletError {
    #[error(transparent)]
    Operation(#[from] WaveletOperationError),
    #[error(transparent)]
    Document(#[from] DocError),
    /// The author is not a participant and the wavelet is read-only for
    /// non-participants.
    #[error("{0} is not a participant of the wavelet, which is read only")]
    ReadOnly(ParticipantId),
    #[error("the wave already has a wavelet {0}")]
    DuplicateWavelet(WaveletId),
    #[error("invalid id: {0}")]
    InvalidId(String),
}

/// A wavelet together with an author on whose behalf it is mutated.
///
/// Every mutation is turned into a [`WaveletOperation`] by the author,
/// applied to the wavelet data at once, and recorded. The recorded
/// operations are what has to be submitted to the server, in one delta
/// ([`OpBasedWavelet::take_delta`]) against the version the wavelet data
/// had when it was wrapped.
///
/// As in the original, operations are created with a version increment of
/// zero: applying them locally changes neither the wavelet's version nor
/// its hashed version, only its content and last-modified time. A blip
/// touched by a local operation gets the unchanged wavelet version as its
/// last-modified version.
#[derive(Debug, Clone)]
pub struct OpBasedWavelet {
    data: WaveletData,
    author: ParticipantId,
    timestamp: i64,
    participation: ParticipationHelper,
    target_version: HashedVersion,
    ops: Vec<WaveletOperation>,
    /// Follow every operation with a version update of one, as the
    /// reference tests' `OpBasedWaveletFactory` does.
    #[cfg(test)]
    pub(crate) bump_versions: bool,
}

impl OpBasedWavelet {
    /// Wraps wavelet data for mutation by `author`. `timestamp`
    /// (milliseconds since the epoch) is the time recorded in the context
    /// of every operation, where the original reads the clock.
    pub fn new(data: WaveletData, author: ParticipantId, timestamp: i64) -> Self {
        let target_version = data.hashed_version().clone();
        OpBasedWavelet {
            data,
            author,
            timestamp,
            participation: ParticipationHelper::default(),
            target_version,
            ops: Vec::new(),
            #[cfg(test)]
            bump_versions: false,
        }
    }

    pub fn with_participation_helper(mut self, helper: ParticipationHelper) -> Self {
        self.participation = helper;
        self
    }

    pub fn data(&self) -> &WaveletData {
        &self.data
    }

    /// Direct access to the wavelet data, for applying operations that
    /// came from elsewhere (the server's, or version updates). Changes made
    /// this way are not recorded.
    pub fn data_mut(&mut self) -> &mut WaveletData {
        &mut self.data
    }

    pub fn into_data(self) -> WaveletData {
        self.data
    }

    pub fn author(&self) -> &ParticipantId {
        &self.author
    }

    /// Changes the author of subsequent operations. Take the delta of the
    /// previous author first: a delta has a single author.
    pub fn set_author(&mut self, author: ParticipantId) {
        self.author = author;
    }

    pub fn timestamp(&self) -> i64 {
        self.timestamp
    }

    pub fn set_timestamp(&mut self, timestamp: i64) {
        self.timestamp = timestamp;
    }

    /// The operations recorded since the last take, oldest first.
    pub fn ops(&self) -> &[WaveletOperation] {
        &self.ops
    }

    /// Removes and returns the recorded operations.
    pub fn take_ops(&mut self) -> Vec<WaveletOperation> {
        std::mem::take(&mut self.ops)
    }

    /// Removes the recorded operations and returns them as a delta by the
    /// current author against the hashed version the wavelet data had when
    /// it was wrapped, or `None` if nothing was recorded.
    pub fn take_delta(&mut self) -> Option<WaveletDelta> {
        if self.ops.is_empty() {
            return None;
        }
        Some(WaveletDelta::new(
            self.author.clone(),
            self.target_version.clone(),
            self.take_ops(),
        ))
    }

    fn create_context(&self) -> WaveletOperationContext {
        WaveletOperationContext::new(self.author.clone(), self.timestamp, 0)
    }

    fn apply_and_send(&mut self, op: WaveletOperation) -> Result<(), WaveletError> {
        op.apply(&mut self.data)?;
        #[cfg(test)]
        if self.bump_versions {
            op.create_version_update_op(1, None).apply(&mut self.data)?;
            // The clock of the golden generator: a millisecond per operation.
            self.timestamp += 1;
        }
        self.ops.push(op);
        Ok(())
    }

    /// Makes sure the author may act on the wavelet (`authorise`). Returns
    /// whether an add-participant operation for the author was injected.
    fn authorise(&mut self) -> Result<bool, WaveletError> {
        let participants = self.data.participants();
        if participants.is_empty() || participants.contains(&self.author) {
            return Ok(false);
        }
        match self.participation {
            ParticipationHelper::Ignorant => Ok(false),
            ParticipationHelper::ReadOnly => Err(WaveletError::ReadOnly(self.author.clone())),
            ParticipationHelper::Auto => {
                let op =
                    WaveletOperation::add_participant(self.create_context(), self.author.clone());
                self.apply_and_send(op)?;
                Ok(true)
            }
        }
    }

    pub fn participants(&self) -> &[ParticipantId] {
        self.data.participants()
    }

    /// Adds a participant unless already present (`addParticipant`).
    pub fn add_participant(&mut self, participant: &ParticipantId) -> Result<(), WaveletError> {
        if self.data.participants().contains(participant) {
            return Ok(());
        }
        let injected = self.authorise()?;
        if injected && participant == &self.author {
            return Ok(());
        }
        let op = WaveletOperation::add_participant(self.create_context(), participant.clone());
        self.apply_and_send(op)
    }

    /// Removes a participant if present (`removeParticipant`).
    pub fn remove_participant(&mut self, participant: &ParticipantId) -> Result<(), WaveletError> {
        if !self.data.participants().contains(participant) {
            return Ok(());
        }
        self.authorise()?;
        let op = WaveletOperation::remove_participant(self.create_context(), participant.clone());
        self.apply_and_send(op)
    }

    /// Emits an operation that changes nothing (`touch`).
    pub fn touch(&mut self) -> Result<(), WaveletError> {
        self.authorise()?;
        self.apply_and_send(WaveletOperation::no_op(self.create_context()))
    }

    /// The document or blip with an id, if an operation ever touched it.
    pub fn blip(&self, doc_id: &str) -> Option<&BlipData> {
        self.data.document(doc_id)
    }

    pub fn has_document(&self, doc_id: &str) -> bool {
        self.data.document(doc_id).is_some()
    }

    /// The content of a document; the empty document if it does not exist
    /// (the original creates documents on first access).
    pub fn document(&self, doc_id: &str) -> &BootstrapDocument {
        match self.data.document(doc_id) {
            Some(blip) => blip.content(),
            None => &EMPTY_DOCUMENT,
        }
    }

    /// The element tree of a document.
    pub fn document_view(&self, doc_id: &str) -> DocView {
        DocView::new(self.document(doc_id))
    }

    /// Applies a document operation by the author to a document, creating
    /// the document if needed, and records it.
    pub fn apply_doc_op(&mut self, doc_id: &str, op: DocOp) -> Result<(), WaveletError> {
        self.authorise()?;
        let op = WaveletOperation::blip_content(self.create_context(), doc_id, op);
        self.apply_and_send(op)
    }

    /// A document of the wavelet for editing. Each edit becomes one
    /// recorded wavelet operation.
    pub fn document_sink(&mut self, doc_id: &str) -> WaveletDocument<'_> {
        WaveletDocument {
            wavelet: self,
            doc_id: doc_id.to_owned(),
        }
    }

    /// Edits a document through `edit`.
    pub fn edit_document<R>(
        &mut self,
        doc_id: &str,
        edit: impl FnOnce(&mut MutableDocument<'_>) -> Result<R, DocError>,
    ) -> Result<R, DocError> {
        let mut sink = self.document_sink(doc_id);
        let mut doc = MutableDocument::new(&mut sink);
        edit(&mut doc)
    }
}

/// One document of an [`OpBasedWavelet`], as the target of a
/// [`MutableDocument`].
pub struct WaveletDocument<'a> {
    wavelet: &'a mut OpBasedWavelet,
    doc_id: String,
}

impl WaveletDocument<'_> {
    pub fn mutable(&mut self) -> MutableDocument<'_> {
        MutableDocument::new(self)
    }
}

impl DocumentSink for WaveletDocument<'_> {
    fn document(&self) -> &BootstrapDocument {
        self.wavelet.document(&self.doc_id)
    }

    fn consume(&mut self, op: DocOp) -> Result<(), DocError> {
        self.wavelet
            .apply_doc_op(&self.doc_id, op)
            .map_err(|e| match e {
                WaveletError::Document(e) => e,
                WaveletError::Operation(e) => DocError::Wavelet(Box::new(e)),
                other => DocError::InvalidArgument(other.to_string()),
            })
    }
}

/// The wavelets of one wave as seen by one user, each wrapped for mutation
/// by that user (`WaveViewImpl`).
///
/// New wavelets get their creator as first participant, as with the
/// original's default `WaveletConfigurator.ADD_CREATOR`.
#[derive(Debug, Clone)]
pub struct WaveView {
    wave_id: WaveId,
    viewer: ParticipantId,
    timestamp: i64,
    wavelets: BTreeMap<WaveletId, OpBasedWavelet>,
    #[cfg(test)]
    pub(crate) bump_versions: bool,
}

impl WaveView {
    /// An empty view. `timestamp` is given to every wavelet it wraps; see
    /// [`OpBasedWavelet::new`].
    pub fn new(wave_id: WaveId, viewer: ParticipantId, timestamp: i64) -> Self {
        WaveView {
            wave_id,
            viewer,
            timestamp,
            wavelets: BTreeMap::new(),
            #[cfg(test)]
            bump_versions: false,
        }
    }

    pub fn wave_id(&self) -> &WaveId {
        &self.wave_id
    }

    pub fn viewer(&self) -> &ParticipantId {
        &self.viewer
    }

    /// The time given to operations of wavelets added from now on.
    pub fn timestamp(&self) -> i64 {
        self.timestamp
    }

    /// Sets the time of the operations to come, for all wavelets.
    pub fn set_timestamp(&mut self, timestamp: i64) {
        self.timestamp = timestamp;
        for wavelet in self.wavelets.values_mut() {
            wavelet.set_timestamp(timestamp);
        }
    }

    /// Adds an existing wavelet of the wave, replacing any with the same
    /// id.
    pub fn add_wavelet(&mut self, data: WaveletData) -> &mut OpBasedWavelet {
        let id = data.wavelet_id().clone();
        #[allow(unused_mut)]
        let mut wavelet = OpBasedWavelet::new(data, self.viewer.clone(), self.timestamp);
        #[cfg(test)]
        {
            wavelet.bump_versions = self.bump_versions;
        }
        match self.wavelets.entry(id) {
            Entry::Occupied(mut entry) => {
                entry.insert(wavelet);
                entry.into_mut()
            }
            Entry::Vacant(entry) => entry.insert(wavelet),
        }
    }

    pub fn wavelet(&self, id: &WaveletId) -> Option<&OpBasedWavelet> {
        self.wavelets.get(id)
    }

    pub fn wavelet_mut(&mut self, id: &WaveletId) -> Option<&mut OpBasedWavelet> {
        self.wavelets.get_mut(id)
    }

    /// The wavelets ordered by id.
    pub fn wavelets(&self) -> impl Iterator<Item = &OpBasedWavelet> {
        self.wavelets.values()
    }

    pub fn wavelets_mut(&mut self) -> impl Iterator<Item = &mut OpBasedWavelet> {
        self.wavelets.values_mut()
    }

    pub fn wavelet_ids(&self) -> impl Iterator<Item = &WaveletId> {
        self.wavelets.keys()
    }

    /// The id of the conversation root wavelet: `conv+root` in the wave's
    /// domain.
    pub fn root_id(&self) -> Result<WaveletId, WaveletError> {
        WaveletId::of(self.wave_id.domain(), CONVERSATION_ROOT_WAVELET)
            .map_err(|e| WaveletError::InvalidId(e.to_string()))
    }

    pub fn root(&self) -> Option<&OpBasedWavelet> {
        self.wavelets.get(&self.root_id().ok()?)
    }

    /// The id under which the viewer's user-data wavelet is created:
    /// `user+<address>` in the viewer's domain
    /// (`IdUtil.buildUserDataWaveletId`).
    pub fn user_data_id(&self) -> Result<WaveletId, WaveletError> {
        build_user_data_wavelet_id(&self.viewer).map_err(|e| WaveletError::InvalidId(e.to_string()))
    }

    /// The viewer's user-data wavelet: the first wavelet whose id is
    /// `user+<address>`, whatever its domain.
    pub fn user_data(&self) -> Option<&OpBasedWavelet> {
        self.wavelets
            .iter()
            .find(|(id, _)| is_user_data_wavelet_of(self.viewer.address(), id))
            .map(|(_, wavelet)| wavelet)
    }

    pub fn user_data_mut(&mut self) -> Option<&mut OpBasedWavelet> {
        let address = self.viewer.address().to_owned();
        self.wavelets
            .iter_mut()
            .find(|(id, _)| is_user_data_wavelet_of(&address, id))
            .map(|(_, wavelet)| wavelet)
    }

    /// Creates an empty wavelet at version zero, created by the viewer,
    /// with the viewer as participant (`createWavelet(WaveletId)`).
    pub fn create_wavelet(&mut self, id: WaveletId) -> Result<&mut OpBasedWavelet, WaveletError> {
        if self.wavelets.contains_key(&id) {
            return Err(WaveletError::DuplicateWavelet(id));
        }
        let name = WaveletName::of(self.wave_id.clone(), id);
        let data = WaveletData::empty(
            &name,
            self.viewer.clone(),
            HashedVersionFactory::create_version_zero(&name),
            self.timestamp,
        );
        let viewer = self.viewer.clone();
        let wavelet = self.add_wavelet(data);
        wavelet.add_participant(&viewer)?;
        Ok(wavelet)
    }

    /// Creates the conversation root wavelet (`createRoot`).
    pub fn create_root(&mut self) -> Result<&mut OpBasedWavelet, WaveletError> {
        self.create_wavelet(self.root_id()?)
    }

    /// Creates a conversation wavelet with a new id (`createWavelet()`).
    pub fn create_conversation_wavelet(
        &mut self,
        id_generator: &mut dyn IdGenerator,
    ) -> Result<&mut OpBasedWavelet, WaveletError> {
        let id = id_generator
            .new_conversation_wavelet_id()
            .map_err(|e| WaveletError::InvalidId(e.to_string()))?;
        self.create_wavelet(id)
    }

    /// Creates the viewer's user-data wavelet (`createUserData`).
    pub fn create_user_data(&mut self) -> Result<&mut OpBasedWavelet, WaveletError> {
        self.create_wavelet(self.user_data_id()?)
    }

    /// Takes the recorded operations of every wavelet that has any, as one
    /// delta per wavelet, ordered by wavelet id.
    pub fn take_deltas(&mut self) -> Vec<(WaveletName, WaveletDelta)> {
        let mut deltas = Vec::new();
        for wavelet in self.wavelets.values_mut() {
            if let Some(delta) = wavelet.take_delta() {
                deltas.push((wavelet.data().wavelet_name(), delta));
            }
        }
        deltas
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::XmlFragment;
    use crate::operation::WaveletOperationKind;

    fn alice() -> ParticipantId {
        ParticipantId::of("alice@example.com").unwrap()
    }

    fn bob() -> ParticipantId {
        ParticipantId::of("bob@other.org").unwrap()
    }

    fn view() -> WaveView {
        WaveView::new(WaveId::of("example.com", "w+1").unwrap(), alice(), 1000)
    }

    #[test]
    fn mutations_are_applied_and_recorded() {
        let mut view = view();
        let wavelet = view.create_root().unwrap();
        let version_zero = wavelet.data().hashed_version().clone();
        assert_eq!(0, version_zero.version());
        wavelet
            .edit_document("b+1", |doc| {
                doc.append_xml(&XmlFragment::text("hi").wrap("p")).map(drop)
            })
            .unwrap();
        wavelet.touch().unwrap();
        // Applied locally: content and times change, versions do not.
        assert_eq!("<p>hi</p>", wavelet.document("b+1").to_xml_string());
        assert_eq!(&[alice()], wavelet.participants());
        assert_eq!(0, wavelet.data().version());
        assert_eq!(&version_zero, wavelet.data().hashed_version());
        let blip = wavelet.blip("b+1").unwrap();
        assert_eq!(
            (&alice(), 1000, 0),
            (
                blip.author(),
                blip.last_modified_time(),
                blip.last_modified_version()
            )
        );
        assert_eq!(3, wavelet.ops().len());
        for op in wavelet.ops() {
            assert_eq!(&alice(), op.context().creator());
            assert_eq!(
                (1000, 0),
                (op.context().timestamp(), op.context().version_increment())
            );
            assert!(!op.context().has_hashed_version());
        }
        let delta = wavelet.take_delta().unwrap();
        assert_eq!(
            (&alice(), &version_zero, 3),
            (delta.author(), delta.target_version(), delta.len())
        );
        assert!(matches!(
            delta[0].kind(),
            WaveletOperationKind::AddParticipant { .. }
        ));
        assert!(matches!(delta[2].kind(), WaveletOperationKind::NoOp));
        assert!(wavelet.ops().is_empty());
        assert!(wavelet.take_delta().is_none());
    }

    #[test]
    fn missing_documents_read_as_empty() {
        let mut view = view();
        let wavelet = view.create_root().unwrap();
        assert!(!wavelet.has_document("nope"));
        assert!(wavelet.blip("nope").is_none());
        assert_eq!("", wavelet.document("nope").to_xml_string());
        assert_eq!(0, wavelet.document_view("nope").size());
        // Reading creates nothing.
        assert!(!wavelet.has_document("nope"));
    }

    #[test]
    fn failed_edit_records_nothing() {
        let mut view = view();
        let wavelet = view.create_root().unwrap();
        wavelet.take_ops();
        assert!(
            wavelet
                .edit_document("b+1", |doc| doc.insert_text(5, "x"))
                .is_err()
        );
        assert!(wavelet.ops().is_empty());
        assert!(matches!(
            wavelet.apply_doc_op(
                "b+1",
                crate::docop::DocOpBuilder::new()
                    .retain(3)
                    .build_unchecked()
            ),
            Err(WaveletError::Operation(_))
        ));
        assert!(wavelet.ops().is_empty());
    }

    #[test]
    fn participation_helpers() {
        let data = {
            let mut view = view();
            view.create_root().unwrap().data().clone()
        };
        // Auto: the author joins first.
        let mut auto = OpBasedWavelet::new(data.clone(), bob(), 5);
        auto.touch().unwrap();
        assert_eq!(&[alice(), bob()], auto.participants());
        assert_eq!(2, auto.ops().len());
        // Ignorant: the operation goes out as it is.
        let mut ignorant = OpBasedWavelet::new(data.clone(), bob(), 5)
            .with_participation_helper(ParticipationHelper::Ignorant);
        ignorant.touch().unwrap();
        assert_eq!(&[alice()], ignorant.participants());
        assert_eq!(1, ignorant.ops().len());
        // Read only.
        let mut read_only = OpBasedWavelet::new(data, bob(), 5)
            .with_participation_helper(ParticipationHelper::ReadOnly);
        assert_eq!(Err(WaveletError::ReadOnly(bob())), read_only.touch());
        assert!(
            read_only
                .edit_document("d", |doc| doc.insert_text(0, "x"))
                .is_err()
        );
        assert!(read_only.add_participant(&bob()).is_err());
        assert!(read_only.ops().is_empty());
        // Adding someone who is there is not a mutation.
        read_only.add_participant(&alice()).unwrap();
        assert!(read_only.ops().is_empty());
    }

    #[test]
    fn author_and_timestamp_can_change() {
        let mut view = view();
        let wavelet = view.create_root().unwrap();
        wavelet.take_delta();
        wavelet.set_author(bob());
        wavelet.set_timestamp(7);
        wavelet.touch().unwrap();
        assert_eq!((&bob(), 7), (wavelet.author(), wavelet.timestamp()));
        let delta = wavelet.take_delta().unwrap();
        assert_eq!(&bob(), delta.author());
        // The auto-add of bob and the no-op.
        assert_eq!(2, delta.len());
        assert_eq!(7, delta[0].context().timestamp());
        wavelet.data_mut().set_version(9);
        assert_eq!(9, wavelet.data().version());
    }

    #[test]
    fn wave_view_wavelets() {
        let mut view = view();
        assert!(view.root().is_none() && view.user_data().is_none());
        assert_eq!("conv+root", view.root_id().unwrap().id());
        view.create_root().unwrap();
        assert!(matches!(
            view.create_root(),
            Err(WaveletError::DuplicateWavelet(_))
        ));
        let mut ids = crate::id::IdGeneratorImpl::new("example.com", || "x".to_owned());
        let conversation = view
            .create_conversation_wavelet(&mut ids)
            .unwrap()
            .data()
            .wavelet_id()
            .clone();
        assert!(conversation.id().starts_with("conv+x"));
        let udw = view.create_user_data().unwrap().data().wavelet_id().clone();
        assert_eq!(
            ("example.com", "user+alice@example.com"),
            (udw.domain(), udw.id())
        );
        assert_eq!(&udw, view.user_data().unwrap().data().wavelet_id());
        assert!(view.user_data_mut().is_some());
        assert_eq!(3, view.wavelet_ids().count());
        assert_eq!(&alice(), view.viewer());
        // One delta per wavelet: the creator joining each.
        let deltas = view.take_deltas();
        assert_eq!(3, deltas.len());
        for (name, delta) in &deltas {
            assert_eq!(view.wave_id(), &name.wave_id);
            assert_eq!(1, delta.len());
            assert_eq!(
                &HashedVersionFactory::create_version_zero(name),
                delta.target_version()
            );
        }
        assert!(view.take_deltas().is_empty());
        // The user-data wavelet of a viewer from another domain lives in
        // that domain.
        let other = WaveView::new(WaveId::of("example.com", "w+1").unwrap(), bob(), 0);
        assert_eq!("other.org", other.user_data_id().unwrap().domain());
        // Wrapping existing data replaces the wavelet.
        let data = view.wavelet(&udw).unwrap().data().clone();
        view.wavelet_mut(&udw).unwrap().touch().unwrap();
        assert!(view.add_wavelet(data).ops().is_empty());
    }
}
