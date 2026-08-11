//! Rendering for terminals and the stable `--json` machine output.
//!
//! The JSON shapes here are a documented, stable surface for agents and
//! shell pipelines: every command emits exactly one JSON object on
//! stdout (except `entrain`, which emits one object per event line, and
//! `watch`, whose stdout is the raw byte feed and whose JSON goes to
//! stderr).

use serde_json::{Value as Json, json};

use resonator_protocol::{Point, PointField, PointKind, Projection, Row, Value};

use crate::client::{QueryOutcome, QueryReport};
use crate::{EXIT_DENIED, EXIT_ERROR, EXIT_OK, EXIT_TIMEOUT};

/// Renders one cell for terminal output.
pub fn render_value(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => f.to_string(),
        Value::Text(t) => t.clone(),
        Value::Blob(b) => {
            let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
            format!("x'{hex}'")
        }
        Value::BlobRef { hash, bytes } => format!("blobref({hash}, {bytes} bytes)"),
    }
}

/// Renders one row in header column order, NULLs restored for the
/// omitted cells.
pub fn render_row(columns: &[String], row: &Row) -> String {
    columns
        .iter()
        .map(|name| {
            row.cells
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| render_value(v))
                .unwrap_or_else(|| "NULL".to_string())
        })
        .collect::<Vec<_>>()
        .join("\t")
}

/// One engine value as JSON. Blobs become `{"blob_hex": ...}`, blob refs
/// `{"blobref": {hash, bytes}}`; a non-finite real falls back to its
/// string form (JSON has no NaN).
pub fn value_to_json(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Integer(i) => json!(i),
        Value::Real(f) => match serde_json::Number::from_f64(*f) {
            Some(n) => Json::Number(n),
            None => json!(f.to_string()),
        },
        Value::Text(t) => json!(t),
        Value::Blob(b) => {
            let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
            json!({ "blob_hex": hex })
        }
        Value::BlobRef { hash, bytes } => json!({ "blobref": { "hash": hash, "bytes": bytes } }),
    }
}

/// One row as a JSON array in header column order, nulls restored.
pub fn row_to_json(columns: &[String], row: &Row) -> Json {
    Json::Array(
        columns
            .iter()
            .map(|name| {
                row.cells
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| value_to_json(v))
                    .unwrap_or(Json::Null)
            })
            .collect(),
    )
}

/// One triple as an N-Triples statement line.
pub fn triple_string(t: &oxrdf::Triple) -> String {
    format!("{t} .")
}

/// The exit code implied by a wire error code: `timeout` maps to 3,
/// everything else to 1.
pub fn error_exit_code(code: &str) -> i32 {
    if code == "timeout" {
        EXIT_TIMEOUT
    } else {
        EXIT_ERROR
    }
}

/// The stable JSON object for one query/help report.
pub fn report_to_json(report: &QueryReport) -> Json {
    let id = &report.id;
    match &report.outcome {
        QueryOutcome::Rows {
            columns,
            rows,
            done,
        } => json!({
            "ok": true,
            "id": id,
            "columns": columns,
            "rows": rows.iter().map(|r| row_to_json(columns, r)).collect::<Vec<_>>(),
            "row_count": done.row_count,
            "affected_rows": done.affected_rows,
            "last_insert_rowid": done.last_insert_rowid,
            "truncated": done.truncated,
        }),
        QueryOutcome::Graph { triples, done } => json!({
            "ok": true,
            "id": id,
            "triples": triples.iter().map(triple_string).collect::<Vec<_>>(),
            "triple_count": done.row_count,
            "truncated": done.truncated,
        }),
        QueryOutcome::Help { text, topics } => json!({
            "ok": true,
            "id": id,
            "help": text,
            "topics": topics,
        }),
        QueryOutcome::Denied(d) => json!({
            "ok": false,
            "id": id,
            "denied": d.reason,
        }),
        QueryOutcome::Failed(e) => json!({
            "ok": false,
            "id": id,
            "error": { "code": e.code, "reason": e.reason },
        }),
    }
}

/// The exit code implied by a report's outcome.
pub fn report_exit_code(report: &QueryReport) -> i32 {
    match &report.outcome {
        QueryOutcome::Rows { .. } | QueryOutcome::Graph { .. } | QueryOutcome::Help { .. } => {
            EXIT_OK
        }
        QueryOutcome::Denied(_) => EXIT_DENIED,
        QueryOutcome::Failed(e) => error_exit_code(&e.code),
    }
}

/// A point kind as its stable JSON string.
pub fn kind_string(kind: &PointKind) -> String {
    match kind {
        PointKind::Excitable => "excitable".to_string(),
        PointKind::Radiant => "radiant".to_string(),
        PointKind::Sympathetic => "sympathetic".to_string(),
        PointKind::Bare => "bare".to_string(),
        PointKind::Other(iri) => iri.clone(),
    }
}

fn field_to_json(f: &PointField) -> Json {
    json!({
        "name": f.name,
        "datatype": f.datatype,
        "required": f.required,
        "default": f.default.as_ref().map(value_to_json),
        "one_of": f.one_of.iter().map(value_to_json).collect::<Vec<_>>(),
        "hint": f.hint,
    })
}

fn point_to_json(p: &Point) -> Json {
    json!({
        "iri": p.iri,
        "kind": kind_string(&p.kind),
        "label": p.label,
        "comment": p.comment,
        "icon": p.icon,
        "role": p.role,
        "projects": p.projects,
        "coupling": p.coupling.iter().map(field_to_json).collect::<Vec<_>>(),
        "modulation": p.modulation,
        "signal": p.signal,
        "params_order": p.params_order,
        "signal_template": p.signal_template,
        "fires": p.fires,
    })
}

/// The stable JSON object for one projection.
pub fn projection_to_json(path: &str, p: &Projection) -> Json {
    json!({
        "ok": true,
        "path": path,
        "offers": p.offers.iter().map(point_to_json).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_to_json_shapes() {
        assert_eq!(value_to_json(&Value::Null), Json::Null);
        assert_eq!(value_to_json(&Value::Integer(7)), json!(7));
        assert_eq!(value_to_json(&Value::Real(1.5)), json!(1.5));
        assert_eq!(value_to_json(&Value::Real(f64::NAN)), json!("NaN"));
        assert_eq!(value_to_json(&Value::Text("x".into())), json!("x"));
        assert_eq!(
            value_to_json(&Value::Blob(vec![0xab, 0x01])),
            json!({ "blob_hex": "ab01" })
        );
    }

    #[test]
    fn row_json_restores_nulls_in_header_order() {
        let columns = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let row = Row {
            seq: 0,
            cells: vec![
                ("c".to_string(), Value::Integer(3)),
                ("a".to_string(), Value::Text("x".to_string())),
            ],
        };
        assert_eq!(row_to_json(&columns, &row), json!(["x", null, 3]));
        assert_eq!(render_row(&columns, &row), "x\tNULL\t3");
    }

    #[test]
    fn exit_codes_are_stable() {
        assert_eq!(error_exit_code("timeout"), EXIT_TIMEOUT);
        assert_eq!(error_exit_code("engine-error"), EXIT_ERROR);
        assert_eq!(error_exit_code("auth-denied"), EXIT_ERROR);
    }
}
