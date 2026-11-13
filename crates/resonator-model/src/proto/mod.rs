//! Protobuf wire and storage encoding of deltas, operations and snapshots.
//!
//! Message definitions follow `federation.protodevel`,
//! `box/server/persistence/protos/delta-store.proto`,
//! `box/common/comms/waveclient-rpc.proto`, `box/server/rpc/rpc.proto`,
//! `box/profile/profiles.proto`, `box/search/search.proto` and
//! `box/attachment/attachment.proto` of Apache Wave (plus `federation_error.protodevel`,
//! and resonator's own federation wire protocol in [`federation_rpc`]); the converters
//! follow `CoreWaveletOperationSerializer`, `SnapshotSerializer`,
//! `ProtoDeltaStoreDataSerializer`, `AppliedDeltaUtil` and
//! `ByteStringMessage` of its server. Apache Wave is licensed under the
//! Apache License, Version 2.0.
//!
//! The messages are proto2. Every field, required or not, is an `Option` so
//! that presence survives decoding; [`decode`] and [`encode`] check that
//! required fields are present, as the Java runtime does when parsing and
//! building. Encoded bytes match the Java runtime for the same message,
//! which matters because history hashes are computed over them. Unknown
//! fields are dropped when decoding, so bytes that are hashed or signed must
//! be kept as received (see [`ByteStringMessage`]).

pub mod attachment;
pub mod convert;
pub mod delta_store;
pub mod federation;
pub mod federation_error;
pub mod federation_rpc;
pub mod json;
pub mod profile;
pub mod rpc;
pub mod search;
pub mod waveclient_rpc;

#[cfg(test)]
mod tests;

pub use convert::*;

use thiserror::Error;

use crate::data::DataError;
use crate::docop::OperationError;
use crate::id::{InvalidIdError, InvalidParticipantAddress};
use crate::operation::{DeltaError, WaveletOperationError};

/// Failure to encode, decode or convert a message.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtoError {
    /// The bytes are not a valid encoding of the message
    /// (`InvalidProtocolBufferException`).
    #[error("invalid protocol buffer: {0}")]
    Decode(#[from] prost::DecodeError),
    /// A required field is not set.
    #[error("message missing required field: {0}")]
    MissingField(&'static str),
    /// A field holds a value the model cannot represent, such as a negative
    /// version or retain count.
    #[error("invalid value for {field}: {message}")]
    InvalidValue {
        field: &'static str,
        message: String,
    },
    /// The model value has no encoding (`IllegalArgumentException` in the
    /// original serializer).
    #[error("Unsupported {0}")]
    Unsupported(String),
    #[error(transparent)]
    Operation(#[from] OperationError),
    #[error(transparent)]
    WaveletOperation(#[from] WaveletOperationError),
    #[error(transparent)]
    Delta(#[from] DeltaError),
    #[error(transparent)]
    Data(#[from] DataError),
    #[error(transparent)]
    ParticipantId(#[from] InvalidParticipantAddress),
    #[error(transparent)]
    Id(#[from] InvalidIdError),
}

/// A message that knows its required fields.
pub trait WaveMessage: prost::Message + Default {
    /// Fails if a required field of this message or of a message nested in
    /// it is not set (`isInitialized` in the Java runtime).
    fn check_initialized(&self) -> Result<(), ProtoError>;
}

/// Decodes a message and checks that its required fields are present.
pub fn decode<M: WaveMessage>(bytes: &[u8]) -> Result<M, ProtoError> {
    let message = M::decode(bytes)?;
    message.check_initialized()?;
    Ok(message)
}

/// Encodes a message after checking that its required fields are present.
pub fn encode<M: WaveMessage>(message: &M) -> Result<Vec<u8>, ProtoError> {
    message.check_initialized()?;
    Ok(message.encode_to_vec())
}

/// Implements [`WaveMessage`]: `required` lists required fields, `nested`
/// singular message fields and `repeated` repeated message fields to
/// descend into.
macro_rules! wave_message {
    ($type:ty { $(required: [$($required:ident),* $(,)?] $(,)?)?
                $(nested: [$($nested:ident),* $(,)?] $(,)?)?
                $(repeated: [$($repeated:ident),* $(,)?] $(,)?)? }) => {
        impl $crate::proto::WaveMessage for $type {
            fn check_initialized(&self) -> Result<(), $crate::proto::ProtoError> {
                $($(
                    if self.$required.is_none() {
                        return Err($crate::proto::ProtoError::MissingField(concat!(
                            stringify!($type), ".", stringify!($required)
                        )));
                    }
                )*)?
                $($(
                    if let Some(message) = &self.$nested {
                        $crate::proto::WaveMessage::check_initialized(message)?;
                    }
                )*)?
                $($(
                    for message in &self.$repeated {
                        $crate::proto::WaveMessage::check_initialized(message)?;
                    }
                )*)?
                Ok(())
            }
        }
    };
}
pub(crate) use wave_message;
