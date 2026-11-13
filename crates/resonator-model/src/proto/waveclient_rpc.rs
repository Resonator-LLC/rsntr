//! Messages of `box/common/comms/waveclient-rpc.proto` (package
//! `waveserver`): the client-server protocol.

use super::federation::{ProtocolDocumentOperation, ProtocolHashedVersion, ProtocolWaveletDelta};
use super::wave_message;

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProtocolAuthenticate {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub token: Option<String>,
}
wave_message!(ProtocolAuthenticate { required: [token] });

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProtocolAuthenticationResult {}
wave_message!(ProtocolAuthenticationResult {});

/// A request to open a wave view.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProtocolOpenRequest {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub participant_id: Option<String>,
    /// Required.
    #[prost(string, optional, tag = "2")]
    pub wave_id: Option<String>,
    #[prost(string, repeated, tag = "3")]
    pub wavelet_id_prefix: Vec<String>,
    #[prost(message, repeated, tag = "4")]
    pub known_wavelet: Vec<WaveletVersion>,
}
wave_message!(ProtocolOpenRequest {
    required: [participant_id, wave_id],
    repeated: [known_wavelet],
});

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WaveletVersion {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub wavelet_id: Option<String>,
    /// Required.
    #[prost(message, optional, tag = "2")]
    pub hashed_version: Option<ProtocolHashedVersion>,
}
wave_message!(WaveletVersion {
    required: [wavelet_id, hashed_version],
    nested: [hashed_version],
});

/// The state of a document.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct DocumentSnapshot {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub document_id: Option<String>,
    /// Required. The document content as an initialization.
    #[prost(message, optional, tag = "2")]
    pub document_operation: Option<ProtocolDocumentOperation>,
    /// Required.
    #[prost(string, optional, tag = "3")]
    pub author: Option<String>,
    #[prost(string, repeated, tag = "4")]
    pub contributor: Vec<String>,
    /// Required.
    #[prost(int64, optional, tag = "5")]
    pub last_modified_version: Option<i64>,
    /// Required.
    #[prost(int64, optional, tag = "6")]
    pub last_modified_time: Option<i64>,
}
wave_message!(DocumentSnapshot {
    required: [
        document_id,
        document_operation,
        author,
        last_modified_version,
        last_modified_time
    ],
    nested: [document_operation],
});

/// The state of a wavelet.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WaveletSnapshot {
    /// Required. In the modern serialisation, `domain/id`.
    #[prost(string, optional, tag = "1")]
    pub wavelet_id: Option<String>,
    #[prost(string, repeated, tag = "2")]
    pub participant_id: Vec<String>,
    #[prost(message, repeated, tag = "3")]
    pub document: Vec<DocumentSnapshot>,
    /// Required.
    #[prost(message, optional, tag = "4")]
    pub version: Option<ProtocolHashedVersion>,
    /// Required.
    #[prost(int64, optional, tag = "5")]
    pub last_modified_time: Option<i64>,
    /// Required.
    #[prost(string, optional, tag = "6")]
    pub creator: Option<String>,
    /// Required.
    #[prost(int64, optional, tag = "7")]
    pub creation_time: Option<i64>,
}
wave_message!(WaveletSnapshot {
    required: [
        wavelet_id,
        version,
        last_modified_time,
        creator,
        creation_time
    ],
    nested: [version],
    repeated: [document],
});

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct WaveViewSnapshot {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub wave_id: Option<String>,
    #[prost(message, repeated, tag = "2")]
    pub wavelet: Vec<WaveletSnapshot>,
}
wave_message!(WaveViewSnapshot {
    required: [wave_id],
    repeated: [wavelet],
});

/// An update of a wave view: a channel id, a marker, a snapshot or deltas.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProtocolWaveletUpdate {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub wavelet_name: Option<String>,
    #[prost(message, repeated, tag = "2")]
    pub applied_delta: Vec<ProtocolWaveletDelta>,
    #[prost(message, optional, tag = "3")]
    pub commit_notice: Option<ProtocolHashedVersion>,
    #[prost(message, optional, tag = "4")]
    pub resulting_version: Option<ProtocolHashedVersion>,
    #[prost(message, optional, tag = "5")]
    pub snapshot: Option<WaveletSnapshot>,
    /// Defaults to false.
    #[prost(bool, optional, tag = "6")]
    pub marker: Option<bool>,
    #[prost(string, optional, tag = "7")]
    pub channel_id: Option<String>,
    /// Not in the original: the wavelet's history was rewound to this
    /// version after a change of host; `applied_delta` are then the deltas
    /// the client has seen since, which no longer exist.
    #[prost(message, optional, tag = "8")]
    pub reset_version: Option<ProtocolHashedVersion>,
}
wave_message!(ProtocolWaveletUpdate {
    required: [wavelet_name],
    nested: [commit_notice, resulting_version, snapshot, reset_version],
    repeated: [applied_delta],
});

/// A request to apply a delta to a wavelet.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProtocolSubmitRequest {
    /// Required.
    #[prost(string, optional, tag = "1")]
    pub wavelet_name: Option<String>,
    /// Required.
    #[prost(message, optional, tag = "2")]
    pub delta: Option<ProtocolWaveletDelta>,
    #[prost(string, optional, tag = "3")]
    pub channel_id: Option<String>,
}
wave_message!(ProtocolSubmitRequest {
    required: [wavelet_name, delta],
    nested: [delta],
});

/// The result of a submit request.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct ProtocolSubmitResponse {
    /// Required.
    #[prost(int32, optional, tag = "1")]
    pub operations_applied: Option<i32>,
    #[prost(string, optional, tag = "2")]
    pub error_message: Option<String>,
    #[prost(message, optional, tag = "3")]
    pub hashed_version_after_application: Option<ProtocolHashedVersion>,
    /// Resonator: an interim answer. The delta is queued on this server
    /// because its host cannot be reached; the final answer (with the
    /// version) follows on the same request once the host applied it.
    #[prost(bool, optional, tag = "4")]
    pub pending: Option<bool>,
}
wave_message!(ProtocolSubmitResponse {
    required: [operations_applied],
    nested: [hashed_version_after_application],
});
