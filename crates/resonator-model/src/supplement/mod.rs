//! The per-user state of a wave: what the user has read, where the wave is
//! filed, whether it is archived or muted, and so on (the "supplement").
//!
//! The state lives in the user's user-data wavelet of the wave, one small
//! document per concern (see [`WaveletBasedSupplement`] for the document
//! shapes). The module has the same layers as the original:
//!
//! * [`ReadablePrimitiveSupplement`] and [`PrimitiveSupplement`] are the raw
//!   state (`PrimitiveSupplement`): versions and flags, without
//!   interpretation. [`WaveletBasedSupplement`] keeps it in a user-data
//!   wavelet, emitting an operation for every change;
//!   [`UserDataSupplement`] only reads one; [`PrimitiveSupplementImpl`]
//!   keeps it in memory.
//! * [`SupplementImpl`] interprets the raw state (`Supplement`): a blip is
//!   unread if it changed after the version at which it, or its whole
//!   wavelet, was last read.
//! * [`SupplementedWaveImpl`] combines the state with what it needs to know
//!   about the wave itself, given by a [`SupplementWaveView`]
//!   (`SupplementedWave`): is the wave in the inbox, mark it all as read,
//!   archive it.
//!
//! Derived from `org.waveprotocol.wave.model.supplement` of Apache Wave
//! (`PrimitiveSupplement`, `PrimitiveSupplementImpl`,
//! `WaveletBasedSupplement`, `WaveletReadStateCollection`,
//! `DocumentBasedWaveletReadState`, `WaveletThreadStateCollection`,
//! `DocumentBasedWaveletThreadState`, `GadgetStateCollection`,
//! `DocumentBasedGadgetState`, `DocumentBasedAbuseStore`, `SupplementImpl`,
//! `SupplementedWaveImpl`, `SupplementWaveView`,
//! `PartitioningPrimitiveSupplement`, `ThreadState`, `WantedEvaluation`,
//! `SimpleWantedEvaluation`, `SimpleWantedEvaluationSet`) and
//! `conversation.InboxState`, licensed under the Apache License, Version
//! 2.0.
//!
//! Not ported: listeners and events (`ObservablePrimitiveSupplement`,
//! `ObservableSupplementedWave`, `ObservableAbuseStore`,
//! `LiveSupplementedWaveImpl`, which only adds events to
//! `SupplementedWaveImpl`), the pure delegation wrapper
//! `SupplementedWaveWrapper`, the client presentation policy
//! (`PresentationPolicy`, `SimplePresentationPolicy`), and
//! `ThreadStateSerializer` with `PrimitiveThreadState`, which nothing uses.
//!
//! Versions are `i32` with [`NO_VERSION`] for "none", as in the original.
//!
//! # Use
//!
//! To read the state of a user for a digest, without writing anything:
//!
//! ```
//! # use resonator_model::data::WaveletData;
//! # use resonator_model::id::ParticipantId;
//! use resonator_model::supplement::{
//!     DefaultFollow, PrimitiveSupplementImpl, SupplementedWaveImpl, UserDataSupplement,
//!     WaveletDataView,
//! };
//!
//! /// Counts the unread blips among `blips` of the root conversation.
//! fn unread_count(
//!     viewer: &ParticipantId,
//!     wavelets: &[WaveletData],
//!     user_data: Option<&WaveletData>,
//!     root: &WaveletData,
//!     blips: &[&str],
//! ) -> usize {
//!     // A snapshot of the user's state; empty if they have no user-data
//!     // wavelet yet.
//!     let state = match user_data {
//!         Some(udw) => PrimitiveSupplementImpl::copy_of(&UserDataSupplement::new(udw)),
//!         None => PrimitiveSupplementImpl::new(),
//!     };
//!     let view = WaveletDataView::new(viewer.clone(), wavelets);
//!     let supplement = SupplementedWaveImpl::new(state, view, DefaultFollow::Always);
//!     let unread = |blip: &&&str| supplement.is_unread(root.wavelet_id(), blip);
//!     blips.iter().filter(unread).count()
//! }
//! ```
//!
//! To change it, wrap the user-data wavelet in an
//! [`OpBasedWavelet`](crate::wavelet::OpBasedWavelet), which records the
//! operations to submit:
//!
//! ```
//! # use resonator_model::data::WaveletData;
//! # use resonator_model::id::ParticipantId;
//! # use resonator_model::operation::WaveletDelta;
//! use resonator_model::supplement::{
//!     DefaultFollow, SupplementError, SupplementedWaveImpl, WaveletBasedSupplement,
//!     WaveletDataView,
//! };
//! use resonator_model::wavelet::OpBasedWavelet;
//!
//! /// Marks a wave as read and archives it; returns the delta to submit to
//! /// the user-data wavelet, if anything changed.
//! fn read_and_archive(
//!     viewer: &ParticipantId,
//!     wavelets: &[WaveletData],
//!     user_data: WaveletData,
//!     now: i64,
//! ) -> Result<Option<WaveletDelta>, SupplementError> {
//!     let mut udw = OpBasedWavelet::new(user_data, viewer.clone(), now);
//!     let view = WaveletDataView::new(viewer.clone(), wavelets);
//!     let state = WaveletBasedSupplement::create(&mut udw)?;
//!     let mut supplement = SupplementedWaveImpl::new(state, view, DefaultFollow::Always);
//!     supplement.mark_as_read()?;
//!     supplement.archive()?;
//!     Ok(udw.take_delta())
//! }
//! ```

mod partitioning;
mod primitive;
mod supplement_impl;
mod supplemented_wave;
mod wanted;
pub(crate) mod wavelet_based;

#[cfg(test)]
mod golden_tests;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use thiserror::Error;

pub use partitioning::PartitioningPrimitiveSupplement;
pub use primitive::PrimitiveSupplementImpl;
pub use supplement_impl::SupplementImpl;
pub use supplemented_wave::{
    ALL_FOLDER, DefaultFollow, INBOX_FOLDER, InboxState, SupplementWaveView, SupplementedWaveImpl,
    TRASH_FOLDER, WaveletDataView,
};
pub use wanted::{WantedEvaluation, WantedEvaluationSet};
pub use wavelet_based::{
    ABUSE_DOCUMENT, ARCHIVING_DOCUMENT, CLEARED_DOCUMENT, FOLDERS_DOCUMENT, GADGETS_DOCUMENT,
    MUTED_DOCUMENT, PRESENTATION_DOCUMENT, READSTATE_DOCUMENT, SEEN_DOCUMENT, UserDataSupplement,
    WaveletBasedSupplement,
};

use crate::adt::{AdtError, Serializable};
use crate::doc::DocError;
use crate::id::WaveletId;
use crate::version::HashedVersion;
use crate::wavelet::WaveletError;

/// The version reported where there is none (`PrimitiveSupplement.NO_VERSION`).
pub const NO_VERSION: i32 = -1;

/// Failure to change supplement state.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SupplementError {
    #[error(transparent)]
    Adt(#[from] AdtError),
    #[error(transparent)]
    Document(#[from] DocError),
    #[error(transparent)]
    Wavelet(#[from] WaveletError),
    /// A state in which the original fails with a `NullPointerException`.
    #[error("{0}")]
    IllegalState(&'static str),
}

/// Whether the user has collapsed or expanded a thread (`ThreadState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ThreadState {
    /// The thread has been explicitly collapsed by the user.
    Collapsed,
    /// The thread has been explicitly expanded by the user.
    Expanded,
}

impl ThreadState {
    /// The name of the Java enum constant, which is the stored form.
    pub fn name(self) -> &'static str {
        match self {
            ThreadState::Collapsed => "COLLAPSED",
            ThreadState::Expanded => "EXPANDED",
        }
    }
}

/// `Serializer.EnumSerializer<ThreadState>`.
impl Serializable for ThreadState {
    fn to_attribute(&self) -> Result<String, AdtError> {
        Ok(self.name().to_owned())
    }

    fn from_attribute(attribute: &str) -> Option<Self> {
        match attribute {
            "COLLAPSED" => Some(ThreadState::Collapsed),
            "EXPANDED" => Some(ThreadState::Expanded),
            _ => None,
        }
    }
}

/// The raw supplement state, for reading (the query half of
/// `PrimitiveSupplement`). Listings are sorted.
pub trait ReadablePrimitiveSupplement {
    /// The state of a thread, if the user gave it one (`getThreadState`).
    fn thread_state(&self, wavelet_id: &WaveletId, thread_id: &str) -> Option<ThreadState>;

    /// `getWaveletsWithThreadState`.
    fn wavelets_with_thread_state(&self) -> Vec<WaveletId>;

    /// The threads of a wavelet that have a state (`getStatefulThreads`).
    fn stateful_threads(&self, wavelet_id: &WaveletId) -> Vec<String>;

    /// The version at which the whole wavelet was last marked as read
    /// (`getLastReadWaveletVersion`).
    fn last_read_wavelet_version(&self, wavelet_id: &WaveletId) -> i32;

    /// `getLastReadBlipVersion`.
    fn last_read_blip_version(&self, wavelet_id: &WaveletId, blip_id: &str) -> i32;

    /// `getLastReadParticipantsVersion`.
    fn last_read_participants_version(&self, wavelet_id: &WaveletId) -> i32;

    /// `getLastReadTagsVersion`.
    fn last_read_tags_version(&self, wavelet_id: &WaveletId) -> i32;

    /// The wavelets that have any read state (`getReadWavelets`).
    fn read_wavelets(&self) -> Vec<WaveletId>;

    /// The blips of a wavelet that have a read version (`getReadBlips`).
    fn read_blips(&self, wavelet_id: &WaveletId) -> Vec<String>;

    /// `getFolders`.
    fn folders(&self) -> Vec<i32>;

    /// `isInFolder`.
    fn is_in_folder(&self, id: i32) -> bool {
        self.folders().contains(&id)
    }

    /// The version up to which a wavelet is archived
    /// (`getArchiveWaveletVersion`).
    fn archive_wavelet_version(&self, wavelet_id: &WaveletId) -> i32;

    /// `getArchiveWavelets`.
    fn archive_wavelets(&self) -> Vec<WaveletId>;

    /// Whether the user explicitly follows (`Some(true)`) or unfollows
    /// (`Some(false)`) the wave (`getFollowed`).
    fn followed(&self) -> Option<bool>;

    /// The version of a wavelet the user last saw; version zero without
    /// hash if none (`getSeenVersion`).
    fn seen_version(&self, wavelet_id: &WaveletId) -> HashedVersion;

    /// `getSeenWavelets`.
    fn seen_wavelets(&self) -> Vec<WaveletId>;

    /// All evaluations of whether the wave is wanted, without duplicates
    /// (`getWantedEvaluations`).
    fn wanted_evaluations(&self) -> Vec<WantedEvaluation>;

    /// `getPendingNotification`.
    fn pending_notification(&self) -> bool;

    /// `getNotifiedWavelets`.
    fn notified_wavelets(&self) -> Vec<WaveletId>;

    /// `getNotifiedVersion`.
    fn notified_version(&self, wavelet_id: &WaveletId) -> i32;

    /// The private state of a gadget, by key (`getGadgetState`).
    fn gadget_state(&self, gadget_id: &str) -> BTreeMap<String, String>;
}

/// The raw supplement state, for writing (the mutating half of
/// `PrimitiveSupplement`).
pub trait PrimitiveSupplement: ReadablePrimitiveSupplement {
    /// Sets the state of a thread, or removes it for `None`
    /// (`setThreadState`).
    fn set_thread_state(
        &mut self,
        wavelet_id: &WaveletId,
        thread_id: &str,
        state: Option<ThreadState>,
    ) -> Result<(), SupplementError>;

    /// `setLastReadWaveletVersion`.
    fn set_last_read_wavelet_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError>;

    /// `setLastReadBlipVersion`.
    fn set_last_read_blip_version(
        &mut self,
        wavelet_id: &WaveletId,
        blip_id: &str,
        version: i32,
    ) -> Result<(), SupplementError>;

    /// `setLastReadParticipantsVersion`.
    fn set_last_read_participants_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError>;

    /// `setLastReadTagsVersion`.
    fn set_last_read_tags_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError>;

    /// Forgets all read state (`clearReadState`).
    fn clear_read_state(&mut self) -> Result<(), SupplementError>;

    /// `clearBlipReadState`.
    fn clear_blip_read_state(
        &mut self,
        wavelet_id: &WaveletId,
        blip_id: &str,
    ) -> Result<(), SupplementError>;

    /// `addFolder`.
    fn add_folder(&mut self, id: i32) -> Result<(), SupplementError>;

    /// `removeFolder`.
    fn remove_folder(&mut self, id: i32) -> Result<(), SupplementError>;

    /// `removeAllFolders`.
    fn remove_all_folders(&mut self) -> Result<(), SupplementError>;

    /// `follow`.
    fn follow(&mut self) -> Result<(), SupplementError>;

    /// `unfollow`.
    fn unfollow(&mut self) -> Result<(), SupplementError>;

    /// Returns to neither following nor unfollowing (`clearFollow`).
    fn clear_follow(&mut self) -> Result<(), SupplementError>;

    /// `archiveAtVersion`.
    fn archive_at_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError>;

    /// `clearArchiveState`.
    fn clear_archive_state(&mut self) -> Result<(), SupplementError>;

    /// `setSeenVersion`.
    fn set_seen_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: &HashedVersion,
    ) -> Result<(), SupplementError>;

    /// `clearSeenVersion`.
    fn clear_seen_version(&mut self, wavelet_id: &WaveletId) -> Result<(), SupplementError>;

    /// `addWantedEvaluation`.
    fn add_wanted_evaluation(
        &mut self,
        evaluation: &WantedEvaluation,
    ) -> Result<(), SupplementError>;

    /// `setNotifiedVersion`.
    fn set_notified_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError>;

    /// `clearPendingNotification`.
    fn clear_pending_notification(&mut self) -> Result<(), SupplementError>;

    /// Sets one key of the private state of a gadget, or removes it for
    /// `None` (`setGadgetState`).
    fn set_gadget_state(
        &mut self,
        gadget_id: &str,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), SupplementError>;
}

macro_rules! forward_reads {
    ($($ty:ty),*) => {$(
        impl<P: ReadablePrimitiveSupplement + ?Sized> ReadablePrimitiveSupplement for $ty {
            fn thread_state(&self, wavelet_id: &WaveletId, thread_id: &str) -> Option<ThreadState> {
                (**self).thread_state(wavelet_id, thread_id)
            }

            fn wavelets_with_thread_state(&self) -> Vec<WaveletId> {
                (**self).wavelets_with_thread_state()
            }

            fn stateful_threads(&self, wavelet_id: &WaveletId) -> Vec<String> {
                (**self).stateful_threads(wavelet_id)
            }

            fn last_read_wavelet_version(&self, wavelet_id: &WaveletId) -> i32 {
                (**self).last_read_wavelet_version(wavelet_id)
            }

            fn last_read_blip_version(&self, wavelet_id: &WaveletId, blip_id: &str) -> i32 {
                (**self).last_read_blip_version(wavelet_id, blip_id)
            }

            fn last_read_participants_version(&self, wavelet_id: &WaveletId) -> i32 {
                (**self).last_read_participants_version(wavelet_id)
            }

            fn last_read_tags_version(&self, wavelet_id: &WaveletId) -> i32 {
                (**self).last_read_tags_version(wavelet_id)
            }

            fn read_wavelets(&self) -> Vec<WaveletId> {
                (**self).read_wavelets()
            }

            fn read_blips(&self, wavelet_id: &WaveletId) -> Vec<String> {
                (**self).read_blips(wavelet_id)
            }

            fn folders(&self) -> Vec<i32> {
                (**self).folders()
            }

            fn is_in_folder(&self, id: i32) -> bool {
                (**self).is_in_folder(id)
            }

            fn archive_wavelet_version(&self, wavelet_id: &WaveletId) -> i32 {
                (**self).archive_wavelet_version(wavelet_id)
            }

            fn archive_wavelets(&self) -> Vec<WaveletId> {
                (**self).archive_wavelets()
            }

            fn followed(&self) -> Option<bool> {
                (**self).followed()
            }

            fn seen_version(&self, wavelet_id: &WaveletId) -> HashedVersion {
                (**self).seen_version(wavelet_id)
            }

            fn seen_wavelets(&self) -> Vec<WaveletId> {
                (**self).seen_wavelets()
            }

            fn wanted_evaluations(&self) -> Vec<WantedEvaluation> {
                (**self).wanted_evaluations()
            }

            fn pending_notification(&self) -> bool {
                (**self).pending_notification()
            }

            fn notified_wavelets(&self) -> Vec<WaveletId> {
                (**self).notified_wavelets()
            }

            fn notified_version(&self, wavelet_id: &WaveletId) -> i32 {
                (**self).notified_version(wavelet_id)
            }

            fn gadget_state(&self, gadget_id: &str) -> BTreeMap<String, String> {
                (**self).gadget_state(gadget_id)
            }
        }
    )*};
}

forward_reads!(&P, &mut P);

impl<P: PrimitiveSupplement + ?Sized> PrimitiveSupplement for &mut P {
    fn set_thread_state(
        &mut self,
        wavelet_id: &WaveletId,
        thread_id: &str,
        state: Option<ThreadState>,
    ) -> Result<(), SupplementError> {
        (**self).set_thread_state(wavelet_id, thread_id, state)
    }

    fn set_last_read_wavelet_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        (**self).set_last_read_wavelet_version(wavelet_id, version)
    }

    fn set_last_read_blip_version(
        &mut self,
        wavelet_id: &WaveletId,
        blip_id: &str,
        version: i32,
    ) -> Result<(), SupplementError> {
        (**self).set_last_read_blip_version(wavelet_id, blip_id, version)
    }

    fn set_last_read_participants_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        (**self).set_last_read_participants_version(wavelet_id, version)
    }

    fn set_last_read_tags_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        (**self).set_last_read_tags_version(wavelet_id, version)
    }

    fn clear_read_state(&mut self) -> Result<(), SupplementError> {
        (**self).clear_read_state()
    }

    fn clear_blip_read_state(
        &mut self,
        wavelet_id: &WaveletId,
        blip_id: &str,
    ) -> Result<(), SupplementError> {
        (**self).clear_blip_read_state(wavelet_id, blip_id)
    }

    fn add_folder(&mut self, id: i32) -> Result<(), SupplementError> {
        (**self).add_folder(id)
    }

    fn remove_folder(&mut self, id: i32) -> Result<(), SupplementError> {
        (**self).remove_folder(id)
    }

    fn remove_all_folders(&mut self) -> Result<(), SupplementError> {
        (**self).remove_all_folders()
    }

    fn follow(&mut self) -> Result<(), SupplementError> {
        (**self).follow()
    }

    fn unfollow(&mut self) -> Result<(), SupplementError> {
        (**self).unfollow()
    }

    fn clear_follow(&mut self) -> Result<(), SupplementError> {
        (**self).clear_follow()
    }

    fn archive_at_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        (**self).archive_at_version(wavelet_id, version)
    }

    fn clear_archive_state(&mut self) -> Result<(), SupplementError> {
        (**self).clear_archive_state()
    }

    fn set_seen_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: &HashedVersion,
    ) -> Result<(), SupplementError> {
        (**self).set_seen_version(wavelet_id, version)
    }

    fn clear_seen_version(&mut self, wavelet_id: &WaveletId) -> Result<(), SupplementError> {
        (**self).clear_seen_version(wavelet_id)
    }

    fn add_wanted_evaluation(
        &mut self,
        evaluation: &WantedEvaluation,
    ) -> Result<(), SupplementError> {
        (**self).add_wanted_evaluation(evaluation)
    }

    fn set_notified_version(
        &mut self,
        wavelet_id: &WaveletId,
        version: i32,
    ) -> Result<(), SupplementError> {
        (**self).set_notified_version(wavelet_id, version)
    }

    fn clear_pending_notification(&mut self) -> Result<(), SupplementError> {
        (**self).clear_pending_notification()
    }

    fn set_gadget_state(
        &mut self,
        gadget_id: &str,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), SupplementError> {
        (**self).set_gadget_state(gadget_id, key, value)
    }
}
