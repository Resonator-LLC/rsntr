//! Supplement state held in memory (`PrimitiveSupplementImpl`).

use std::collections::{BTreeMap, BTreeSet};

use super::{
    NO_VERSION, PrimitiveSupplement, ReadablePrimitiveSupplement, SupplementError, ThreadState,
    WantedEvaluation,
};
use crate::id::WaveletId;
use crate::version::HashedVersion;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct WaveletReadState {
    blip_versions: BTreeMap<String, i32>,
    wavelet_version: Option<i32>,
    participants_version: Option<i32>,
    tags_version: Option<i32>,
}

/// Supplement state held in memory: a stand-in where a user has no
/// user-data wavelet, and a cheap snapshot of one that does
/// ([`PrimitiveSupplementImpl::copy_of`]).
///
/// Quirks of the original that are kept:
///
/// * The last-read version of a whole wavelet, once set, is only replaced
///   when it is less than the last-read *participants* version of the
///   wavelet (the original compares the wrong field). If there is no
///   participants version at that point the original throws a
///   `NullPointerException`; here [`SupplementError::IllegalState`] is
///   returned.
/// * Archive versions are simply overwritten, not kept monotonic, and
///   clearing the archive state forgets them, unlike the wavelet-based
///   supplement.
/// * Clearing the pending notification is all that can be done to it: it
///   can only be set by copying.
///
/// Deviations: reading a thread state or gadget state does not create an
/// empty entry for the wavelet or gadget (in the original it does, which
/// shows in `getWaveletsWithThreadState`), and evaluations are kept in the
/// order added rather than in a hash set.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrimitiveSupplementImpl {
    read_states: BTreeMap<WaveletId, WaveletReadState>,
    thread_states: BTreeMap<WaveletId, BTreeMap<String, ThreadState>>,
    folders: BTreeSet<i32>,
    archive_versions: BTreeMap<WaveletId, i32>,
    seen_versions: BTreeMap<WaveletId, HashedVersion>,
    wanted_evaluations: Vec<WantedEvaluation>,
    follow: Option<bool>,
    pending_notification: bool,
    notified_versions: BTreeMap<WaveletId, i32>,
    gadget_states: BTreeMap<String, BTreeMap<String, String>>,
}

impl PrimitiveSupplementImpl {
    pub fn new() -> Self {
        Self::default()
    }

    /// A copy of the state of another supplement
    /// (`PrimitiveSupplementImpl(PrimitiveSupplement)`). As in the
    /// original, gadget states are not copied.
    pub fn copy_of(other: &(impl ReadablePrimitiveSupplement + ?Sized)) -> Self {
        let mut copy = Self::new();
        for wavelet in other.read_wavelets() {
            let state = copy.read_states.entry(wavelet.clone()).or_default();
            // Set directly: on a fresh state the original's setters do just
            // that.
            let version = |v: i32| (v != NO_VERSION).then_some(v);
            state.wavelet_version = version(other.last_read_wavelet_version(&wavelet));
            state.participants_version = version(other.last_read_participants_version(&wavelet));
            state.tags_version = version(other.last_read_tags_version(&wavelet));
            for blip in other.read_blips(&wavelet) {
                let version = other.last_read_blip_version(&wavelet, &blip);
                state.blip_versions.insert(blip, version);
            }
        }
        for wavelet in other.wavelets_with_thread_state() {
            let threads = copy.thread_states.entry(wavelet.clone()).or_default();
            for thread in other.stateful_threads(&wavelet) {
                if let Some(state) = other.thread_state(&wavelet, &thread) {
                    threads.insert(thread, state);
                }
            }
        }
        for wavelet in other.archive_wavelets() {
            let version = other.archive_wavelet_version(&wavelet);
            copy.archive_versions.insert(wavelet, version);
        }
        copy.folders.extend(other.folders());
        for evaluation in other.wanted_evaluations() {
            if !copy.wanted_evaluations.contains(&evaluation) {
                copy.wanted_evaluations.push(evaluation);
            }
        }
        for wavelet in other.seen_wavelets() {
            let version = other.seen_version(&wavelet);
            copy.seen_versions.insert(wavelet, version);
        }
        for wavelet in other.notified_wavelets() {
            let version = other.notified_version(&wavelet);
            copy.notified_versions.insert(wavelet, version);
        }
        copy.follow = other.followed();
        copy.pending_notification = other.pending_notification();
        copy
    }

    fn read_state(&self, wavelet_id: &WaveletId) -> Option<&WaveletReadState> {
        self.read_states.get(wavelet_id)
    }
}

impl ReadablePrimitiveSupplement for PrimitiveSupplementImpl {
    fn thread_state(&self, wavelet_id: &WaveletId, thread_id: &str) -> Option<ThreadState> {
        self.thread_states.get(wavelet_id)?.get(thread_id).copied()
    }

    fn wavelets_with_thread_state(&self) -> Vec<WaveletId> {
        self.thread_states.keys().cloned().collect()
    }

    fn stateful_threads(&self, wavelet_id: &WaveletId) -> Vec<String> {
        self.thread_states
            .get(wavelet_id)
            .map(|threads| threads.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn last_read_wavelet_version(&self, wavelet_id: &WaveletId) -> i32 {
        self.read_state(wavelet_id)
            .and_then(|state| state.wavelet_version)
            .unwrap_or(NO_VERSION)
    }

    fn last_read_blip_version(&self, wavelet_id: &WaveletId, blip_id: &str) -> i32 {
        self.read_state(wavelet_id)
            .and_then(|state| state.blip_versions.get(blip_id).copied())
            .unwrap_or(NO_VERSION)
    }

    fn last_read_participants_version(&self, wavelet_id: &WaveletId) -> i32 {
        self.read_state(wavelet_id)
            .and_then(|state| state.participants_version)
            .unwrap_or(NO_VERSION)
    }

    fn last_read_tags_version(&self, wavelet_id: &WaveletId) -> i32 {
        self.read_state(wavelet_id)
            .and_then(|state| state.tags_version)
            .unwrap_or(NO_VERSION)
    }

    fn read_wavelets(&self) -> Vec<WaveletId> {
        self.read_states.keys().cloned().collect()
    }

    fn read_blips(&self, wavelet_id: &WaveletId) -> Vec<String> {
        self.read_state(wavelet_id)
            .map(|state| state.blip_versions.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn folders(&self) -> Vec<i32> {
        self.folders.iter().copied().collect()
    }

    fn archive_wavelet_version(&self, wavelet_id: &WaveletId) -> i32 {
        self.archive_versions
            .get(wavelet_id)
            .copied()
            .unwrap_or(NO_VERSION)
    }

    fn archive_wavelets(&self) -> Vec<WaveletId> {
        self.archive_versions.keys().cloned().collect()
    }

    fn followed(&self) -> Option<bool> {
        self.follow
    }

    fn seen_version(&self, wavelet_id: &WaveletId) -> HashedVersion {
        self.seen_versions
            .get(wavelet_id)
            .cloned()
            .unwrap_or_else(|| HashedVersion::unsigned(0))
    }

    fn seen_wavelets(&self) -> Vec<WaveletId> {
        self.seen_versions.keys().cloned().collect()
    }

    fn wanted_evaluations(&self) -> Vec<WantedEvaluation> {
        self.wanted_evaluations.clone()
    }

    fn pending_notification(&self) -> bool {
        self.pending_notification
    }

    fn notified_wavelets(&self) -> Vec<WaveletId> {
        self.notified_versions.keys().cloned().collect()
    }

    fn notified_version(&self, wavelet_id: &WaveletId) -> i32 {
        self.notified_versions
            .get(wavelet_id)
            .copied()
            .unwrap_or(NO_VERSION)
    }

    fn gadget_state(&self, gadget_id: &str) -> BTreeMap<String, String> {
        self.gadget_states
            .get(gadget_id)
            .cloned()
            .unwrap_or_default()
    }
}

fn raise(slot: &mut Option<i32>, version: i32) {
    if slot.is_none_or(|current| current < version) {
        *slot = Some(version);
    }
}

impl PrimitiveSupplement for PrimitiveSupplementImpl {
    fn set_thread_state(
        &mut self,
        wavelet_id: &WaveletId,
        thread_id: &str,
        state: Option<ThreadState>,
    ) -> Result<(), SupplementError> {
        let threads = self.thread_states.entry(wavelet_id.clone()).or_default();
        match state {
            Some(state) => threads.insert(thread_id.to_owned(), state),
            None => threads.remove(thread_id),
        };
        Ok(())
    }

    fn set_last_read_wavelet_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        let state = self.read_states.entry(wavelet_id.clone()).or_default();
        match (state.wavelet_version, state.participants_version) {
            (None, _) => state.wavelet_version = Some(version),
            (Some(_), None) => {
                return Err(SupplementError::IllegalState(
                    "the wavelet has a last-read version but no last-read participants \
                     version to compare it with",
                ));
            }
            (Some(current), Some(participants)) if current < participants => {
                state.wavelet_version = Some(version);
            }
            _ => {}
        }
        Ok(())
    }

    fn set_last_read_blip_version(
        &mut self,
        wavelet_id: &WaveletId,
        blip_id: &str,
        version: i32,
    ) -> Result<(), SupplementError> {
        let state = self.read_states.entry(wavelet_id.clone()).or_default();
        let current = state.blip_versions.get(blip_id).copied();
        if current.is_none_or(|current| current < version) {
            state.blip_versions.insert(blip_id.to_owned(), version);
        }
        Ok(())
    }

    fn set_last_read_participants_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        let state = self.read_states.entry(wavelet_id.clone()).or_default();
        raise(&mut state.participants_version, version);
        Ok(())
    }

    fn set_last_read_tags_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        let state = self.read_states.entry(wavelet_id.clone()).or_default();
        raise(&mut state.tags_version, version);
        Ok(())
    }

    fn clear_read_state(&mut self) -> Result<(), SupplementError> {
        self.read_states.clear();
        Ok(())
    }

    fn clear_blip_read_state(
        &mut self,
        wavelet_id: &WaveletId,
        blip_id: &str,
    ) -> Result<(), SupplementError> {
        let state = self.read_states.entry(wavelet_id.clone()).or_default();
        state.blip_versions.remove(blip_id);
        Ok(())
    }

    fn add_folder(&mut self, id: i32) -> Result<(), SupplementError> {
        self.folders.insert(id);
        Ok(())
    }

    fn remove_folder(&mut self, id: i32) -> Result<(), SupplementError> {
        self.folders.remove(&id);
        Ok(())
    }

    fn remove_all_folders(&mut self) -> Result<(), SupplementError> {
        self.folders.clear();
        Ok(())
    }

    fn follow(&mut self) -> Result<(), SupplementError> {
        self.follow = Some(true);
        Ok(())
    }

    fn unfollow(&mut self) -> Result<(), SupplementError> {
        self.follow = Some(false);
        Ok(())
    }

    fn clear_follow(&mut self) -> Result<(), SupplementError> {
        self.follow = None;
        Ok(())
    }

    fn archive_at_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        self.archive_versions.insert(wavelet_id.clone(), version);
        Ok(())
    }

    fn clear_archive_state(&mut self) -> Result<(), SupplementError> {
        self.archive_versions.clear();
        Ok(())
    }

    fn set_seen_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: &HashedVersion,
    ) -> Result<(), SupplementError> {
        self.seen_versions
            .insert(wavelet_id.clone(), version.clone());
        Ok(())
    }

    fn clear_seen_version(&mut self, wavelet_id: &WaveletId) -> Result<(), SupplementError> {
        self.seen_versions.remove(wavelet_id);
        Ok(())
    }

    fn add_wanted_evaluation(
        &mut self,
        evaluation: &WantedEvaluation,
    ) -> Result<(), SupplementError> {
        if !self.wanted_evaluations.contains(evaluation) {
            self.wanted_evaluations.push(evaluation.clone());
        }
        Ok(())
    }

    fn set_notified_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        self.notified_versions.insert(wavelet_id.clone(), version);
        Ok(())
    }

    fn clear_pending_notification(&mut self) -> Result<(), SupplementError> {
        self.pending_notification = false;
        Ok(())
    }

    fn set_gadget_state(
        &mut self,
        gadget_id: &str,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), SupplementError> {
        let state = self.gadget_states.entry(gadget_id.to_owned()).or_default();
        match value {
            Some(value) => state.insert(key.to_owned(), value.to_owned()),
            None => state.remove(key),
        };
        Ok(())
    }
}
