//! The meaning of the raw supplement state (`SupplementImpl`).

use std::collections::BTreeMap;

use super::{
    NO_VERSION, PrimitiveSupplement, ReadablePrimitiveSupplement, SupplementError, ThreadState,
    WantedEvaluation, WantedEvaluationSet,
};
use crate::id::WaveletId;
use crate::version::HashedVersion;

/// Interprets raw supplement state (`Supplement`, `SupplementImpl`). The
/// queries need a readable state, the changes a writable one.
#[derive(Debug, Clone)]
pub struct SupplementImpl<P> {
    primitive: P,
}

fn is_unread(read: i32, modified: i32) -> bool {
    read == NO_VERSION || read < modified
}

impl<P> SupplementImpl<P> {
    pub fn new(primitive: P) -> Self {
        SupplementImpl { primitive }
    }

    pub fn primitive(&self) -> &P {
        &self.primitive
    }

    pub fn primitive_mut(&mut self) -> &mut P {
        &mut self.primitive
    }

    pub fn into_primitive(self) -> P {
        self.primitive
    }
}

impl<P: ReadablePrimitiveSupplement> SupplementImpl<P> {
    /// `getThreadState`.
    pub fn thread_state(&self, wavelet_id: &WaveletId, thread_id: &str) -> Option<ThreadState> {
        self.primitive.thread_state(wavelet_id, thread_id)
    }

    /// Whether a blip last modified at `version` is unread: neither the
    /// blip nor its whole wavelet was marked as read at that version or
    /// later (`isBlipUnread`).
    pub fn is_blip_unread(&self, wavelet_id: &WaveletId, blip_id: &str, version: i32) -> bool {
        is_unread(
            self.primitive.last_read_blip_version(wavelet_id, blip_id),
            version,
        ) && is_unread(
            self.primitive.last_read_wavelet_version(wavelet_id),
            version,
        )
    }

    /// Whether the participants of a wavelet now at `version` are unread
    /// (`isParticipantsUnread`).
    pub fn is_participants_unread(&self, wavelet_id: &WaveletId, version: i32) -> bool {
        is_unread(
            self.primitive.last_read_participants_version(wavelet_id),
            version,
        ) && is_unread(
            self.primitive.last_read_wavelet_version(wavelet_id),
            version,
        )
    }

    /// `haveParticipantsEverBeenRead`.
    pub fn have_participants_ever_been_read(&self, wavelet_id: &WaveletId) -> bool {
        self.primitive.last_read_participants_version(wavelet_id) != NO_VERSION
            || self.primitive.last_read_wavelet_version(wavelet_id) != NO_VERSION
    }

    /// Whether the tags of a wavelet now at `version` are unread
    /// (`isTagsUnread`).
    pub fn is_tags_unread(&self, wavelet_id: &WaveletId, version: i32) -> bool {
        is_unread(self.primitive.last_read_tags_version(wavelet_id), version)
            && is_unread(
                self.primitive.last_read_wavelet_version(wavelet_id),
                version,
            )
    }

    /// `getFolders`.
    pub fn folders(&self) -> Vec<i32> {
        self.primitive.folders()
    }

    /// Whether a wavelet now at `version` is archived: it was archived at
    /// that version or later (`isArchived`).
    pub fn is_archived(&self, wavelet_id: &WaveletId, version: i32) -> bool {
        self.primitive.archive_wavelet_version(wavelet_id) >= version
    }

    /// The explicit follow state, or the default if there is none
    /// (`isFollowed`).
    pub fn is_followed(&self, default_followed: bool) -> bool {
        self.primitive.followed().unwrap_or(default_followed)
    }

    /// `getSeenVersion`.
    pub fn seen_version(&self, wavelet_id: &WaveletId) -> HashedVersion {
        self.primitive.seen_version(wavelet_id)
    }

    /// `getNotifiedVersion`.
    pub fn notified_version(&self, wavelet_id: &WaveletId) -> i32 {
        self.primitive.notified_version(wavelet_id)
    }

    /// `getSeenWavelets`.
    pub fn seen_wavelets(&self) -> Vec<WaveletId> {
        self.primitive.seen_wavelets()
    }

    /// `hasSeenVersion`.
    pub fn has_seen_version(&self) -> bool {
        !self.primitive.seen_wavelets().is_empty()
    }

    /// The evaluations of one wavelet (`getWantedEvaluationSet`).
    pub fn wanted_evaluation_set(&self, wavelet_id: &WaveletId) -> WantedEvaluationSet {
        let relevant = self
            .primitive
            .wanted_evaluations()
            .into_iter()
            .filter(|evaluation| evaluation.wavelet_id.as_ref() == Some(wavelet_id));
        WantedEvaluationSet::new(wavelet_id.clone(), relevant)
    }

    /// `hasPendingNotification()`.
    pub fn has_pending_notification(&self) -> bool {
        self.primitive.pending_notification()
    }

    /// Whether a wavelet was notified at a later version than it was seen
    /// at; if no wavelet has a notified version, the pending flag
    /// (`hasPendingNotification(WaveletId)`).
    pub fn has_pending_notification_for(&self, wavelet_id: &WaveletId) -> bool {
        if !self.has_notified_version() {
            return self.has_pending_notification();
        }
        let seen = self.seen_version(wavelet_id).version();
        i64::try_from(seen).unwrap_or(i64::MAX) < i64::from(self.notified_version(wavelet_id))
    }

    /// `hasNotifiedVersion`.
    pub fn has_notified_version(&self) -> bool {
        !self.primitive.notified_wavelets().is_empty()
    }

    /// `getGadgetState`.
    pub fn gadget_state(&self, gadget_id: &str) -> BTreeMap<String, String> {
        self.primitive.gadget_state(gadget_id)
    }
}

impl<P: PrimitiveSupplement> SupplementImpl<P> {
    /// `setThreadState`.
    pub fn set_thread_state(
        &mut self,
        wavelet_id: &WaveletId,
        thread_id: &str,
        state: Option<ThreadState>,
    ) -> Result<(), SupplementError> {
        self.primitive
            .set_thread_state(wavelet_id, thread_id, state)
    }

    /// Records that a blip was read at `version`, if it is unread at that
    /// version (`markBlipAsRead`).
    pub fn mark_blip_as_read(
        &mut self,
        wavelet_id: &WaveletId,
        blip_id: &str,
        version: i32,
    ) -> Result<(), SupplementError> {
        if self.is_blip_unread(wavelet_id, blip_id, version) {
            self.primitive
                .set_last_read_blip_version(wavelet_id, blip_id, version)?;
        }
        Ok(())
    }

    /// `markParticipantsAsRead`.
    pub fn mark_participants_as_read(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        if self.is_participants_unread(wavelet_id, version) {
            self.primitive
                .set_last_read_participants_version(wavelet_id, version)?;
        }
        Ok(())
    }

    /// `markTagsAsRead`.
    pub fn mark_tags_as_read(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        if self.is_tags_unread(wavelet_id, version) {
            self.primitive
                .set_last_read_tags_version(wavelet_id, version)?;
        }
        Ok(())
    }

    /// Records that everything in a wavelet was read at `version`
    /// (`markWaveletAsRead`).
    pub fn mark_wavelet_as_read(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        self.primitive
            .set_last_read_wavelet_version(wavelet_id, version)
    }

    /// Forgets all read state, making the whole wave unread
    /// (`markAsUnread`).
    pub fn mark_as_unread(&mut self) -> Result<(), SupplementError> {
        self.primitive.clear_read_state()
    }

    /// Makes a folder the only folder of the wave (`moveToFolder`).
    pub fn move_to_folder(&mut self, id: i32) -> Result<(), SupplementError> {
        self.primitive.remove_all_folders()?;
        self.primitive.add_folder(id)
    }

    /// `removeAllFolders`.
    pub fn remove_all_folders(&mut self) -> Result<(), SupplementError> {
        self.primitive.remove_all_folders()
    }

    /// `follow`.
    pub fn follow(&mut self) -> Result<(), SupplementError> {
        self.primitive.follow()
    }

    /// Unfollows the wave, clearing the archive state first (`unfollow`).
    pub fn unfollow(&mut self) -> Result<(), SupplementError> {
        self.primitive.clear_archive_state()?;
        self.primitive.unfollow()
    }

    /// `clearArchive`.
    pub fn clear_archive(&mut self) -> Result<(), SupplementError> {
        self.primitive.clear_archive_state()
    }

    /// `archive`.
    pub fn archive(&mut self, wavelet_id: &WaveletId, version: i32) -> Result<(), SupplementError> {
        self.primitive.archive_at_version(wavelet_id, version)
    }

    /// `setSeenVersion`.
    pub fn set_seen_version(
        &mut self,
        wavelet_id: &WaveletId,
        signature: &HashedVersion,
    ) -> Result<(), SupplementError> {
        self.primitive.set_seen_version(wavelet_id, signature)
    }

    /// `addWantedEvaluation`.
    pub fn add_wanted_evaluation(
        &mut self,
        evaluation: &WantedEvaluation,
    ) -> Result<(), SupplementError> {
        self.primitive.add_wanted_evaluation(evaluation)
    }

    /// `markWaveletAsNotified`.
    pub fn mark_wavelet_as_notified(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        self.primitive.set_notified_version(wavelet_id, version)
    }

    /// `clearPendingNotification`.
    pub fn clear_pending_notification(&mut self) -> Result<(), SupplementError> {
        self.primitive.clear_pending_notification()
    }

    /// `setGadgetState`.
    pub fn set_gadget_state(
        &mut self,
        gadget_id: &str,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), SupplementError> {
        self.primitive.set_gadget_state(gadget_id, key, value)
    }
}
