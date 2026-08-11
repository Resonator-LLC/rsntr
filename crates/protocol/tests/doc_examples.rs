//! The worked Turtle examples from the POC protocol docs
//! (rdf-envelope-protocol.md, connection-protocol.md,
//! projection-protocol.md), stored as fixtures under `tests/examples/` and
//! re-namespaced to v3 (the namespace lives in the implied prefix block,
//! which is never in the frame text, so the fixture bytes are unchanged
//! from the docs). Each fixture must parse through the real wire path into
//! the expected typed object and re-serialize equivalently
//! (graph-isomorphic).

use bytes::BytesMut;
use oxrdf::dataset::CanonicalizationAlgorithm;
use oxrdf::{Dataset, GraphName};
use oxttl::TurtleParser;
use resonator_protocol::vocab::IMPLIED_PREFIXES;
use resonator_protocol::{
    Decision, Done, Entrain, EnvelopeObject, Generic, Hello, Help, Knock, Point, PointField,
    PointKind, Presence, Projection, ResultHeader, Row, Statement, Value, Vibration,
    decode_envelope, encode_frame,
};

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const RSNTR_NS: &str = "http://resonator.network/v3/rsntr#";

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/examples/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Parses a fixture through the real wire path: frame it, decode it.
fn parse(doc: &str) -> EnvelopeObject {
    let mut buf = BytesMut::new();
    encode_frame(doc.as_bytes(), &mut buf).expect("frame");
    decode_envelope(&mut buf)
        .expect("decode")
        .expect("complete frame")
}

/// Parses a Turtle document (implied prefixes registered) into a
/// canonicalized dataset for isomorphism comparison. `rsntr:required false`
/// triples are dropped first: false is the wire default, so stating it and
/// omitting it are the same graph by design and the serializer omits it.
fn canonical_dataset(doc: &str) -> Dataset {
    let mut parser = TurtleParser::new();
    for (name, iri) in IMPLIED_PREFIXES {
        parser = parser.with_prefix(name, iri).expect("static prefix");
    }
    let mut ds = Dataset::new();
    for t in parser.for_slice(doc.as_bytes()) {
        let t = t.expect("fixture parses as Turtle");
        if t.predicate.as_str() == format!("{RSNTR_NS}required")
            && matches!(&t.object, oxrdf::Term::Literal(l) if l.value() == "false")
        {
            continue;
        }
        ds.insert(&t.in_graph(GraphName::DefaultGraph));
    }
    ds.canonicalize(CanonicalizationAlgorithm::Unstable);
    ds
}

/// The full doc-example check: parse to the expected object, then
/// re-serialize and require (a) typed-object round-trip equality and
/// (b) graph isomorphism between the fixture and the re-serialized form.
fn check(name: &str, expected: &EnvelopeObject) {
    let doc = fixture(name);
    let got = parse(&doc);
    assert_eq!(&got, expected, "{name}: parse mismatch");
    let rewritten = got.to_turtle().expect("serialize");
    let back = EnvelopeObject::from_turtle(&rewritten).expect("reparse");
    assert_eq!(&back, expected, "{name}: round-trip mismatch:\n{rewritten}");
    assert_eq!(
        canonical_dataset(&doc),
        canonical_dataset(&rewritten),
        "{name}: re-serialized form is not graph-isomorphic:\n{rewritten}"
    );
}

fn statement(id: &str, modulation: &str, signal: &str) -> Statement {
    Statement {
        id: id.into(),
        modulation: modulation.into(),
        signal: signal.into(),
        params: vec![],
        database: None,
        row_limit: None,
        byte_limit: None,
        timeout_ms: None,
    }
}

// --- rdf-envelope-protocol.md ---------------------------------------------

#[test]
fn envelope_help_query() {
    check(
        "envelope-4.1-help-query.ttl",
        &EnvelopeObject::Query(statement("01J9V8...", "help", "")),
    );
}

#[test]
fn envelope_help_response() {
    let text = "This is a resonator node (rsntr, envelope 0.1).\n\
You talk to me in RDF objects over QUIC; I serve the modulations: sql-sqlite, help.\n\
Publicly readable now: notes(title, body, mtime).\n\
To run a read:\n  \
[] a rsntr:Query ; rsntr:mod \"sql-sqlite\" ; rsntr:signal \"SELECT title FROM notes\" .\n\
Not admitted yet? Introduce yourself and I may let you in:\n  \
[] a rsntr:Knock ; rsntr:message \"who you are and what you want\" .\n\
Ask for more: send a help query with signal one of: modulations, tables, knock, examples.";
    check(
        "envelope-4.1-help-response.ttl",
        &EnvelopeObject::Help(Help {
            id: Some("01J9V8...".into()),
            signal: text.into(),
            topics: vec![
                "modulations".into(),
                "tables".into(),
                "knock".into(),
                "examples".into(),
            ],
        }),
    );
}

#[test]
fn envelope_query() {
    check(
        "envelope-5-query.ttl",
        &EnvelopeObject::Query(Statement {
            params: vec![Value::Text("2026-07-01".into())],
            row_limit: Some(1000),
            ..statement(
                "01J9V3Z8K7Q2M4X6P8R0T2V4W6",
                "sql-sqlite",
                "SELECT name, mtime FROM notes WHERE mtime > ?",
            )
        }),
    );
}

#[test]
fn envelope_execute() {
    check(
        "envelope-5-execute.ttl",
        &EnvelopeObject::Execute(Statement {
            params: vec![
                Value::Text("hello from bob".into()),
                Value::Text("...".into()),
            ],
            ..statement(
                "01J9V3ZS3FQZJ8B1N5D7F9H1K3",
                "sql-sqlite",
                "INSERT INTO notes (title, body) VALUES (?, ?)",
            )
        }),
    );
}

#[test]
fn envelope_result_header() {
    check(
        "envelope-6-result-header.ttl",
        &EnvelopeObject::Result(ResultHeader {
            id: "01J9V3Z8K7Q2M4X6P8R0T2V4W6".into(),
            columns: vec!["name".into(), "mtime".into()],
            decl_types: vec!["TEXT".into(), "TEXT".into()],
        }),
    );
}

#[test]
fn envelope_row_batch() {
    check(
        "envelope-6-row-batch.ttl",
        &EnvelopeObject::Row(vec![
            Row {
                seq: 0,
                cells: vec![
                    ("name".into(), Value::Text("groceries".into())),
                    ("mtime".into(), Value::Text("2026-07-04T10:11:12".into())),
                ],
            },
            Row {
                seq: 1,
                cells: vec![
                    ("name".into(), Value::Text("reading list".into())),
                    ("mtime".into(), Value::Text("2026-07-19T08:00:00".into())),
                ],
            },
        ]),
    );
}

#[test]
fn envelope_done() {
    check(
        "envelope-6-done.ttl",
        &EnvelopeObject::Done(Done {
            id: "01J9V3Z8K7Q2M4X6P8R0T2V4W6".into(),
            row_count: Some(2),
            affected_rows: None,
            last_insert_rowid: None,
            truncated: false,
        }),
    );
}

#[test]
fn envelope_hello() {
    check(
        "envelope-8-hello.ttl",
        &EnvelopeObject::Hello(Hello {
            envelope_version: "0.1".into(),
            encodings: vec!["turtle".into(), "compact-postcard".into()],
            mods: vec!["sql-sqlite-3.46".into(), "help".into()],
            hint: None,
        }),
    );
}

#[test]
fn envelope_knock() {
    check(
        "envelope-9-knock.ttl",
        &EnvelopeObject::Knock(Knock {
            id: Some("01J9V6QK3M8ZT0R4Y2W6B8N1D5".into()),
            message: "Hi, this is carol, met you at the workshop. Requesting recipe access.".into(),
        }),
    );
}

#[test]
fn envelope_presence() {
    check(
        "envelope-9-presence.ttl",
        &EnvelopeObject::Presence(Presence {
            at: "2026-07-23T09:15:00".into(),
            status: Some("around".into()),
            endpoint: Some(
                "e00c36119fb26262a12a5571f918966caf0b4cefe3c72ed046a1c1bbf1cd63d9".into(),
            ),
        }),
    );
}

#[test]
fn envelope_decision() {
    check(
        "envelope-9-decision.ttl",
        &EnvelopeObject::Decision(Decision {
            id: Some("01J9V3Z8K7Q2M4X6P8R0T2V4W6".into()),
            decision: "deny".into(),
            decided_by: "script".into(),
            reason: Some("bulk read exceeds hourly row budget for this peer".into()),
            at: None,
        }),
    );
}

// --- connection-protocol.md -----------------------------------------------

#[test]
fn connection_hellos() {
    let hint = "resonator node; send an rsntr:Query with modulation 'help' for usage, or type HELP";
    check(
        "connection-2-hello-alice.ttl",
        &EnvelopeObject::Hello(Hello {
            envelope_version: "0.1".into(),
            encodings: vec!["turtle".into()],
            mods: vec!["sql-sqlite-3.46".into(), "help".into()],
            hint: Some(hint.into()),
        }),
    );
    check(
        "connection-2-hello-bob.ttl",
        &EnvelopeObject::Hello(Hello {
            envelope_version: "0.1".into(),
            encodings: vec!["turtle".into(), "compact-postcard".into()],
            mods: vec!["sql-sqlite-3.46".into(), "help".into()],
            hint: Some(hint.into()),
        }),
    );
}

#[test]
fn connection_presence() {
    check(
        "connection-3-presence.ttl",
        &EnvelopeObject::Presence(Presence {
            at: "2026-07-23T09:15:00".into(),
            status: Some("around".into()),
            endpoint: None,
        }),
    );
}

#[test]
fn connection_knock() {
    check(
        "connection-4-knock.ttl",
        &EnvelopeObject::Knock(Knock {
            id: Some("01J9V6QK3M8ZT0R4Y2W6B8N1D5".into()),
            message:
                "Hi, this is alice, we met at the workshop. Requesting read access to recipes."
                    .into(),
        }),
    );
}

#[test]
fn connection_decision() {
    check(
        "connection-4.3-decision.ttl",
        &EnvelopeObject::Decision(Decision {
            id: Some("01J9V6QK3M8ZT0R4Y2W6B8N1D5".into()),
            decision: "allow".into(),
            decided_by: "human".into(),
            reason: Some("welcome, recipes are readable".into()),
            at: Some("2026-07-23T09:20:00".into()),
        }),
    );
}

/// `rsntr:Refused` is defined by the connection doc but is not a modeled
/// class in this codec: it must decode as the v3 Generic passthrough.
#[test]
fn connection_refused_is_generic() {
    use oxrdf::{Literal, NamedNode, Term};
    check(
        "connection-5-refused.ttl",
        &EnvelopeObject::Generic(Generic {
            class: "Refused".into(),
            props: vec![
                (
                    NamedNode::new(format!("{RSNTR_NS}code")).unwrap(),
                    Term::Literal(Literal::new_simple_literal("envelope-version")),
                ),
                (
                    NamedNode::new(format!("{RSNTR_NS}reason")).unwrap(),
                    Term::Literal(Literal::new_simple_literal(
                        "this node speaks envelope 0.x only",
                    )),
                ),
            ],
        }),
    );
}

// --- projection-protocol.md -----------------------------------------------

fn bare_point(iri: &str, kind: PointKind) -> Point {
    Point {
        iri: iri.into(),
        kind,
        label: None,
        comment: None,
        icon: None,
        role: None,
        projects: None,
        coupling: Vec::new(),
        modulation: None,
        signal: None,
        params_order: Vec::new(),
        signal_template: None,
        fires: None,
    }
}

#[test]
fn projection_query() {
    check(
        "projection-3-query.ttl",
        &EnvelopeObject::Query(statement("01K12M8Z4T9Q6W2E8R4T0Y6X3A", "projection", "")),
    );
}

#[test]
fn projection_response() {
    check(
        "projection-3-response.ttl",
        &EnvelopeObject::Projection(Projection {
            id: Some("01K12M8Z4T9Q6W2E8R4T0Y6X3A".into()),
            offers: vec![
                Point {
                    label: Some("browse notes".into()),
                    modulation: Some("sql-sqlite".into()),
                    signal: Some("SELECT title, mtime FROM notes ORDER BY mtime DESC".into()),
                    ..bare_point("urn:notes:browse", PointKind::Radiant)
                },
                Point {
                    label: Some("add a note".into()),
                    icon: Some("plus".into()),
                    coupling: vec![
                        PointField {
                            name: "title".into(),
                            datatype: Some(XSD_STRING.into()),
                            required: true,
                            default: None,
                            one_of: Vec::new(),
                            hint: Some("short title for the note".into()),
                        },
                        PointField {
                            name: "body".into(),
                            datatype: Some(XSD_STRING.into()),
                            required: false,
                            default: None,
                            one_of: Vec::new(),
                            hint: None,
                        },
                    ],
                    modulation: Some("sql-sqlite".into()),
                    signal: Some("INSERT INTO notes (title, body) VALUES (?, ?)".into()),
                    params_order: vec!["title".into(), "body".into()],
                    ..bare_point("urn:notes:add", PointKind::Excitable)
                },
                Point {
                    label: Some("a note was added".into()),
                    ..bare_point("urn:notes:changed", PointKind::Sympathetic)
                },
                Point {
                    label: Some("admin".into()),
                    projects: Some("urn:notes:admin".into()),
                    ..bare_point("urn:notes:admin", PointKind::Bare)
                },
            ],
        }),
    );
}

#[test]
fn projection_invoke() {
    check(
        "projection-4-invoke.ttl",
        &EnvelopeObject::Execute(Statement {
            params: vec![Value::Text("groceries".into()), Value::Null],
            ..statement(
                "01K12M9G7H2J5K8M1N4P7R0S3B",
                "sql-sqlite",
                "INSERT INTO notes (title, body) VALUES (?, ?)",
            )
        }),
    );
}

#[test]
fn projection_entrain() {
    check(
        "projection-5-entrain.ttl",
        &EnvelopeObject::Entrain(Entrain {
            id: "01K12MA53C6D9F2G5H8J1K4M7C".into(),
            point: "urn:notes:changed".into(),
        }),
    );
}

#[test]
fn projection_vibration() {
    check(
        "projection-5-vibration.ttl",
        &EnvelopeObject::Vibration(Vibration {
            id: "01K12MA53C6D9F2G5H8J1K4M7C".into(),
            point: "urn:notes:changed".into(),
            seq: 0,
            at: Some("2026-07-26T10:00:00".into()),
            payload: vec![],
        }),
    );
}

/// Every fixture in tests/examples/ is covered by an explicit test above.
#[test]
fn all_fixtures_are_covered() {
    let dir = format!("{}/tests/examples", env!("CARGO_MANIFEST_DIR"));
    let known = [
        "envelope-4.1-help-query.ttl",
        "envelope-4.1-help-response.ttl",
        "envelope-5-query.ttl",
        "envelope-5-execute.ttl",
        "envelope-6-result-header.ttl",
        "envelope-6-row-batch.ttl",
        "envelope-6-done.ttl",
        "envelope-8-hello.ttl",
        "envelope-9-knock.ttl",
        "envelope-9-presence.ttl",
        "envelope-9-decision.ttl",
        "connection-2-hello-alice.ttl",
        "connection-2-hello-bob.ttl",
        "connection-3-presence.ttl",
        "connection-4-knock.ttl",
        "connection-4.3-decision.ttl",
        "connection-5-refused.ttl",
        "projection-3-query.ttl",
        "projection-3-response.ttl",
        "projection-4-invoke.ttl",
        "projection-5-entrain.ttl",
        "projection-5-vibration.ttl",
    ];
    for entry in std::fs::read_dir(&dir).expect("examples dir") {
        let name = entry.expect("entry").file_name();
        let name = name.to_string_lossy();
        assert!(
            known.contains(&name.as_ref()),
            "fixture {name} has no covering test"
        );
    }
}
