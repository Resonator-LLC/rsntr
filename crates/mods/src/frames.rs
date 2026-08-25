//! ABI `FrameOut` -> protocol envelope objects.
//!
//! The conventional classes map onto the real protocol frames, so
//! standard clients render plugin responses exactly like sql-sqlite
//! ones: `Result` (props["column"]), `Row` (props["seq"] plus cells
//! under `col_<raw name>`), `Done` (props["rows"]). The envelope codec
//! applies the wire's percent column-name encoding when it serializes
//! Row cells, so cell names pass through raw here. Any other class
//! becomes an `EnvelopeObject::Generic` under `rsntr:`, with the request
//! id stamped.

use extism::Error;
use oxrdf::{Literal, NamedNode, Term};

use resonator_mod_pdk::{FrameOut, PropValue, Value as PdkValue};
use resonator_protocol::vocab::{RSNTR_NS, XSD_BASE64_BINARY, prop};
use resonator_protocol::{Done, EnvelopeObject, Generic, ResultHeader, Row, encode_column_name};

use crate::host::pdk_to_proto;

fn values(pv: &PropValue) -> &[PdkValue] {
    match pv {
        PropValue::One(v) => std::slice::from_ref(v),
        PropValue::Many(vs) => vs,
    }
}

fn one_integer(f: &FrameOut, key: &str) -> Option<i64> {
    match f.props.get(key) {
        Some(PropValue::One(PdkValue::Integer(n))) => Some(*n),
        _ => None,
    }
}

/// Converts one emitted frame; the request id comes from the host, never
/// the plugin.
pub(crate) fn frame_to_envelope(request_id: &str, f: &FrameOut) -> Result<EnvelopeObject, Error> {
    match f.class.as_str() {
        "Result" => {
            let mut columns = Vec::new();
            if let Some(pv) = f.props.get("column") {
                for v in values(pv) {
                    match v {
                        PdkValue::Text(s) => columns.push(s.clone()),
                        other => {
                            return Err(Error::msg(format!(
                                "Result column entries must be text, got {other:?}"
                            )));
                        }
                    }
                }
            }
            Ok(EnvelopeObject::Result(ResultHeader {
                id: request_id.to_string(),
                columns,
                decl_types: Vec::new(),
            }))
        }
        "Row" => {
            let seq = one_integer(f, "seq")
                .ok_or_else(|| Error::msg("Row frame needs an integer 'seq' prop"))?;
            let mut cells = Vec::new();
            for (key, pv) in &f.props {
                if key == "seq" {
                    continue;
                }
                let Some(name) = key.strip_prefix("col_") else {
                    return Err(Error::msg(format!(
                        "unknown Row prop {key:?}: cells go under \"col_\" + column name"
                    )));
                };
                let PropValue::One(v) = pv else {
                    return Err(Error::msg(format!(
                        "Row cell {name:?} must be a single value"
                    )));
                };
                match pdk_to_proto(v)? {
                    // NULL cells use the wire's column-omission encoding.
                    resonator_protocol::Value::Null => {}
                    value => cells.push((name.to_string(), value)),
                }
            }
            Ok(EnvelopeObject::Row(vec![Row { seq, cells }]))
        }
        "Done" => Ok(EnvelopeObject::Done(Done {
            id: request_id.to_string(),
            row_count: one_integer(f, "rows"),
            affected_rows: None,
            last_insert_rowid: None,
            truncated: false,
        })),
        other => generic_frame(request_id, other, f),
    }
}

fn generic_frame(request_id: &str, class: &str, f: &FrameOut) -> Result<EnvelopeObject, Error> {
    let plain = !class.is_empty()
        && class
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if !plain {
        return Err(Error::msg(format!(
            "frame class {class:?} is not a plain rsntr local name"
        )));
    }
    let mut props: Vec<(NamedNode, Term)> = Vec::new();
    // The host stamps the request id; a plugin-supplied "id" prop is
    // ignored in favor of it.
    props.push((
        prop::ID.into_owned(),
        Literal::new_simple_literal(request_id).into(),
    ));
    for (key, pv) in &f.props {
        if key == "id" {
            continue;
        }
        let pred = NamedNode::new(format!("{RSNTR_NS}{}", encode_column_name(key)))
            .map_err(|e| Error::msg(format!("prop name {key:?} does not form an IRI: {e}")))?;
        for v in values(pv) {
            props.push((pred.clone(), value_term(v)?));
        }
    }
    Ok(EnvelopeObject::Generic(Generic {
        class: class.to_string(),
        props,
    }))
}

fn value_term(v: &PdkValue) -> Result<Term, Error> {
    Ok(match v {
        PdkValue::Null => Term::NamedNode(prop::NULL.into_owned()),
        PdkValue::Integer(n) => {
            Literal::new_typed_literal(n.to_string(), oxrdf::vocab::xsd::INTEGER).into()
        }
        PdkValue::Real(f) => {
            Literal::new_typed_literal(double_lex(*f), oxrdf::vocab::xsd::DOUBLE).into()
        }
        PdkValue::Text(s) => Literal::new_simple_literal(s).into(),
        PdkValue::Blob(b64) => {
            if v.blob_bytes().is_none() {
                return Err(Error::msg("blob value is not valid base64"));
            }
            Literal::new_typed_literal(b64.clone(), XSD_BASE64_BINARY).into()
        }
        PdkValue::BlobRef { .. } => {
            return Err(Error::msg(
                "blob_ref values are not supported in emitted frames yet",
            ));
        }
    })
}

/// Shortest-round-trip `xsd:double` lexical form (the wire convention).
fn double_lex(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_owned()
    } else if f == f64::INFINITY {
        "INF".to_owned()
    } else if f == f64::NEG_INFINITY {
        "-INF".to_owned()
    } else {
        format!("{f:e}")
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    #[test]
    fn conventional_frames_map_to_protocol() {
        let header = frame_to_envelope(ID, &FrameOut::result(&["a", "b"])).unwrap();
        let EnvelopeObject::Result(h) = header else {
            panic!("not a Result");
        };
        assert_eq!(h.id, ID);
        assert_eq!(h.columns, vec!["a".to_string(), "b".to_string()]);

        let row = frame_to_envelope(
            ID,
            &FrameOut::row(
                1,
                vec![
                    ("a".into(), PdkValue::Integer(7)),
                    ("b".into(), PdkValue::Null),
                ],
            ),
        )
        .unwrap();
        let EnvelopeObject::Row(rows) = row else {
            panic!("not a Row");
        };
        assert_eq!(rows[0].seq, 1);
        // The NULL cell is omitted (column-omission encoding).
        assert_eq!(
            rows[0].cells,
            vec![("a".to_string(), resonator_protocol::Value::Integer(7))]
        );

        let done = frame_to_envelope(ID, &FrameOut::done(3)).unwrap();
        let EnvelopeObject::Done(d) = done else {
            panic!("not a Done");
        };
        assert_eq!(d.id, ID);
        assert_eq!(d.row_count, Some(3));
    }

    #[test]
    fn generic_frames_stamp_the_id_and_serialize() {
        let f = FrameOut::new("Time")
            .prop("recvNs", PdkValue::Integer(5))
            .prop("note", PdkValue::Text("x".into()));
        let obj = frame_to_envelope(ID, &f).unwrap();
        let EnvelopeObject::Generic(g) = &obj else {
            panic!("not Generic");
        };
        assert_eq!(g.class, "Time");
        assert!(g.props.iter().any(|(p, _)| p.as_str().ends_with("#id")));
        assert!(g.props.iter().any(|(p, _)| p.as_str().ends_with("#recvNs")));
        // The codec must accept it.
        let turtle = obj.to_turtle().unwrap();
        assert!(turtle.contains("Time"), "{turtle}");
        assert!(turtle.contains(ID), "{turtle}");

        // A class that is not a plain local name is refused.
        assert!(frame_to_envelope(ID, &FrameOut::new("bad class")).is_err());
        // Row frames insist on the col_ convention.
        let mut bad = FrameOut::new("Row").prop("seq", PdkValue::Integer(1));
        bad.props
            .insert("loose".into(), PropValue::One(PdkValue::Integer(1)));
        assert!(frame_to_envelope(ID, &bad).is_err());
    }
}
