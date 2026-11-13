//! Keeping presentation state out of the user-data wavelet
//! (`PartitioningPrimitiveSupplement`).

use std::collections::BTreeMap;

use super::{
    PrimitiveSupplement, PrimitiveSupplementImpl, ReadablePrimitiveSupplement, SupplementError,
    ThreadState, WantedEvaluation,
};
use crate::id::WaveletId;
use crate::version::HashedVersion;

/// A supplement that keeps thread states in memory rather than in the real
/// supplement, unless told to use the presentation model, and that reads
/// and writes private gadget states only if told to
/// (`PartitioningPrimitiveSupplement`). Everything else goes to the real
/// supplement.
#[derive(Debug, Clone)]
pub struct PartitioningPrimitiveSupplement<P> {
    real: P,
    /// Thread states when they are not kept in the real supplement.
    presentation: Option<PrimitiveSupplementImpl>,
    use_private_gadget_states: bool,
}

impl<P> PartitioningPrimitiveSupplement<P> {
    pub fn new(real: P, use_presentation_model: bool, use_private_gadget_states: bool) -> Self {
        PartitioningPrimitiveSupplement {
            real,
            presentation: (!use_presentation_model).then(PrimitiveSupplementImpl::new),
            use_private_gadget_states,
        }
    }

    pub fn real(&self) -> &P {
        &self.real
    }

    pub fn into_real(self) -> P {
        self.real
    }

    /// Forgets the thread states kept in memory (`stopPartitioning`).
    pub fn stop_partitioning(&mut self) {
        if self.presentation.is_some() {
            self.presentation = Some(PrimitiveSupplementImpl::new());
        }
    }
}

impl<P: ReadablePrimitiveSupplement> ReadablePrimitiveSupplement
    for PartitioningPrimitiveSupplement<P>
{
    fn thread_state(&self, wavelet_id: &WaveletId, thread_id: &str) -> Option<ThreadState> {
        match &self.presentation {
            Some(presentation) => presentation.thread_state(wavelet_id, thread_id),
            None => self.real.thread_state(wavelet_id, thread_id),
        }
    }

    fn wavelets_with_thread_state(&self) -> Vec<WaveletId> {
        match &self.presentation {
            Some(presentation) => presentation.wavelets_with_thread_state(),
            None => self.real.wavelets_with_thread_state(),
        }
    }

    fn stateful_threads(&self, wavelet_id: &WaveletId) -> Vec<String> {
        match &self.presentation {
            Some(presentation) => presentation.stateful_threads(wavelet_id),
            None => self.real.stateful_threads(wavelet_id),
        }
    }

    fn last_read_wavelet_version(&self, wavelet_id: &WaveletId) -> i32 {
        self.real.last_read_wavelet_version(wavelet_id)
    }

    fn last_read_blip_version(&self, wavelet_id: &WaveletId, blip_id: &str) -> i32 {
        self.real.last_read_blip_version(wavelet_id, blip_id)
    }

    fn last_read_participants_version(&self, wavelet_id: &WaveletId) -> i32 {
        self.real.last_read_participants_version(wavelet_id)
    }

    fn last_read_tags_version(&self, wavelet_id: &WaveletId) -> i32 {
        self.real.last_read_tags_version(wavelet_id)
    }

    fn read_wavelets(&self) -> Vec<WaveletId> {
        self.real.read_wavelets()
    }

    fn read_blips(&self, wavelet_id: &WaveletId) -> Vec<String> {
        self.real.read_blips(wavelet_id)
    }

    fn folders(&self) -> Vec<i32> {
        self.real.folders()
    }

    fn is_in_folder(&self, id: i32) -> bool {
        self.real.is_in_folder(id)
    }

    fn archive_wavelet_version(&self, wavelet_id: &WaveletId) -> i32 {
        self.real.archive_wavelet_version(wavelet_id)
    }

    fn archive_wavelets(&self) -> Vec<WaveletId> {
        self.real.archive_wavelets()
    }

    fn followed(&self) -> Option<bool> {
        self.real.followed()
    }

    fn seen_version(&self, wavelet_id: &WaveletId) -> HashedVersion {
        self.real.seen_version(wavelet_id)
    }

    fn seen_wavelets(&self) -> Vec<WaveletId> {
        self.real.seen_wavelets()
    }

    fn wanted_evaluations(&self) -> Vec<WantedEvaluation> {
        self.real.wanted_evaluations()
    }

    fn pending_notification(&self) -> bool {
        self.real.pending_notification()
    }

    fn notified_wavelets(&self) -> Vec<WaveletId> {
        self.real.notified_wavelets()
    }

    fn notified_version(&self, wavelet_id: &WaveletId) -> i32 {
        self.real.notified_version(wavelet_id)
    }

    fn gadget_state(&self, gadget_id: &str) -> BTreeMap<String, String> {
        if self.use_private_gadget_states {
            self.real.gadget_state(gadget_id)
        } else {
            BTreeMap::new()
        }
    }
}

impl<P: PrimitiveSupplement> PrimitiveSupplement for PartitioningPrimitiveSupplement<P> {
    fn set_thread_state(
        &mut self,
        wavelet_id: &WaveletId,
        thread_id: &str,
        state: Option<ThreadState>,
    ) -> Result<(), SupplementError> {
        match &mut self.presentation {
            Some(presentation) => presentation.set_thread_state(wavelet_id, thread_id, state),
            None => self.real.set_thread_state(wavelet_id, thread_id, state),
        }
    }

    fn set_last_read_wavelet_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        self.real.set_last_read_wavelet_version(wavelet_id, version)
    }

    fn set_last_read_blip_version(
        &mut self,
        wavelet_id: &WaveletId,
        blip_id: &str,
        version: i32,
    ) -> Result<(), SupplementError> {
        self.real
            .set_last_read_blip_version(wavelet_id, blip_id, version)
    }

    fn set_last_read_participants_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        self.real
            .set_last_read_participants_version(wavelet_id, version)
    }

    fn set_last_read_tags_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        self.real.set_last_read_tags_version(wavelet_id, version)
    }

    fn clear_read_state(&mut self) -> Result<(), SupplementError> {
        self.real.clear_read_state()
    }

    fn clear_blip_read_state(
        &mut self,
        wavelet_id: &WaveletId,
        blip_id: &str,
    ) -> Result<(), SupplementError> {
        self.real.clear_blip_read_state(wavelet_id, blip_id)
    }

    fn add_folder(&mut self, id: i32) -> Result<(), SupplementError> {
        self.real.add_folder(id)
    }

    fn remove_folder(&mut self, id: i32) -> Result<(), SupplementError> {
        self.real.remove_folder(id)
    }

    fn remove_all_folders(&mut self) -> Result<(), SupplementError> {
        self.real.remove_all_folders()
    }

    fn follow(&mut self) -> Result<(), SupplementError> {
        self.real.follow()
    }

    fn unfollow(&mut self) -> Result<(), SupplementError> {
        self.real.unfollow()
    }

    fn clear_follow(&mut self) -> Result<(), SupplementError> {
        self.real.clear_follow()
    }

    fn archive_at_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        self.real.archive_at_version(wavelet_id, version)
    }

    fn clear_archive_state(&mut self) -> Result<(), SupplementError> {
        self.real.clear_archive_state()
    }

    fn set_seen_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: &HashedVersion,
    ) -> Result<(), SupplementError> {
        self.real.set_seen_version(wavelet_id, version)
    }

    fn clear_seen_version(&mut self, wavelet_id: &WaveletId) -> Result<(), SupplementError> {
        self.real.clear_seen_version(wavelet_id)
    }

    fn add_wanted_evaluation(
        &mut self,
        evaluation: &WantedEvaluation,
    ) -> Result<(), SupplementError> {
        self.real.add_wanted_evaluation(evaluation)
    }

    fn set_notified_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        self.real.set_notified_version(wavelet_id, version)
    }

    fn clear_pending_notification(&mut self) -> Result<(), SupplementError> {
        self.real.clear_pending_notification()
    }

    fn set_gadget_state(
        &mut self,
        gadget_id: &str,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), SupplementError> {
        if self.use_private_gadget_states {
            self.real.set_gadget_state(gadget_id, key, value)
        } else {
            Ok(())
        }
    }
}
