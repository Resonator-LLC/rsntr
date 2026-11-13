//! A wavelet before and after a run of deltas (`WaveletAndDeltas`,
//! `WaveletOperationInverter`).

use resonator_model::data::WaveletData;
use resonator_model::docop;
use resonator_model::id::WaveletName;
use resonator_model::operation::{
    BlipOperation, TransformedWaveletDelta, UpdateContributorMethod, WaveletOperation,
    WaveletOperationContext, WaveletOperationKind,
};
use resonator_model::version::HashedVersion;
use thiserror::Error;

/// Failure to prepare or generate events.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EventError {
    /// Bad input: a snapshot that does not match its deltas, deltas that
    /// do not follow each other.
    #[error("{0}")]
    InvalidArgument(String),
    /// An operation, or its inverse, does not apply to the wavelet.
    #[error("{0}")]
    Operation(String),
}

/// The operation that undoes an operation (`WaveletOperationInverter`).
///
/// As in the original the inverse keeps the timestamp and hashed version
/// of the operation and only negates the version increment, so a wavelet
/// rolled back with it has the right content and version number but not
/// the last-modified times, blip versions and contributors it had before.
pub fn invert_operation(op: &WaveletOperation) -> Result<WaveletOperation, EventError> {
    let forward = op.context();
    let reverse = match forward.hashed_version() {
        Some(version) => WaveletOperationContext::with_hashed_version(
            forward.creator().clone(),
            forward.timestamp(),
            -forward.version_increment(),
            version.clone(),
        ),
        None => WaveletOperationContext::new(
            forward.creator().clone(),
            forward.timestamp(),
            -forward.version_increment(),
        ),
    };
    let kind = match op.kind() {
        WaveletOperationKind::NoOp => WaveletOperationKind::NoOp,
        WaveletOperationKind::AddParticipant { participant, .. } => {
            WaveletOperationKind::RemoveParticipant {
                participant: participant.clone(),
            }
        }
        WaveletOperationKind::RemoveParticipant { participant } => {
            WaveletOperationKind::AddParticipant {
                participant: participant.clone(),
                position: None,
            }
        }
        WaveletOperationKind::Blip { blip_id, op } => WaveletOperationKind::Blip {
            blip_id: blip_id.clone(),
            op: match op {
                BlipOperation::Content { op, .. } => BlipOperation::Content {
                    op: docop::invert(op).map_err(|e| EventError::Operation(e.to_string()))?,
                    method: UpdateContributorMethod::Add,
                },
                BlipOperation::Submit => BlipOperation::Submit,
            },
        },
        WaveletOperationKind::VersionUpdate { .. } => {
            return Err(EventError::Operation(
                "a version update cannot be inverted".to_owned(),
            ));
        }
    };
    Ok(WaveletOperation::new(reverse, kind))
}

/// Undoes deltas on a wavelet that is in the state after them, last
/// operation first (`WaveletAndDeltas.rollback`).
pub fn rollback(
    wavelet: &mut WaveletData,
    deltas: &[TransformedWaveletDelta],
) -> Result<(), EventError> {
    let start_version = wavelet.version();
    let mut op_count: u64 = 0;
    for delta in deltas.iter().rev() {
        for op in delta.ops().iter().rev() {
            invert_operation(op)?
                .apply(wavelet)
                .map_err(|e| EventError::Operation(e.to_string()))?;
            op_count += 1;
        }
    }
    if start_version.checked_sub(op_count) != Some(wavelet.version()) {
        return Err(EventError::Operation(format!(
            "Expected end version {} doesn't match the version of the wavelet {}",
            start_version as i128 - op_count as i128,
            wavelet.version()
        )));
    }
    Ok(())
}

/// A wavelet as it was before a run of consecutive deltas, the deltas,
/// and the wavelet after them (`WaveletAndDeltas`). This is what events
/// for a robot are generated from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaveletAndDeltas {
    snapshot_before_deltas: WaveletData,
    deltas: Vec<TransformedWaveletDelta>,
    snapshot_after_deltas: WaveletData,
    wavelet_name: WaveletName,
}

fn start_version(deltas: &[TransformedWaveletDelta]) -> Option<u64> {
    deltas
        .first()
        .map(TransformedWaveletDelta::applied_at_version)
}

impl WaveletAndDeltas {
    /// From the wavelet after the deltas and the deltas; the state before
    /// them is found by undoing them (`create`).
    pub fn create(
        snapshot: &WaveletData,
        deltas: Vec<TransformedWaveletDelta>,
    ) -> Result<Self, EventError> {
        let end_version = deltas
            .last()
            .map_or(snapshot.version(), |d| d.resulting_version().version());
        if snapshot.version() != end_version {
            return Err(EventError::InvalidArgument(format!(
                "Version of snapshot {} doesn't match the end version {}",
                snapshot.version(),
                end_version
            )));
        }
        let mut before = snapshot.clone();
        rollback(&mut before, &deltas)?;
        Ok(WaveletAndDeltas {
            wavelet_name: snapshot.wavelet_name(),
            snapshot_before_deltas: before,
            deltas,
            snapshot_after_deltas: snapshot.clone(),
        })
    }

    pub fn wavelet_name(&self) -> &WaveletName {
        &self.wavelet_name
    }

    pub fn snapshot_before_deltas(&self) -> &WaveletData {
        &self.snapshot_before_deltas
    }

    pub fn deltas(&self) -> &[TransformedWaveletDelta] {
        &self.deltas
    }

    pub fn snapshot_after_deltas(&self) -> &WaveletData {
        &self.snapshot_after_deltas
    }

    /// The hashed version after the last delta (`getVersionAfterDeltas`).
    pub fn version_after_deltas(&self) -> HashedVersion {
        match self.deltas.last() {
            Some(delta) => delta.resulting_version().clone(),
            None => self.snapshot_after_deltas.hashed_version().clone(),
        }
    }

    /// Whether deltas start at the version this ends at
    /// (`areContiguousToCurrentVersion`).
    pub fn are_contiguous_to_current_version(&self, deltas: &[TransformedWaveletDelta]) -> bool {
        start_version(deltas) == Some(self.version_after_deltas().version())
    }

    /// Adds deltas that follow the ones here, with the wavelet after them
    /// (`appendDeltas`).
    pub fn append_deltas(
        &mut self,
        updated_snapshot: &WaveletData,
        new_deltas: Vec<TransformedWaveletDelta>,
    ) -> Result<(), EventError> {
        let Some(last) = new_deltas.last() else {
            return Err(EventError::InvalidArgument(
                "There were no new deltas passed to appendDeltas".to_owned(),
            ));
        };
        if updated_snapshot.version() != last.resulting_version().version() {
            return Err(EventError::InvalidArgument(format!(
                "Version of snapshot {} doesn't match the HashedVersion {}",
                updated_snapshot.version(),
                last.resulting_version()
            )));
        }
        if !self.are_contiguous_to_current_version(&new_deltas) {
            return Err(EventError::InvalidArgument(format!(
                "Deltas are not contiguous to the current version({})",
                self.version_after_deltas()
            )));
        }
        let updated_name = updated_snapshot.wavelet_name();
        if updated_name != self.wavelet_name {
            return Err(EventError::InvalidArgument(format!(
                "Updated wavelet doesn't have the same name as with which this class has been \
                 instantiated. {} != {}",
                updated_name, self.wavelet_name
            )));
        }
        self.snapshot_after_deltas = updated_snapshot.clone();
        self.deltas.extend(new_deltas);
        Ok(())
    }
}
