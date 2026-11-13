//! Messages of `box/server/rpc/rpc.proto` (package `rpc`): the control
//! messages of the client RPC channel.

use super::wave_message;

/// Sent by the client to ask that an RPC, streaming or not, be cancelled.
/// The server still finishes the RPC in the standard manner.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct CancelRpc {}
wave_message!(CancelRpc {});

/// Sent by the server when a streaming RPC has finished (failed or not) or
/// when a normal RPC has failed.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct RpcFinished {
    /// Required.
    #[prost(bool, optional, tag = "1")]
    pub failed: Option<bool>,
    #[prost(string, optional, tag = "2")]
    pub error_text: Option<String>,
}
wave_message!(RpcFinished { required: [failed] });

impl RpcFinished {
    /// A streaming RPC that ended normally.
    pub fn success() -> Self {
        RpcFinished {
            failed: Some(false),
            error_text: None,
        }
    }

    /// An RPC that failed with `error_text`.
    pub fn failure(error_text: impl Into<String>) -> Self {
        RpcFinished {
            failed: Some(true),
            error_text: Some(error_text.into()),
        }
    }
}
