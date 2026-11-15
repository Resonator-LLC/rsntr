//! The JSON form of the protocol messages, as spoken on the client
//! WebSocket and served by the fetch, profile, search and attachment-info
//! endpoints.
//!
//! This is the encoding produced by the `proto` and `gson` templates of
//! Apache Wave's `pst` message generator
//! (`org/waveprotocol/pst/templates/{proto,gson}/*.st`) together with
//! `GsonUtil`, `JsonLongHelper`, `Codec` and `Int52` of
//! `org.waveprotocol.wave.communication`; Apache Wave is licensed under the
//! Apache License, Version 2.0. The rules:
//!
//! - A message is a JSON object keyed by the decimal field number (`"1"`),
//!   fields in declaration order.
//! - An optional field is present only if set; a repeated field is always
//!   present as an array, possibly empty; a required field is always
//!   present.
//! - `string` is a JSON string, `bool` a JSON boolean, `int32` and enums
//!   JSON integers, a nested message a JSON object.
//! - `bytes` is a string of upper-case hexadecimal digits (`Codec`).
//! - `int64` is a two-element array `[low, high]` of the signed 32-bit
//!   halves (`GsonUtil.toJson(long)`), except for fields carrying the
//!   `int52` option (`ProtocolHashedVersion.version` and
//!   `ProtocolAppliedWaveletDelta.application_timestamp`), which are JSON
//!   numbers. The Java server holds those as `double` and so writes them
//!   the way `Double.toString` does (`7.0`, `1.2E10`); the writer here
//!   does the same.
//! - Strings are escaped as Gson's `JsonWriter` does without HTML safety:
//!   `"` and `\`, the short escapes `\t \b \n \r \f`, `\u00XX` for the
//!   other control characters, and ` `, ` `.
//!
//! Reading is as lenient as the original where that is harmless: unknown
//! keys are ignored, `null` counts as absent, `int52` fields accept any
//! integral JSON number, hexadecimal digits may be lower case, and the low
//! word of an `int64` may be given unsigned. Strings holding unpaired
//! UTF-16 surrogates cannot be parsed at all, because Rust strings cannot
//! hold them ([`parse`] reports them as invalid JSON).

use std::fmt::Write as _;

use serde_json::Value;
use thiserror::Error;

use super::app::{
    AppEventRequest, AppEventResponse, AppMediaRequest, AppOpenRequest, AppPair, AppUiNode,
    AppUpdate,
};
use super::attachment::{AttachmentMetadata, AttachmentsResponse, ImageMetadata};
use super::federation::protocol_document_operation::Component;
use super::federation::protocol_document_operation::component::{
    AnnotationBoundary, ElementStart, KeyValuePair, KeyValueUpdate, ReplaceAttributes,
    UpdateAttributes,
};
use super::federation::protocol_wavelet_operation::MutateDocument;
use super::federation::{
    ProtocolDocumentOperation, ProtocolHashedVersion, ProtocolWaveletDelta,
    ProtocolWaveletOperation,
};
use super::profile::profile_response::FetchedProfile;
use super::profile::{ProfileRequest, ProfileResponse};
use super::rpc::{CancelRpc, RpcFinished};
use super::search::search_response::Digest;
use super::search::{SearchRequest, SearchResponse};
use super::waveclient_rpc::{
    DocumentSnapshot, ProtocolAuthenticate, ProtocolAuthenticationResult, ProtocolOpenRequest,
    ProtocolSubmitRequest, ProtocolSubmitResponse, ProtocolWaveletUpdate, WaveViewSnapshot,
    WaveletSnapshot, WaveletVersion,
};
use super::{ProtoError, WaveMessage};

/// Failure to read a message from JSON.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum JsonError {
    /// The text is not JSON (this includes strings with unpaired
    /// surrogates).
    #[error("invalid JSON: {0}")]
    Syntax(String),
    /// A value has the wrong JSON type or is out of range.
    #[error("field {path}: expected {expected}")]
    Type {
        /// Field numbers from the outermost message inwards, dot-separated.
        path: String,
        expected: &'static str,
    },
    /// A required field is missing.
    #[error(transparent)]
    Proto(#[from] ProtoError),
    /// The envelope names a message type this server does not know.
    #[error("Unknown proto class: {0}")]
    UnknownType(String),
}

impl JsonError {
    fn expected(expected: &'static str) -> Self {
        JsonError::Type {
            path: String::new(),
            expected,
        }
    }

    /// Prefixes the path with the field the error occurred in.
    fn within(self, field: &str) -> Self {
        match self {
            JsonError::Type { path, expected } => JsonError::Type {
                path: if path.is_empty() {
                    field.to_owned()
                } else {
                    format!("{field}.{path}")
                },
                expected,
            },
            other => other,
        }
    }
}

/// A message with a JSON form.
pub trait JsonMessage: WaveMessage {
    /// The message's simple name, which is the `messageType` of the RPC
    /// envelope.
    const NAME: &'static str;

    /// Appends the JSON form. Does not check required fields.
    fn write_json(&self, out: &mut String);

    /// Reads the fields present in `value`. Does not check required fields.
    fn read_json(value: &Value) -> Result<Self, JsonError>;
}

/// The JSON form of a message, after checking its required fields.
pub fn to_json<M: JsonMessage>(message: &M) -> Result<String, ProtoError> {
    message.check_initialized()?;
    let mut out = String::new();
    message.write_json(&mut out);
    Ok(out)
}

/// Reads a message from parsed JSON and checks its required fields.
pub fn from_value<M: JsonMessage>(value: &Value) -> Result<M, JsonError> {
    let message = M::read_json(value)?;
    message.check_initialized()?;
    Ok(message)
}

/// Parses JSON text. Fails on unpaired surrogates in strings.
pub fn parse(text: &str) -> Result<Value, JsonError> {
    serde_json::from_str(text).map_err(|error| JsonError::Syntax(error.to_string()))
}

/// Reads a message from JSON text and checks its required fields.
pub fn from_json<M: JsonMessage>(text: &str) -> Result<M, JsonError> {
    from_value(&parse(text)?)
}

/// Appends `s` as a JSON string, escaped as Gson's `JsonWriter` does.
pub fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{c}' => out.push_str("\\f"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Appends an `int52` value the way Java's `Double.toString` prints the
/// `double` holding it.
fn write_int52(value: i64, out: &mut String) {
    if value.unsigned_abs() < 10_000_000 {
        let _ = write!(out, "{value}.0");
        return;
    }
    // Computerised scientific notation with the shortest digits that
    // identify the double.
    let scientific = format!("{:e}", value as f64);
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    out.push_str(mantissa);
    if !mantissa.contains('.') {
        out.push_str(".0");
    }
    out.push('E');
    out.push_str(exponent);
}

fn integral(value: &Value) -> Option<i64> {
    if let Some(n) = value.as_i64() {
        return Some(n);
    }
    let f = value.as_f64()?;
    // 2^63, which itself does not fit.
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    if f.fract() == 0.0 && (-LIMIT..LIMIT).contains(&f) {
        Some(f as i64)
    } else {
        None
    }
}

/// Field kinds: how a value of each protobuf type is written and read.
mod kind {
    use super::*;

    pub mod string {
        use super::*;
        pub fn write(value: &str, out: &mut String) {
            write_string(value, out);
        }
        pub fn read(value: &Value) -> Result<String, JsonError> {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or(JsonError::expected("a string"))
        }
    }

    pub mod boolean {
        use super::*;
        pub fn write(value: &bool, out: &mut String) {
            out.push_str(if *value { "true" } else { "false" });
        }
        pub fn read(value: &Value) -> Result<bool, JsonError> {
            value.as_bool().ok_or(JsonError::expected("a boolean"))
        }
    }

    pub mod int32 {
        use super::*;
        pub fn write(value: &i32, out: &mut String) {
            let _ = write!(out, "{value}");
        }
        pub fn read(value: &Value) -> Result<i32, JsonError> {
            integral(value)
                .and_then(|n| i32::try_from(n).ok())
                .ok_or(JsonError::expected("a 32-bit integer"))
        }
    }

    /// `int64` with the `int52` option: a JSON number.
    pub mod int52 {
        use super::*;
        pub fn write(value: &i64, out: &mut String) {
            write_int52(*value, out);
        }
        pub fn read(value: &Value) -> Result<i64, JsonError> {
            integral(value).ok_or(JsonError::expected("an integral number"))
        }
    }

    /// `int64` without the `int52` option: `[low, high]`.
    pub mod long {
        use super::*;
        pub fn write(value: &i64, out: &mut String) {
            let _ = write!(out, "[{},{}]", *value as i32, (*value >> 32) as i32);
        }
        pub fn read(value: &Value) -> Result<i64, JsonError> {
            let expected = JsonError::expected("an array of two 32-bit integers [low, high]");
            let Some([low, high]) = value.as_array().map(Vec::as_slice) else {
                return Err(expected);
            };
            let low = integral(low).filter(|n| (-(1i64 << 31)..(1i64 << 32)).contains(n));
            let high = integral(high).and_then(|n| i32::try_from(n).ok());
            match (low, high) {
                (Some(low), Some(high)) => Ok((i64::from(high) << 32) | (low & 0xFFFF_FFFF)),
                _ => Err(expected),
            }
        }
    }

    pub mod bytes {
        use super::*;
        pub fn write(value: &[u8], out: &mut String) {
            out.push('"');
            for byte in value {
                let _ = write!(out, "{byte:02X}");
            }
            out.push('"');
        }
        pub fn read(value: &Value) -> Result<Vec<u8>, JsonError> {
            value
                .as_str()
                .and_then(|s| hex::decode(s).ok())
                .ok_or(JsonError::expected("a string of hexadecimal digits"))
        }
    }

    pub mod message {
        use super::*;
        pub fn write<M: JsonMessage>(value: &M, out: &mut String) {
            value.write_json(out);
        }
        pub fn read<M: JsonMessage>(value: &Value) -> Result<M, JsonError> {
            M::read_json(value)
        }
    }
}

/// Implements [`JsonMessage`]. Each field is `number => name: label kind`
/// where the label is `one` (optional or required) or `many` (repeated).
macro_rules! json_message {
    ($type:ty, $name:literal { $($number:literal => $field:ident: $label:ident $kind:ident),* $(,)? }) => {
        impl JsonMessage for $type {
            const NAME: &'static str = $name;

            #[allow(unused_mut, unused_variables, unused_assignments)]
            fn write_json(&self, out: &mut String) {
                out.push('{');
                let mut first = true;
                $(json_message!(@write $label $kind, self.$field, $number, out, first);)*
                out.push('}');
            }

            #[allow(unused_mut, unused_variables)]
            fn read_json(value: &Value) -> Result<Self, JsonError> {
                let object = value
                    .as_object()
                    .ok_or(JsonError::expected(concat!("an object (", $name, ")")))?;
                let mut message = <$type>::default();
                $(
                    if let Some(value) = object.get(stringify!($number)).filter(|v| !v.is_null()) {
                        json_message!(@read $label $kind, message.$field, value, $number);
                    }
                )*
                Ok(message)
            }
        }
    };
    (@key $number:literal, $out:ident, $first:ident) => {
        if !$first {
            $out.push(',');
        }
        $first = false;
        $out.push_str(concat!("\"", stringify!($number), "\":"));
    };
    (@write one $kind:ident, $value:expr, $number:literal, $out:ident, $first:ident) => {
        if let Some(value) = &$value {
            json_message!(@key $number, $out, $first);
            kind::$kind::write(value, $out);
        }
    };
    (@write many $kind:ident, $value:expr, $number:literal, $out:ident, $first:ident) => {
        json_message!(@key $number, $out, $first);
        $out.push('[');
        for (index, value) in $value.iter().enumerate() {
            if index > 0 {
                $out.push(',');
            }
            kind::$kind::write(value, $out);
        }
        $out.push(']');
    };
    (@read one $kind:ident, $target:expr, $value:ident, $number:literal) => {
        $target = Some(
            kind::$kind::read($value).map_err(|e| e.within(stringify!($number)))?,
        );
    };
    (@read many $kind:ident, $target:expr, $value:ident, $number:literal) => {
        let array = $value
            .as_array()
            .ok_or(JsonError::expected("an array").within(stringify!($number)))?;
        for element in array {
            $target.push(
                kind::$kind::read(element).map_err(|e| e.within(stringify!($number)))?,
            );
        }
    };
}

// federation.protodevel

json_message!(ProtocolHashedVersion, "ProtocolHashedVersion" {
    1 => version: one int52,
    2 => history_hash: one bytes,
});

json_message!(ProtocolWaveletDelta, "ProtocolWaveletDelta" {
    1 => hashed_version: one message,
    2 => author: one string,
    3 => operation: many message,
    4 => address_path: many string,
});

json_message!(ProtocolWaveletOperation, "ProtocolWaveletOperation" {
    1 => add_participant: one string,
    2 => remove_participant: one string,
    3 => mutate_document: one message,
    4 => no_op: one boolean,
});

json_message!(MutateDocument, "MutateDocument" {
    1 => document_id: one string,
    2 => document_operation: one message,
});

json_message!(ProtocolDocumentOperation, "ProtocolDocumentOperation" {
    1 => component: many message,
});

json_message!(Component, "Component" {
    1 => annotation_boundary: one message,
    2 => characters: one string,
    3 => element_start: one message,
    4 => element_end: one boolean,
    5 => retain_item_count: one int32,
    6 => delete_characters: one string,
    7 => delete_element_start: one message,
    8 => delete_element_end: one boolean,
    9 => replace_attributes: one message,
    10 => update_attributes: one message,
});

json_message!(KeyValuePair, "KeyValuePair" {
    1 => key: one string,
    2 => value: one string,
});

json_message!(KeyValueUpdate, "KeyValueUpdate" {
    1 => key: one string,
    2 => old_value: one string,
    3 => new_value: one string,
});

json_message!(ElementStart, "ElementStart" {
    1 => r#type: one string,
    2 => attribute: many message,
});

json_message!(ReplaceAttributes, "ReplaceAttributes" {
    1 => empty: one boolean,
    2 => old_attribute: many message,
    3 => new_attribute: many message,
});

json_message!(UpdateAttributes, "UpdateAttributes" {
    1 => empty: one boolean,
    2 => attribute_update: many message,
});

json_message!(AnnotationBoundary, "AnnotationBoundary" {
    1 => empty: one boolean,
    2 => end: many string,
    3 => change: many message,
});

// waveclient-rpc.proto

json_message!(ProtocolAuthenticate, "ProtocolAuthenticate" {
    1 => token: one string,
});

json_message!(ProtocolAuthenticationResult, "ProtocolAuthenticationResult" {});

json_message!(ProtocolOpenRequest, "ProtocolOpenRequest" {
    1 => participant_id: one string,
    2 => wave_id: one string,
    3 => wavelet_id_prefix: many string,
    4 => known_wavelet: many message,
});

json_message!(WaveletVersion, "WaveletVersion" {
    1 => wavelet_id: one string,
    2 => hashed_version: one message,
});

json_message!(DocumentSnapshot, "DocumentSnapshot" {
    1 => document_id: one string,
    2 => document_operation: one message,
    3 => author: one string,
    4 => contributor: many string,
    5 => last_modified_version: one long,
    6 => last_modified_time: one long,
});

json_message!(WaveletSnapshot, "WaveletSnapshot" {
    1 => wavelet_id: one string,
    2 => participant_id: many string,
    3 => document: many message,
    4 => version: one message,
    5 => last_modified_time: one long,
    6 => creator: one string,
    7 => creation_time: one long,
});

json_message!(WaveViewSnapshot, "WaveViewSnapshot" {
    1 => wave_id: one string,
    2 => wavelet: many message,
});

json_message!(ProtocolWaveletUpdate, "ProtocolWaveletUpdate" {
    1 => wavelet_name: one string,
    2 => applied_delta: many message,
    3 => commit_notice: one message,
    4 => resulting_version: one message,
    5 => snapshot: one message,
    6 => marker: one boolean,
    7 => channel_id: one string,
    8 => reset_version: one message,
});

json_message!(ProtocolSubmitRequest, "ProtocolSubmitRequest" {
    1 => wavelet_name: one string,
    2 => delta: one message,
    3 => channel_id: one string,
});

json_message!(ProtocolSubmitResponse, "ProtocolSubmitResponse" {
    1 => operations_applied: one int32,
    2 => error_message: one string,
    3 => hashed_version_after_application: one message,
});

// rpc.proto

json_message!(CancelRpc, "CancelRpc" {});

json_message!(RpcFinished, "RpcFinished" {
    1 => failed: one boolean,
    2 => error_text: one string,
});

// profiles.proto

json_message!(ProfileRequest, "ProfileRequest" {
    1 => addresses: many string,
});

json_message!(ProfileResponse, "ProfileResponse" {
    1 => profiles: many message,
});

json_message!(FetchedProfile, "FetchedProfile" {
    1 => address: one string,
    2 => name: one string,
    3 => image_url: one string,
    4 => profile_url: one string,
});

// search.proto

json_message!(SearchRequest, "SearchRequest" {
    1 => query: one string,
    2 => index: one int32,
    3 => num_results: one int32,
});

json_message!(SearchResponse, "SearchResponse" {
    1 => query: one string,
    2 => total_results: one int32,
    3 => digests: many message,
});

json_message!(Digest, "Digest" {
    1 => title: one string,
    2 => snippet: one string,
    3 => wave_id: one string,
    4 => last_modified: one long,
    5 => unread_count: one int32,
    6 => blip_count: one int32,
    7 => participants: many string,
    8 => author: one string,
});

// Wave apps (not in the original protocol).

json_message!(AppOpenRequest, "AppOpenRequest" {
    1 => wave_id: one string,
    2 => gadget_id: one string,
    3 => view: one int32,
});

json_message!(AppPair, "AppPair" {
    1 => name: one string,
    2 => value: one string,
});

json_message!(AppUiNode, "AppUiNode" {
    1 => parent: one int32,
    2 => tag: one string,
    3 => text: one string,
    4 => attrs: many message,
    5 => on: many string,
});

json_message!(AppUpdate, "AppUpdate" {
    1 => status: one string,
    2 => reason: one string,
    3 => app_id: one string,
    4 => name: one string,
    5 => permissions: many string,
    6 => nodes: many message,
    7 => media: many string,
});

json_message!(AppMediaRequest, "AppMediaRequest" {
    1 => wave_id: one string,
    2 => gadget_id: one string,
    3 => data: one string,
});

json_message!(AppEventRequest, "AppEventRequest" {
    1 => wave_id: one string,
    2 => gadget_id: one string,
    3 => view: one int32,
    4 => node: one int32,
    5 => name: one string,
    6 => value: one string,
    7 => fields: many message,
});

json_message!(AppEventResponse, "AppEventResponse" {
    1 => error_message: one string,
});

// attachment.proto

json_message!(AttachmentsResponse, "AttachmentsResponse" {
    1 => attachment: many message,
});

json_message!(AttachmentMetadata, "AttachmentMetadata" {
    1 => attachment_id: one string,
    2 => wave_ref: one string,
    3 => file_name: one string,
    4 => mime_type: one string,
    5 => size: one long,
    6 => creator: one string,
    7 => attachment_url: one string,
    8 => thumbnail_url: one string,
    9 => image_metadata: one message,
    10 => thumbnail_metadata: one message,
    11 => malware: one boolean,
});

json_message!(ImageMetadata, "ImageMetadata" {
    1 => width: one int32,
    2 => height: one int32,
});

// The RPC envelope of `WebSocketChannel.MessageWrapper`.

/// A parsed RPC envelope: `{"messageType": ..., "sequenceNumber": ...,
/// "message": ...}`.
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub message_type: String,
    pub sequence_number: i32,
    /// The message body, still as JSON.
    pub message: Value,
}

impl Envelope {
    /// Parses an envelope. Fails on unpaired surrogates anywhere in the
    /// text.
    pub fn parse(text: &str) -> Result<Self, JsonError> {
        let mut value = parse(text)?;
        let object = value
            .as_object_mut()
            .ok_or(JsonError::expected("an envelope object"))?;
        let message_type = object
            .get("messageType")
            .and_then(Value::as_str)
            .ok_or(JsonError::expected("a string").within("messageType"))?
            .to_owned();
        let sequence_number = object
            .get("sequenceNumber")
            .and_then(integral)
            .and_then(|n| i32::try_from(n).ok())
            .ok_or(JsonError::expected("a 32-bit integer").within("sequenceNumber"))?;
        let message = object.remove("message").unwrap_or(Value::Null);
        Ok(Envelope {
            message_type,
            sequence_number,
            message,
        })
    }

    /// The body as a message of type `M`, with required fields checked.
    pub fn body<M: JsonMessage>(&self) -> Result<M, JsonError> {
        from_value(&self.message)
    }
}

/// The envelope text for a message: `messageType`, `sequenceNumber` and
/// `message`, in that order.
pub fn envelope<M: JsonMessage>(sequence_number: i32, message: &M) -> Result<String, ProtoError> {
    message.check_initialized()?;
    let mut out = String::with_capacity(128);
    out.push_str("{\"messageType\":");
    write_string(M::NAME, &mut out);
    let _ = write!(out, ",\"sequenceNumber\":{sequence_number},\"message\":");
    message.write_json(&mut out);
    out.push('}');
    Ok(out)
}

#[cfg(test)]
mod tests;
