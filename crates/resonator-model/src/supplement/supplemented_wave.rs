//! Supplement state combined with the wave it is about
//! (`SupplementedWaveImpl`).

use std::collections::BTreeMap;

use super::{
    NO_VERSION, PrimitiveSupplement, ReadablePrimitiveSupplement, SupplementError, SupplementImpl,
    ThreadState, WantedEvaluation, WantedEvaluationSet,
};
use crate::data::WaveletData;
use crate::doc::DocView;
use crate::id::constants::MANIFEST_DOCUMENT_ID;
use crate::id::util::{is_blip_id, is_conversational_id};
use crate::id::{ParticipantId, WaveletId};
use crate::version::HashedVersion;

/// The legacy inbox folder. Moving a wave there puts it in the inbox; the
/// folder itself is never stored.
pub const INBOX_FOLDER: i32 = 1;
/// The legacy "all" folder. Moving a wave there archives it and removes it
/// from all folders; the folder itself is never stored.
pub const ALL_FOLDER: i32 = 3;
/// The folder of trashed waves.
pub const TRASH_FOLDER: i32 = 8;

/// Where a wave shows up for a user (`conversation.InboxState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InboxState {
    Inbox,
    Mute,
    Archive,
}

/// What the supplement needs to know about the conversational wavelets of
/// the wave (`SupplementWaveView`).
pub trait SupplementWaveView {
    /// The ids of the conversational wavelets in view (`getWavelets`).
    /// Ids that are not conversational are disregarded by
    /// [`SupplementedWaveImpl`] (the original throws when it meets one).
    fn wavelets(&self) -> Vec<WaveletId>;

    /// The version of a wavelet, or -1 ([`NO_VERSION`]) if it is not in
    /// view (`getVersion`).
    fn version(&self, wavelet_id: &WaveletId) -> i64;

    /// The hashed version of a wavelet; version zero without hash if it is
    /// not in view (`getSignature`).
    fn signature(&self, wavelet_id: &WaveletId) -> HashedVersion;

    /// The last-modified version of every blip of a wavelet
    /// (`getBlipVersions`).
    fn blip_versions(&self, wavelet_id: &WaveletId) -> BTreeMap<String, i64>;

    /// The last-modified version of one blip, if the wavelet has it. Stands
    /// in for the blip objects the original is handed.
    fn blip_version(&self, wavelet_id: &WaveletId, blip_id: &str) -> Option<i64> {
        self.blip_versions(wavelet_id).get(blip_id).copied()
    }

    /// Whether the viewer is a participant of any of the wavelets
    /// (`isExplicitParticipant`).
    fn is_explicit_participant(&self) -> bool;
}

impl<V: SupplementWaveView + ?Sized> SupplementWaveView for &V {
    fn wavelets(&self) -> Vec<WaveletId> {
        (**self).wavelets()
    }

    fn version(&self, wavelet_id: &WaveletId) -> i64 {
        (**self).version(wavelet_id)
    }

    fn signature(&self, wavelet_id: &WaveletId) -> HashedVersion {
        (**self).signature(wavelet_id)
    }

    fn blip_versions(&self, wavelet_id: &WaveletId) -> BTreeMap<String, i64> {
        (**self).blip_versions(wavelet_id)
    }

    fn blip_version(&self, wavelet_id: &WaveletId, blip_id: &str) -> Option<i64> {
        (**self).blip_version(wavelet_id, blip_id)
    }

    fn is_explicit_participant(&self) -> bool {
        (**self).is_explicit_participant()
    }
}

/// A [`SupplementWaveView`] over wavelet data
/// (`SupplementedWaveImpl.WaveViewAdapter`, which sits on the conversation
/// model).
///
/// In view are the wavelets with a conversational id whose manifest
/// document starts with a `conversation` element, which is what the
/// original's conversation view takes for conversations. They are listed in
/// order of id (the original: hash order). The blips of a wavelet are taken
/// to be all its documents with a blip id, whereas the original walks the
/// manifest; the difference only shows in [`Self::blip_versions`], which
/// the supplement does not use.
#[derive(Debug, Clone)]
pub struct WaveletDataView<'a> {
    viewer: ParticipantId,
    wavelets: BTreeMap<WaveletId, &'a WaveletData>,
}

fn has_conversation(wavelet: &WaveletData) -> bool {
    if !is_conversational_id(wavelet.wavelet_id()).unwrap_or(false) {
        return false;
    }
    let Some(manifest) = wavelet.document(MANIFEST_DOCUMENT_ID) else {
        return false;
    };
    let view = DocView::new(manifest.content());
    view.first_child_element(view.root())
        .is_some_and(|top| view.has_tag(top, "conversation"))
}

impl<'a> WaveletDataView<'a> {
    /// A view for `viewer` of the conversations among `wavelets`; other
    /// wavelets are left out.
    pub fn new(viewer: ParticipantId, wavelets: impl IntoIterator<Item = &'a WaveletData>) -> Self {
        WaveletDataView {
            viewer,
            wavelets: wavelets
                .into_iter()
                .filter(|wavelet| has_conversation(wavelet))
                .map(|wavelet| (wavelet.wavelet_id().clone(), wavelet))
                .collect(),
        }
    }
}

impl SupplementWaveView for WaveletDataView<'_> {
    fn wavelets(&self) -> Vec<WaveletId> {
        self.wavelets.keys().cloned().collect()
    }

    fn version(&self, wavelet_id: &WaveletId) -> i64 {
        match self.wavelets.get(wavelet_id) {
            Some(wavelet) => wavelet.version() as i64,
            None => i64::from(NO_VERSION),
        }
    }

    fn signature(&self, wavelet_id: &WaveletId) -> HashedVersion {
        match self.wavelets.get(wavelet_id) {
            Some(wavelet) => wavelet.hashed_version().clone(),
            None => HashedVersion::unsigned(0),
        }
    }

    fn blip_versions(&self, wavelet_id: &WaveletId) -> BTreeMap<String, i64> {
        let Some(wavelet) = self.wavelets.get(wavelet_id) else {
            return BTreeMap::new();
        };
        wavelet
            .documents()
            .filter(|document| is_blip_id(document.id()))
            .map(|document| {
                (
                    document.id().to_owned(),
                    document.last_modified_version() as i64,
                )
            })
            .collect()
    }

    fn blip_version(&self, wavelet_id: &WaveletId, blip_id: &str) -> Option<i64> {
        let document = self.wavelets.get(wavelet_id)?.document(blip_id)?;
        Some(document.last_modified_version() as i64)
    }

    fn is_explicit_participant(&self) -> bool {
        self.wavelets
            .values()
            .any(|wavelet| wavelet.participants().contains(&self.viewer))
    }
}

/// Whether a user follows a wave they have neither followed nor unfollowed
/// (`SupplementedWaveImpl.DefaultFollow`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefaultFollow {
    /// Only if the user is a participant of one of its conversations
    /// (`WHEN_PARTICIPANT`).
    WhenParticipant,
    /// Always (`ALWAYS`); what the server uses for digests.
    Always,
}

impl DefaultFollow {
    pub fn is_followed(self, wave: &(impl SupplementWaveView + ?Sized)) -> bool {
        match self {
            DefaultFollow::WhenParticipant => wave.is_explicit_participant(),
            DefaultFollow::Always => true,
        }
    }
}

/// The supplement of a wave, interpreted against the wave
/// (`SupplementedWave`, `SupplementedWaveImpl`).
///
/// `P` is the raw state: a [`ReadablePrimitiveSupplement`] suffices for the
/// queries, the changes need a [`PrimitiveSupplement`]. Where the original
/// takes wavelet, blip and thread objects, the methods take ids and ask the
/// view `V` for versions.
///
/// Java narrows wavelet versions to `int` before comparing them with stored
/// versions; so does this.
#[derive(Debug, Clone)]
pub struct SupplementedWaveImpl<P, V> {
    supplement: SupplementImpl<P>,
    wave: V,
    follow_policy: DefaultFollow,
}

impl<P, V: SupplementWaveView> SupplementedWaveImpl<P, V> {
    /// `SupplementedWaveImpl.create`.
    pub fn new(primitive: P, wave: V, follow_policy: DefaultFollow) -> Self {
        SupplementedWaveImpl {
            supplement: SupplementImpl::new(primitive),
            wave,
            follow_policy,
        }
    }

    /// The interpretation of the raw state that does not involve the wave.
    pub fn supplement(&self) -> &SupplementImpl<P> {
        &self.supplement
    }

    pub fn supplement_mut(&mut self) -> &mut SupplementImpl<P> {
        &mut self.supplement
    }

    pub fn wave(&self) -> &V {
        &self.wave
    }

    pub fn into_primitive(self) -> P {
        self.supplement.into_primitive()
    }

    /// The conversational wavelets in view.
    fn wavelets(&self) -> Vec<WaveletId> {
        let mut wavelets = self.wave.wavelets();
        wavelets.retain(|id| is_conversational_id(id).unwrap_or(false));
        wavelets
    }

    fn version(&self, wavelet_id: &WaveletId) -> i32 {
        self.wave.version(wavelet_id) as i32
    }
}

impl<P: ReadablePrimitiveSupplement, V: SupplementWaveView> SupplementedWaveImpl<P, V> {
    /// `getThreadState`.
    pub fn thread_state(&self, wavelet_id: &WaveletId, thread_id: &str) -> Option<ThreadState> {
        self.supplement.thread_state(wavelet_id, thread_id)
    }

    /// Whether a blip changed since it was last read (`isUnread`). False
    /// for a blip the view does not know.
    pub fn is_unread(&self, wavelet_id: &WaveletId, blip_id: &str) -> bool {
        match self.wave.blip_version(wavelet_id, blip_id) {
            Some(version) => self
                .supplement
                .is_blip_unread(wavelet_id, blip_id, version as i32),
            None => false,
        }
    }

    /// Whether the participants of a wavelet may have changed since they
    /// were last read: whether the wavelet has (`isParticipantsUnread`).
    pub fn is_participants_unread(&self, wavelet_id: &WaveletId) -> bool {
        self.supplement
            .is_participants_unread(wavelet_id, self.version(wavelet_id))
    }

    /// `haveParticipantsEverBeenRead`.
    pub fn have_participants_ever_been_read(&self, wavelet_id: &WaveletId) -> bool {
        self.supplement.have_participants_ever_been_read(wavelet_id)
    }

    /// Whether the tags of a wavelet may have changed since they were last
    /// read: whether the wavelet has (`isTagsUnread`).
    pub fn is_tags_unread(&self, wavelet_id: &WaveletId) -> bool {
        self.supplement
            .is_tags_unread(wavelet_id, self.version(wavelet_id))
    }

    /// `getFolders`.
    pub fn folders(&self) -> Vec<i32> {
        self.supplement.folders()
    }

    /// Whether the wave is in the inbox: followed and not archived
    /// (`isInbox`).
    pub fn is_inbox(&self) -> bool {
        self.is_followed() && !self.is_archived()
    }

    /// Whether every wavelet in view was archived at its current version
    /// or later (`isArchived`). True for a wave without conversations; any
    /// change to a wavelet after archiving makes it false again.
    pub fn is_archived(&self) -> bool {
        self.wavelets()
            .iter()
            .all(|id| self.supplement.is_archived(id, self.version(id)))
    }

    /// `isMute`: not followed.
    pub fn is_mute(&self) -> bool {
        !self.is_followed()
    }

    /// Whether the user follows the wave, explicitly or by the default
    /// policy (`isFollowed`).
    pub fn is_followed(&self) -> bool {
        self.supplement
            .is_followed(self.follow_policy.is_followed(&self.wave))
    }

    /// Whether the wave is in the trash folder (`isTrashed`).
    pub fn is_trashed(&self) -> bool {
        self.supplement.folders().contains(&TRASH_FOLDER)
    }

    /// Muted if not followed, else archived if [`Self::is_archived`], else
    /// in the inbox. The original has no such method; this is the reading
    /// its documentation of `PrimitiveSupplement` gives.
    pub fn inbox_state(&self) -> InboxState {
        if !self.is_followed() {
            InboxState::Mute
        } else if self.is_archived() {
            InboxState::Archive
        } else {
            InboxState::Inbox
        }
    }

    /// `getWantedEvaluationSet`.
    pub fn wanted_evaluation_set(&self, wavelet_id: &WaveletId) -> WantedEvaluationSet {
        self.supplement.wanted_evaluation_set(wavelet_id)
    }

    /// `getSeenVersion`.
    pub fn seen_version(&self, wavelet_id: &WaveletId) -> HashedVersion {
        self.supplement.seen_version(wavelet_id)
    }

    /// Whether any wavelet in view was seen at a version above zero
    /// (`hasBeenSeen`).
    pub fn has_been_seen(&self) -> bool {
        self.wavelets()
            .iter()
            .any(|id| self.supplement.seen_version(id).version() > 0)
    }

    /// Whether any wavelet in view was notified at a later version than it
    /// was seen at; if no wavelet has a notified version, the pending flag
    /// (`hasPendingNotification`).
    pub fn has_pending_notification(&self) -> bool {
        if !self.supplement.has_notified_version() {
            return self.supplement.has_pending_notification();
        }
        self.wavelets()
            .iter()
            .any(|id| self.supplement.has_pending_notification_for(id))
    }

    /// `getGadgetState`.
    pub fn gadget_state(&self, gadget_id: &str) -> BTreeMap<String, String> {
        self.supplement.gadget_state(gadget_id)
    }

    /// `getGadgetStateValue`.
    pub fn gadget_state_value(&self, gadget_id: &str, key: &str) -> Option<String> {
        self.supplement.gadget_state(gadget_id).remove(key)
    }
}

impl<P: PrimitiveSupplement, V: SupplementWaveView> SupplementedWaveImpl<P, V> {
    /// `setThreadState`.
    pub fn set_thread_state(
        &mut self,
        wavelet_id: &WaveletId,
        thread_id: &str,
        state: Option<ThreadState>,
    ) -> Result<(), SupplementError> {
        self.supplement
            .set_thread_state(wavelet_id, thread_id, state)
    }

    /// Marks every wavelet in view as read at its current version
    /// (`markAsRead()`).
    pub fn mark_as_read(&mut self) -> Result<(), SupplementError> {
        for id in self.wavelets() {
            let version = self.version(&id);
            self.supplement.mark_wavelet_as_read(&id, version)?;
        }
        Ok(())
    }

    /// Marks a blip as read, if it is unread, at the current version of
    /// its wavelet (`markAsRead(ConversationBlip)`). Does nothing for a
    /// blip the view does not know.
    pub fn mark_blip_as_read(
        &mut self,
        wavelet_id: &WaveletId,
        blip_id: &str,
    ) -> Result<(), SupplementError> {
        let Some(modified) = self.wave.blip_version(wavelet_id, blip_id) else {
            return Ok(());
        };
        if self
            .supplement
            .is_blip_unread(wavelet_id, blip_id, modified as i32)
        {
            let version = modified.max(self.wave.version(wavelet_id)) as i32;
            self.supplement
                .mark_blip_as_read(wavelet_id, blip_id, version)?;
        }
        Ok(())
    }

    /// `markParticipantAsRead`.
    pub fn mark_participants_as_read(
        &mut self,
        wavelet_id: &WaveletId,
    ) -> Result<(), SupplementError> {
        let version = self.version(wavelet_id);
        self.supplement
            .mark_participants_as_read(wavelet_id, version)
    }

    /// `markTagsAsRead`.
    pub fn mark_tags_as_read(&mut self, wavelet_id: &WaveletId) -> Result<(), SupplementError> {
        let version = self.version(wavelet_id);
        self.supplement.mark_tags_as_read(wavelet_id, version)
    }

    /// Makes the whole wave unread (`markAsUnread`).
    pub fn mark_as_unread(&mut self) -> Result<(), SupplementError> {
        self.supplement.mark_as_unread()
    }

    /// `mute`: the same as [`Self::unfollow`].
    pub fn mute(&mut self) -> Result<(), SupplementError> {
        self.unfollow()
    }

    /// `follow`.
    pub fn follow(&mut self) -> Result<(), SupplementError> {
        self.supplement.follow()
    }

    /// Stops following the wave, which also clears its archive state
    /// (`unfollow`).
    pub fn unfollow(&mut self) -> Result<(), SupplementError> {
        self.supplement.unfollow()
    }

    /// Moves the wave to a folder (`moveToFolder`): the inbox folder means
    /// [`Self::inbox`]; the "all" folder archives the wave and removes it
    /// from all folders; any other folder archives the wave and becomes its
    /// only folder.
    pub fn move_to_folder(&mut self, folder_id: i32) -> Result<(), SupplementError> {
        match folder_id {
            INBOX_FOLDER => self.inbox(),
            ALL_FOLDER => {
                self.archive()?;
                self.supplement.remove_all_folders()
            }
            _ => {
                self.archive()?;
                self.supplement.move_to_folder(folder_id)
            }
        }
    }

    /// Puts the wave in the inbox: follows it, removes it from all folders
    /// and clears its archive state (`inbox`).
    pub fn inbox(&mut self) -> Result<(), SupplementError> {
        self.follow()?;
        self.supplement.remove_all_folders()?;
        self.supplement.clear_archive()
    }

    /// Records the current hashed version of every wavelet in view as seen
    /// and clears the pending notification (`see()`).
    pub fn see(&mut self) -> Result<(), SupplementError> {
        for id in self.wavelets() {
            let signature = self.wave.signature(&id);
            self.supplement.set_seen_version(&id, &signature)?;
        }
        self.supplement.clear_pending_notification()
    }

    /// The same for one wavelet (`see(Wavelet)`).
    pub fn see_wavelet(&mut self, wavelet_id: &WaveletId) -> Result<(), SupplementError> {
        let signature = self.wave.signature(wavelet_id);
        self.supplement.set_seen_version(wavelet_id, &signature)?;
        self.supplement.clear_pending_notification()
    }

    /// Archives every wavelet in view at its current version, if the wave
    /// is followed; does nothing otherwise (`archive`).
    pub fn archive(&mut self) -> Result<(), SupplementError> {
        if self.is_followed() {
            for id in self.wavelets() {
                let version = self.version(&id);
                self.supplement.archive(&id, version)?;
            }
        }
        Ok(())
    }

    /// `addWantedEvaluation`.
    pub fn add_wanted_evaluation(
        &mut self,
        evaluation: &WantedEvaluation,
    ) -> Result<(), SupplementError> {
        self.supplement.add_wanted_evaluation(evaluation)
    }

    /// Records the current version of every wavelet in view as notified
    /// (`markAsNotified`).
    pub fn mark_as_notified(&mut self) -> Result<(), SupplementError> {
        for id in self.wavelets() {
            let version = self.version(&id);
            self.supplement.mark_wavelet_as_notified(&id, version)?;
        }
        Ok(())
    }

    /// `setGadgetState`.
    pub fn set_gadget_state(
        &mut self,
        gadget_id: &str,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), SupplementError> {
        self.supplement.set_gadget_state(gadget_id, key, value)
    }
}
