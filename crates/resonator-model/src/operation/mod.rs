//! Wavelet operations, deltas and their transformation.
//!
//! Derived from `org.waveprotocol.wave.model.operation.wave`,
//! `org.waveprotocol.wave.concurrencycontrol.common.DeltaPair` and the pure
//! parts of `org.waveprotocol.box.server.waveserver.WaveletContainerImpl`
//! and `org.waveprotocol.box.server.util.WaveletDataUtil` of Apache Wave,
//! licensed under the Apache License, Version 2.0.

mod apply;
pub mod context;
pub mod delta;
pub mod error;
pub mod transform;
pub mod wave_op;
pub mod wavelet_util;
pub mod worthy;

#[cfg(test)]
mod tests;

pub use context::{NO_TIMESTAMP, WaveletOperationContext};
pub use delta::{TransformedWaveletDelta, WaveletDelta};
pub use error::{DeltaError, SubmitTransformError, WaveTransformError, WaveletOperationError};
pub use transform::{
    DeltaPair, OperationPair, transform, transform_blip_ops, transform_op_lists, transform_ops,
    transform_submitted_delta,
};
pub use wave_op::{BlipOperation, UpdateContributorMethod, WaveletOperation, WaveletOperationKind};
pub use wavelet_util::{
    add_empty_blip, apply_wavelet_delta, build_wavelet_from_deltas, build_wavelet_from_first_delta,
};
pub use worthy::{is_blip_id_worthy, is_worthy};

use crate::docop::{DocInitialization, DocOp, to_xml_string};

/// Syntactic equality of document operations
/// (`OpComparators.SYNTACTIC_IDENTITY`); the same as `==` on [`DocOp`].
pub fn syntactic_identity(a: &DocOp, b: &DocOp) -> bool {
    a == b
}

/// Whether two initializations describe the same document, comparing their
/// XML forms (`OpComparators.equalDocuments`).
pub fn equal_documents(a: &DocInitialization, b: &DocInitialization) -> bool {
    match (to_xml_string(a), to_xml_string(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
