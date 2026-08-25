//! Typed ABI v1 shapes crossing the wasm boundary as JSON.
//!
//! These structs are the contract between a mod plugin and the Resonator
//! mods host. The host serializes/deserializes the exact same JSON shapes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One engine value. Mirrors the protocol crate's `Value` 1:1; blobs cross
/// the boundary as base64 text.
///
/// JSON encoding (externally tagged, snake_case):
/// `"null"`, `{"integer": 5}`, `{"real": 1.5}`, `{"text": "hi"}`,
/// `{"blob": "<base64>"}`, `{"blob_ref": {"hash": "blake3:...", "bytes": 5}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    /// Base64-encoded bytes.
    Blob(String),
    BlobRef {
        hash: String,
        bytes: u64,
    },
}

impl Value {
    /// Builds a `Blob` from raw bytes (base64-encodes them).
    pub fn blob_from_bytes(bytes: &[u8]) -> Self {
        use base64::Engine as _;
        Value::Blob(base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    /// Decodes a `Blob` back to raw bytes; `None` for other variants or
    /// invalid base64.
    pub fn blob_bytes(&self) -> Option<Vec<u8>> {
        use base64::Engine as _;
        match self {
            Value::Blob(b64) => base64::engine::general_purpose::STANDARD
                .decode(b64.as_bytes())
                .ok(),
            _ => None,
        }
    }
}

/// Returned by the required `describe()` export, called once at load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Descriptor {
    /// ABI version; must be 1.
    pub abi: u32,
    /// Modulation name; must match the `_modulations` row.
    pub name: String,
    pub version: String,
    pub help_text: String,
    /// Help topics this mod contributes.
    #[serde(default)]
    pub topics: Vec<String>,
    /// Declared capability intent, e.g. "db_read", "db_write", "clock".
    /// The host refuses to load the mod if the granted capabilities do
    /// not cover this list.
    #[serde(default)]
    pub needs: Vec<String>,
}

/// Statement kind as carried by `StatementIn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Query,
    Execute,
}

/// Input to the required `handle()` export: one request statement plus the
/// budgets the host will enforce on emitted frames and db calls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatementIn {
    /// ULID of the request; already stamped on every emitted frame by the
    /// host, provided here for logging/idempotency.
    pub request_id: String,
    /// Requesting peer id.
    pub peer: String,
    pub kind: Kind,
    /// The signal text of the statement.
    pub text: String,
    #[serde(default)]
    pub params: Vec<Value>,
    pub row_limit: u64,
    pub byte_limit: u64,
    pub timeout_ms: u64,
}

/// Return value of `handle()`. Frames are emitted through host functions
/// during the call; this only reports final status.
///
/// JSON: `{"status": "done"}` or
/// `{"status": "error", "code": "...", "message": "..."}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum HandleResult {
    Done,
    Error { code: String, message: String },
}

impl HandleResult {
    pub fn done() -> Self {
        HandleResult::Done
    }

    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        HandleResult::Error {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// A property value in a [`FrameOut`]: a single value or a list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PropValue {
    One(Value),
    Many(Vec<Value>),
}

/// A generic outbound frame. The host serializes it to Turtle under the
/// `rsntr:` namespace (props keys become `rsntr:` local names), stamps
/// `rsntr:requestId`, and enforces the row/byte/frame-size budgets.
///
/// Conventions the builtin helpers follow:
///
/// - `Result` header: `props["column"]` is the ordered column list.
/// - `Row`: `props["seq"]` is the 1-based row number; each cell is stored
///   under `"col_" + <raw column name>` (the host applies the wire's
///   column-name encoding when serializing).
/// - `Done`: `props["rows"]` is the total row count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameOut {
    pub class: String,
    #[serde(default)]
    pub props: BTreeMap<String, PropValue>,
}

impl FrameOut {
    pub fn new(class: impl Into<String>) -> Self {
        FrameOut {
            class: class.into(),
            props: BTreeMap::new(),
        }
    }

    /// Adds a single-valued property (builder style).
    pub fn prop(mut self, name: impl Into<String>, value: Value) -> Self {
        self.props.insert(name.into(), PropValue::One(value));
        self
    }

    /// Adds a list-valued property (builder style).
    pub fn prop_list(mut self, name: impl Into<String>, values: Vec<Value>) -> Self {
        self.props.insert(name.into(), PropValue::Many(values));
        self
    }

    /// A `Result` header frame opening a row-streaming response.
    pub fn result(columns: &[&str]) -> Self {
        FrameOut::new("Result").prop_list(
            "column",
            columns
                .iter()
                .map(|c| Value::Text((*c).to_string()))
                .collect(),
        )
    }

    /// A `Row` frame. `seq` is 1-based; cells are (column name, value).
    pub fn row(seq: i64, cells: Vec<(String, Value)>) -> Self {
        let mut f = FrameOut::new("Row").prop("seq", Value::Integer(seq));
        for (name, value) in cells {
            f.props.insert(format!("col_{name}"), PropValue::One(value));
        }
        f
    }

    /// A `Done` trailer closing a response after `rows` rows.
    pub fn done(rows: i64) -> Self {
        FrameOut::new("Done").prop("rows", Value::Integer(rows))
    }
}

/// Result of the `db_query` host function.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DbRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// Result of the `db_execute` host function.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DbExec {
    pub rows_affected: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_json_shapes() {
        let cases = [
            (Value::Null, r#""null""#),
            (Value::Integer(5), r#"{"integer":5}"#),
            (Value::Real(1.5), r#"{"real":1.5}"#),
            (Value::Text("hi".into()), r#"{"text":"hi"}"#),
            (Value::blob_from_bytes(&[0, 1]), r#"{"blob":"AAE="}"#),
            (
                Value::BlobRef {
                    hash: "blake3:x".into(),
                    bytes: 5,
                },
                r#"{"blob_ref":{"hash":"blake3:x","bytes":5}}"#,
            ),
        ];
        for (v, want) in cases {
            assert_eq!(serde_json::to_string(&v).unwrap(), want);
            assert_eq!(serde_json::from_str::<Value>(want).unwrap(), v);
        }
    }

    #[test]
    fn handle_result_json_shapes() {
        assert_eq!(
            serde_json::to_string(&HandleResult::done()).unwrap(),
            r#"{"status":"done"}"#
        );
        assert_eq!(
            serde_json::to_string(&HandleResult::error("engine-error", "boom")).unwrap(),
            r#"{"status":"error","code":"engine-error","message":"boom"}"#
        );
    }

    #[test]
    fn statement_in_round_trip() {
        let json = r#"{"request_id":"01J","peer":"abc","kind":"query","text":"now",
            "params":["null",{"integer":1}],"row_limit":100,"byte_limit":1024,
            "timeout_ms":5000}"#;
        let st: StatementIn = serde_json::from_str(json).unwrap();
        assert_eq!(st.kind, Kind::Query);
        assert_eq!(st.params, vec![Value::Null, Value::Integer(1)]);
        let back: StatementIn = serde_json::from_str(&serde_json::to_string(&st).unwrap()).unwrap();
        assert_eq!(back, st);
    }

    #[test]
    fn frame_helpers() {
        let header = FrameOut::result(&["now"]);
        assert_eq!(
            serde_json::to_string(&header).unwrap(),
            r#"{"class":"Result","props":{"column":[{"text":"now"}]}}"#
        );
        let row = FrameOut::row(1, vec![("now".into(), Value::Text("t".into()))]);
        assert_eq!(
            serde_json::to_string(&row).unwrap(),
            r#"{"class":"Row","props":{"col_now":{"text":"t"},"seq":{"integer":1}}}"#
        );
        let done = FrameOut::done(1);
        assert_eq!(
            serde_json::to_string(&done).unwrap(),
            r#"{"class":"Done","props":{"rows":{"integer":1}}}"#
        );
        // Untagged PropValue must round-trip both arms.
        let back: FrameOut =
            serde_json::from_str(&serde_json::to_string(&header).unwrap()).unwrap();
        assert_eq!(back, header);
        let back: FrameOut = serde_json::from_str(&serde_json::to_string(&row).unwrap()).unwrap();
        assert_eq!(back, row);
    }

    #[test]
    fn descriptor_defaults() {
        let d: Descriptor =
            serde_json::from_str(r#"{"abi":1,"name":"time","version":"0.1.0","help_text":"h"}"#)
                .unwrap();
        assert!(d.topics.is_empty() && d.needs.is_empty());
    }
}
