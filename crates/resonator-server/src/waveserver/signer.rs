//! The signing seam: what signs the deltas this server applies on behalf
//! of its users (`SignatureHandler` and `CertificateManager.signDelta` in
//! the original).
//!
//! By default no signer is configured and deltas are wrapped without
//! signatures (as `NonSigningSignatureHandler` does), so the applied-delta
//! bytes, and with them the history hashes, are exactly those of a server
//! without federation. A federating server injects its signer through
//! [`super::WaveServerOptions::delta_signer`].

use std::sync::Arc;

use resonator_model::proto::ByteStringMessage;
use resonator_model::proto::federation::{ProtocolSignature, ProtocolWaveletDelta};

/// Signs serialised deltas.
pub trait DeltaSigner: Send + Sync {
    /// The signatures to attach to `delta`, one per domain of its extended
    /// address path (only the author's, as long as address paths are not
    /// used). Signs the exact bytes of `delta`.
    fn sign(&self, delta: &ByteStringMessage<ProtocolWaveletDelta>) -> Vec<ProtocolSignature>;
}

/// A shared [`DeltaSigner`]. Two handles are equal when they share the
/// signer.
#[derive(Clone)]
pub struct SharedDeltaSigner(pub Arc<dyn DeltaSigner>);

impl SharedDeltaSigner {
    pub fn new(signer: impl DeltaSigner + 'static) -> Self {
        SharedDeltaSigner(Arc::new(signer))
    }
}

impl std::fmt::Debug for SharedDeltaSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedDeltaSigner(..)")
    }
}

impl PartialEq for SharedDeltaSigner {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for SharedDeltaSigner {}
