//! Every EnvelopeObject variant survives encode -> frame -> decode
//! unchanged: property tests over the value-carrying variants plus explicit
//! edge-value and per-class regression tests.

use bytes::BytesMut;
use oxrdf::{Literal, NamedNode, Term, Triple};
use proptest::prelude::*;
use resonator_protocol::{
    AudioDuplex, Damp, Decision, Denied, Done, Entrain, EnvelopeObject, ErrorEnvelope, Generic,
    Graph, Hello, Help, Knock, Media, Point, PointField, PointKind, Presence, Projection,
    ResultHeader, Row, Statement, Value, Vibration, decode_envelope, encode_envelope,
};

fn round_trip(obj: &EnvelopeObject) -> EnvelopeObject {
    let mut buf = BytesMut::new();
    encode_envelope(obj, &mut buf).expect("encode");
    let got = decode_envelope(&mut buf)
        .expect("decode")
        .expect("complete frame");
    assert!(buf.is_empty(), "frame not fully consumed");
    got
}

fn assert_round_trips(obj: EnvelopeObject) {
    let got = round_trip(&obj);
    assert_eq!(got, obj, "wire form:\n{}", obj.to_turtle().unwrap());
}

// --- strategies -----------------------------------------------------------

/// Doubles across the whole space; NaN is collapsed to 0.0 because NaN
/// payload bits are not representable in the "NaN" lexical token (a
/// dedicated regression test covers NaN itself).
fn any_real() -> impl Strategy<Value = f64> {
    any::<f64>().prop_map(|f| if f.is_nan() { 0.0 } else { f })
}

fn non_null_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<i64>().prop_map(Value::Integer),
        any_real().prop_map(Value::Real),
        any::<String>().prop_map(Value::Text),
        proptest::collection::vec(any::<u8>(), 0..128).prop_map(Value::Blob),
        (any::<String>(), any::<u64>()).prop_map(|(hash, bytes)| Value::BlobRef { hash, bytes }),
    ]
}

fn any_value() -> impl Strategy<Value = Value> {
    prop_oneof![Just(Value::Null), non_null_value()]
}

prop_compose! {
    fn any_statement()(
        id in any::<String>(),
        modulation in any::<String>(),
        signal in any::<String>(),
        params in proptest::collection::vec(any_value(), 0..6),
        database in proptest::option::of(any::<String>()),
        row_limit in proptest::option::of(any::<i64>()),
        byte_limit in proptest::option::of(any::<i64>()),
        timeout_ms in proptest::option::of(any::<i64>()),
    ) -> Statement {
        Statement { id, modulation, signal, params, database, row_limit, byte_limit, timeout_ms }
    }
}

prop_compose! {
    fn any_row()(
        seq in any::<i64>(),
        cells in proptest::collection::btree_map(any::<String>(), non_null_value(), 0..5),
    ) -> Row {
        Row { seq, cells: cells.into_iter().collect() }
    }
}

prop_compose! {
    fn any_result()(
        id in any::<String>(),
        columns in proptest::collection::vec(any::<String>(), 0..5),
        decl_types in proptest::collection::vec(any::<String>(), 0..5),
    ) -> ResultHeader {
        ResultHeader { id, columns, decl_types }
    }
}

prop_compose! {
    fn any_done()(
        id in any::<String>(),
        row_count in proptest::option::of(any::<i64>()),
        affected_rows in proptest::option::of(any::<i64>()),
        last_insert_rowid in proptest::option::of(any::<i64>()),
        truncated in any::<bool>(),
    ) -> Done {
        Done { id, row_count, affected_rows, last_insert_rowid, truncated }
    }
}

prop_compose! {
    fn any_hello()(
        envelope_version in any::<String>(),
        encodings in proptest::collection::vec(any::<String>(), 0..4),
        mods in proptest::collection::vec(any::<String>(), 0..4),
        hint in proptest::option::of(any::<String>()),
    ) -> Hello {
        Hello { envelope_version, encodings, mods, hint }
    }
}

prop_compose! {
    fn any_help()(
        id in proptest::option::of(any::<String>()),
        signal in any::<String>(),
        topics in proptest::collection::vec(any::<String>(), 0..5),
    ) -> Help {
        Help { id, signal, topics }
    }
}

fn any_envelope() -> impl Strategy<Value = EnvelopeObject> {
    prop_oneof![
        any_statement().prop_map(EnvelopeObject::Query),
        any_statement().prop_map(EnvelopeObject::Execute),
        any_result().prop_map(EnvelopeObject::Result),
        proptest::collection::vec(any_row(), 1..4).prop_map(EnvelopeObject::Row),
        any_done().prop_map(EnvelopeObject::Done),
        (
            proptest::option::of(any::<String>()),
            proptest::option::of(any::<String>())
        )
            .prop_map(|(id, reason)| EnvelopeObject::Denied(Denied { id, reason })),
        (
            proptest::option::of(any::<String>()),
            any::<String>(),
            proptest::option::of(any::<String>())
        )
            .prop_map(|(id, code, reason)| EnvelopeObject::Error(ErrorEnvelope {
                id,
                code,
                reason
            })),
        any_hello().prop_map(EnvelopeObject::Hello),
        (proptest::option::of(any::<String>()), any::<String>())
            .prop_map(|(id, message)| EnvelopeObject::Knock(Knock { id, message })),
        (
            any::<String>(),
            proptest::option::of(any::<String>()),
            proptest::option::of(any::<String>()),
        )
            .prop_map(|(at, status, endpoint)| {
                EnvelopeObject::Presence(Presence {
                    at,
                    status,
                    endpoint,
                })
            }),
        any_help().prop_map(EnvelopeObject::Help),
        (any::<String>(), any::<String>())
            .prop_map(|(id, content_type)| { EnvelopeObject::Media(Media { id, content_type }) }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn every_variant_round_trips(obj in any_envelope()) {
        let got = round_trip(&obj);
        prop_assert_eq!(got, obj);
    }
}

// --- one explicit round-trip per envelope class ---------------------------

#[test]
fn all_classes_round_trip() {
    let payload = vec![
        Triple::new(
            NamedNode::new("urn:note:42").unwrap(),
            NamedNode::new("http://example.org/vocab#title").unwrap(),
            Literal::new_simple_literal("groceries"),
        ),
        Triple::new(
            NamedNode::new("urn:note:42").unwrap(),
            NamedNode::new("http://www.w3.org/1999/02/22-rdf-syntax-ns#type").unwrap(),
            NamedNode::new("http://example.org/vocab#Note").unwrap(),
        ),
    ];
    let st = Statement {
        id: "01J9V3Z8K7Q2M4X6P8R0T2V4W6".into(),
        modulation: "sql-sqlite".into(),
        signal: "SELECT 1".into(),
        params: vec![Value::Integer(1)],
        database: Some("main".into()),
        row_limit: Some(10),
        byte_limit: Some(1024),
        timeout_ms: Some(500),
    };
    let objs = vec![
        EnvelopeObject::Query(st.clone()),
        EnvelopeObject::Execute(st),
        EnvelopeObject::Result(ResultHeader {
            id: "x".into(),
            columns: vec!["a".into()],
            decl_types: vec!["INTEGER".into()],
        }),
        EnvelopeObject::Row(vec![Row {
            seq: 0,
            cells: vec![("a".into(), Value::Integer(1))],
        }]),
        EnvelopeObject::Done(Done {
            id: "x".into(),
            row_count: Some(1),
            affected_rows: None,
            last_insert_rowid: None,
            truncated: false,
        }),
        EnvelopeObject::Denied(Denied {
            id: Some("x".into()),
            reason: Some("no".into()),
        }),
        EnvelopeObject::Error(ErrorEnvelope {
            id: None,
            code: "engine-error".into(),
            reason: Some("boom".into()),
        }),
        EnvelopeObject::Hello(Hello {
            envelope_version: "0.1".into(),
            encodings: vec!["turtle".into()],
            mods: vec!["help".into(), "sparql".into()],
            hint: Some("hi".into()),
        }),
        EnvelopeObject::Knock(Knock {
            id: None,
            message: "hello".into(),
        }),
        EnvelopeObject::Presence(Presence {
            at: "2026-07-23T09:15:00".into(),
            status: None,
            endpoint: None,
        }),
        EnvelopeObject::Decision(Decision {
            id: Some("x".into()),
            decision: "allow".into(),
            decided_by: "policy".into(),
            reason: None,
            at: Some("2026-07-23T09:20:00".into()),
        }),
        EnvelopeObject::Help(Help {
            id: None,
            signal: "line one\nline \"two\"".into(),
            topics: vec!["a".into()],
        }),
        EnvelopeObject::Media(Media {
            id: "x".into(),
            content_type: "video/mp2t".into(),
        }),
        EnvelopeObject::AudioDuplex(AudioDuplex {
            id: "x".into(),
            content_type: Some("audio/L16;rate=8000;channels=1".into()),
            accepts: "audio/L16;rate=8000;channels=1".into(),
        }),
        EnvelopeObject::AudioDuplex(AudioDuplex {
            id: "x".into(),
            // A pure talk sink emits nothing downstream.
            content_type: None,
            accepts: "audio/L16;rate=8000;channels=1".into(),
        }),
        EnvelopeObject::Projection(Projection {
            id: Some("x".into()),
            offers: vec![Point {
                iri: "urn:x:hi".into(),
                kind: PointKind::Excitable,
                label: Some("say hi".into()),
                comment: Some("a greeting".into()),
                icon: Some("wave".into()),
                role: Some("default".into()),
                projects: Some("urn:x:deeper".into()),
                coupling: vec![PointField {
                    name: "mood".into(),
                    datatype: Some("http://www.w3.org/2001/XMLSchema#string".into()),
                    required: true,
                    default: Some(Value::Text("calm".into())),
                    one_of: vec![Value::Text("calm".into()), Value::Integer(3)],
                    hint: Some("how do you feel".into()),
                }],
                modulation: Some("sql-sqlite".into()),
                signal: Some("INSERT INTO moods VALUES (?)".into()),
                params_order: vec!["mood".into()],
                signal_template: Some("SAY {mood}".into()),
                fires: Some("urn:msg:hi:{mood}".into()),
            }],
        }),
        EnvelopeObject::Entrain(Entrain {
            id: "x".into(),
            point: "urn:notes:changed".into(),
        }),
        EnvelopeObject::Vibration(Vibration {
            id: "x".into(),
            point: "urn:notes:changed".into(),
            seq: 7,
            at: Some("2026-07-26T10:00:00".into()),
            payload: payload.clone(),
        }),
        EnvelopeObject::Damp(Damp {
            id: None,
            point: "urn:notes:changed".into(),
        }),
        EnvelopeObject::Graph(Graph {
            id: "x".into(),
            seq: 0,
            payload,
        }),
        EnvelopeObject::Generic(Generic {
            class: "Refused".into(),
            props: vec![
                (
                    NamedNode::new("http://resonator.network/v3/rsntr#code").unwrap(),
                    Term::Literal(Literal::new_simple_literal("envelope-version")),
                ),
                (
                    NamedNode::new("http://example.org/vocab#extra").unwrap(),
                    Term::NamedNode(NamedNode::new("urn:x:thing").unwrap()),
                ),
            ],
        }),
    ];
    for obj in objs {
        assert_round_trips(obj);
    }
}

// --- explicit edge cases --------------------------------------------------

#[test]
fn edge_params_round_trip() {
    let obj = EnvelopeObject::Query(Statement {
        id: "01J9V3Z8K7Q2M4X6P8R0T2V4W6".into(),
        modulation: "sql-sqlite".into(),
        signal: "SELECT 'it''s \"quoted\"',\n  x\nFROM t -- newline in SQL\n".into(),
        params: vec![
            Value::Null,
            Value::Integer(i64::MIN),
            Value::Integer(i64::MAX),
            Value::Real(1e308),
            Value::Real(-0.0),
            Value::Real(0.1),
            Value::Text("non-ASCII: \u{65e5}\u{672c}\u{8a9e} \u{1f422} caf\u{e9}".into()),
            Value::Text("quotes \" and \\ backslash and \r\n newline".into()),
            Value::Blob(vec![0, 1, 2, 253, 254, 255]),
            Value::BlobRef {
                hash: "blake3:aa00ff".into(),
                bytes: 104_857_600,
            },
        ],
        database: None,
        row_limit: Some(i64::MAX),
        byte_limit: Some(0),
        timeout_ms: Some(1),
    });
    let got = round_trip(&obj);
    assert_eq!(got, obj);

    // -0.0 must survive as a negative zero, checked at the bit level.
    let EnvelopeObject::Query(st) = got else {
        panic!("not a query");
    };
    let Value::Real(z) = st.params[4] else {
        panic!("param 4 not real");
    };
    assert_eq!(z.to_bits(), (-0.0f64).to_bits());
}

#[test]
fn nan_round_trips_to_a_nan() {
    let obj = EnvelopeObject::Row(vec![Row {
        seq: 0,
        cells: vec![("x".into(), Value::Real(f64::NAN))],
    }]);
    let got = round_trip(&obj);
    let EnvelopeObject::Row(rows) = got else {
        panic!("not rows");
    };
    let Value::Real(f) = rows[0].cells[0].1 else {
        panic!("not real");
    };
    assert!(f.is_nan());
}

#[test]
fn infinities_round_trip() {
    for f in [f64::INFINITY, f64::NEG_INFINITY] {
        assert_round_trips(EnvelopeObject::Row(vec![Row {
            seq: 0,
            cells: vec![("x".into(), Value::Real(f))],
        }]));
    }
}

#[test]
fn weird_column_names_round_trip() {
    assert_round_trips(EnvelopeObject::Row(vec![Row {
        seq: 42,
        cells: vec![
            ("col with spaces".into(), Value::Integer(1)),
            ("percent%sign".into(), Value::Integer(2)),
            ("\u{65e5}\u{672c}\u{8a9e}".into(), Value::Integer(3)),
            ("trailing.dot.".into(), Value::Integer(4)),
            ("".into(), Value::Integer(5)),
        ],
    }]));
}

#[test]
fn help_multiline_uses_a_triple_quoted_literal() {
    let obj = EnvelopeObject::Help(Help {
        id: None,
        signal: "line one\nline \"two\" with a quote\nline three".into(),
        topics: vec![],
    });
    let doc = obj.to_turtle().expect("serialize");
    assert!(doc.contains("\"\"\""), "expected a long string:\n{doc}");
    assert!(
        doc.contains("line one\nline"),
        "expected literal newlines in the long string:\n{doc}"
    );
    assert_round_trips(obj);
}

#[test]
fn null_cells_are_omitted_on_the_wire() {
    let obj = EnvelopeObject::Row(vec![Row {
        seq: 0,
        cells: vec![("a".into(), Value::Integer(1)), ("b".into(), Value::Null)],
    }]);
    // The NULL cell disappears: absence of the column predicate is the
    // designed encoding for row NULLs.
    assert_eq!(
        round_trip(&obj),
        EnvelopeObject::Row(vec![Row {
            seq: 0,
            cells: vec![("a".into(), Value::Integer(1))],
        }])
    );
}

#[test]
fn graph_payload_is_exposed_as_triples() {
    let payload = vec![
        Triple::new(
            NamedNode::new("urn:person:alice").unwrap(),
            NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap(),
            Literal::new_simple_literal("Alice"),
        ),
        Triple::new(
            NamedNode::new("urn:person:alice").unwrap(),
            NamedNode::new("http://xmlns.com/foaf/0.1/knows").unwrap(),
            NamedNode::new("urn:person:bob").unwrap(),
        ),
    ];
    let obj = EnvelopeObject::Graph(Graph {
        id: "01K12M8Z4T9Q6W2E8R4T0Y6X3A".into(),
        seq: 3,
        payload: payload.clone(),
    });
    let got = round_trip(&obj);
    let EnvelopeObject::Graph(gr) = got else {
        panic!("not a graph frame");
    };
    assert_eq!(gr.id, "01K12M8Z4T9Q6W2E8R4T0Y6X3A");
    assert_eq!(gr.seq, 3);
    assert_eq!(gr.payload, payload);
}

#[test]
fn graph_frame_parses_from_hand_written_turtle() {
    let doc = r#"[] a rsntr:Graph ;
   rsntr:id "01K12M8Z4T9Q6W2E8R4T0Y6X3A" ;
   rsntr:seq 0 .
<urn:person:alice> <http://xmlns.com/foaf/0.1/name> "Alice" .
"#;
    let got = EnvelopeObject::from_turtle(doc).expect("parse");
    let EnvelopeObject::Graph(gr) = got else {
        panic!("not a graph frame");
    };
    assert_eq!(gr.seq, 0);
    assert_eq!(gr.payload.len(), 1);
    assert_eq!(
        gr.payload[0].predicate.as_str(),
        "http://xmlns.com/foaf/0.1/name"
    );
}
