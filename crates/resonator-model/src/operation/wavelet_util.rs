//! Building wavelet state from deltas (`WaveletDataUtil`).

use super::delta::TransformedWaveletDelta;
use super::error::WaveletOperationError;
use crate::data::{BlipData, WaveletData};
use crate::docop::DocInitialization;
use crate::id::{ParticipantId, WaveletName};
use crate::version::HashedVersion;

/// Applies a delta to a wavelet at the delta's applied-at version. If an
/// operation fails, the operations before it are rolled back with their
/// reverse operations and the error is returned.
///
/// The rollback is only as exact as the reverse operations are; see
/// [`WaveletOperation::apply_and_return_reverse`](super::WaveletOperation::apply_and_return_reverse).
pub fn apply_wavelet_delta(
    delta: &TransformedWaveletDelta,
    wavelet: &mut WaveletData,
) -> Result<(), WaveletOperationError> {
    if delta.applied_at_version() != wavelet.version() {
        return Err(WaveletOperationError::DeltaVersionMismatch {
            delta_version: delta.applied_at_version(),
            wavelet_version: wavelet.version(),
        });
    }
    let mut reverse_ops = Vec::new();
    for (applied, op) in delta.iter().enumerate() {
        match op.apply_and_return_reverse(wavelet) {
            Ok(reverse) => reverse_ops.extend(reverse),
            Err(cause) => {
                // Deltas are atomic.
                for inverse in reverse_ops.iter().rev() {
                    if let Err(cause) = inverse.apply(wavelet) {
                        return Err(WaveletOperationError::RollbackFailed {
                            inverse: inverse.to_string(),
                            cause: Box::new(cause),
                        });
                    }
                }
                return Err(WaveletOperationError::DeltaFailed {
                    applied,
                    total: delta.len(),
                    version: wavelet.version(),
                    failed_op: op.to_string(),
                    cause: Box::new(cause),
                });
            }
        }
    }
    Ok(())
}

/// Creates a wavelet from its first delta, which must apply at version
/// zero. The delta's author becomes the creator and its timestamp the
/// creation time.
pub fn build_wavelet_from_first_delta(
    wavelet_name: &WaveletName,
    delta: &TransformedWaveletDelta,
) -> Result<WaveletData, WaveletOperationError> {
    if delta.applied_at_version() != 0 {
        return Err(WaveletOperationError::InvalidArgument(format!(
            "first delta has non-zero version: {}",
            delta.applied_at_version()
        )));
    }
    let mut wavelet = WaveletData::empty(
        wavelet_name,
        delta.author().clone(),
        // Overwritten by the delta.
        HashedVersion::unsigned(0),
        delta.application_timestamp(),
    );
    apply_wavelet_delta(delta, &mut wavelet)?;
    Ok(wavelet)
}

/// Creates a wavelet by applying a non-empty, contiguous sequence of deltas
/// starting at version zero.
pub fn build_wavelet_from_deltas<'a>(
    wavelet_name: &WaveletName,
    deltas: impl IntoIterator<Item = &'a TransformedWaveletDelta>,
) -> Result<WaveletData, WaveletOperationError> {
    let mut deltas = deltas.into_iter();
    let first = deltas
        .next()
        .ok_or_else(|| WaveletOperationError::InvalidArgument("empty deltas".to_owned()))?;
    let mut wavelet = build_wavelet_from_first_delta(wavelet_name, first)?;
    for delta in deltas {
        apply_wavelet_delta(delta, &mut wavelet)?;
    }
    Ok(wavelet)
}

/// Adds an empty document authored by `author`. As in the original, `time`
/// is used for both the last-modified time and the last-modified version.
pub fn add_empty_blip<'a>(
    wavelet: &'a mut WaveletData,
    blip_id: &str,
    author: ParticipantId,
    time: i64,
) -> Result<&'a mut BlipData, WaveletOperationError> {
    let version = u64::try_from(time)
        .map_err(|_| WaveletOperationError::InvalidArgument(format!("negative time {time}")))?;
    Ok(wavelet.create_document(
        blip_id,
        author.clone(),
        [author],
        &DocInitialization::default(),
        time,
        version,
    )?)
}
