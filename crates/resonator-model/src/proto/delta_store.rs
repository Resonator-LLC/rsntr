//! Messages of `box/server/persistence/protos/delta-store.proto` (package
//! `protodeltastore`).

use super::federation::{ProtocolHashedVersion, ProtocolWaveletOperation};
use super::wave_message;

/// A transformed delta as stored by the delta store.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProtoTransformedWaveletDelta {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub author: Option<String>,
    /// Required.
    #[prost(message, optional, tag = "2")]
    pub resulting_version: Option<ProtocolHashedVersion>,
    /// Required. Milliseconds since the epoch.
    #[prost(int64, optional, tag = "3")]
    pub application_timestamp: Option<i64>,
    #[prost(message, repeated, tag = "4")]
    pub operation: Vec<ProtocolWaveletOperation>,
}
wave_message!(ProtoTransformedWaveletDelta {
    required: [author, resulting_version, application_timestamp],
    nested: [resulting_version],
    repeated: [operation],
});
