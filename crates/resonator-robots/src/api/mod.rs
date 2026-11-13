//! The data model and JSON of the Wave Robots API / Data API.
//!
//! Derived from `com.google.wave.api` of Apache Wave (`OperationRequest`,
//! `OperationType`, `JsonRpcConstant`, `JsonRpcResponse`, `BlipData`,
//! `BlipThread`, `Element` and its subclasses, `Annotation`, `Range`,
//! `SearchResult`, `ParticipantProfile`, `impl.WaveletData`,
//! `impl.DocumentModifyAction`, `impl.DocumentModifyQuery`,
//! `impl.EventMessageBundle`, `impl.GsonFactory` and the Gson adaptors,
//! `event.*`, `robot.RobotName`, `RobotSerializer`), licensed under the
//! Apache License, Version 2.0.
//!
//! The JSON is what the original's Gson configuration reads and writes:
//! the same property names and enum spellings, `null` for absent values
//! (Gson is set to serialize nulls), numbers as JSON numbers, `byte[]` as
//! base64 strings. Property order within an object is not significant and
//! is not always the original's (which, for response data, depends on Java
//! hash codes). Gson's HTML-safe escaping (`<` for `<` and so on) is
//! not reproduced; the JSON values are equal.
//!
//! Text positions (ranges, element positions, thread locations) are in
//! UTF-16 code units, as in the original.
//!
//! The client half of the package (`Wavelet`, `Blip`, `OperationQueue`,
//! `AbstractRobot`, OAuth) is not ported.

mod event;
pub(crate) mod lenient;
mod request;
mod robot_name;
mod types;

pub use event::{Event, EventMessageBundle, EventProperties};
pub use lenient::{ByteArrays, base64_bytes};
pub use request::{
    InvalidRequest, JsonRpcResponse, OperationRequest, OperationType, ParamKind, ParamsProperty,
    deserialize_operations, protocol_version_of,
};
pub use resonator_server::account::{
    Capability, Context, EventType, ProtocolVersion, RobotCapabilities,
};
pub use robot_name::RobotName;
pub use types::{
    Annotation, ApiWaveletId, BlipData, BlipThread, BundledAnnotation, DocumentModifyAction,
    DocumentModifyQuery, Element, ElementType, FetchProfilesRequest, FetchProfilesResult,
    ModifyHow, ParticipantProfile, Range, RawAttachmentData, SearchDigest, SearchResult,
    WaveletData,
};

/// The prefix of ids that stand for a wave, wavelet or blip created
/// earlier in the same batch of operations
/// (`OperationContext.TEMP_ID_MARKER`).
pub const TEMP_ID_MARKER: &str = "TBD_";

#[cfg(test)]
mod tests;
