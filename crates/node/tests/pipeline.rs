//! Pipeline tests against an in-memory database, driven through a local
//! RequestStream double (no network).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use resonator_authenticator::Chain;
use resonator_node::{DbHandle, Node, NodeConfig, open_node_db_in_memory, seed_rsntr_defaults};
use resonator_protocol::{EnvelopeObject, Hello, Knock, Request, RequestKind, Value};
use resonator_transport::{IncomingRequest, PeerId, RequestStream, TransportError, basic_hello};

// ---------------------------------------------------------------------------
// Local stream double
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Recorded {
    sent: Arc<Mutex<Vec<EnvelopeObject>>>,
    raw: Arc<Mutex<Vec<u8>>>,
}

impl Recorded {
    fn frames(&self) -> Vec<EnvelopeObject> {
        self.sent.lock().unwrap().clone()
    }
    fn raw_bytes(&self) -> Vec<u8> {
        self.raw.lock().unwrap().clone()
    }
}

struct LocalStream {
    out: Recorded,
    incoming: VecDeque<EnvelopeObject>,
}

impl RequestStream for LocalStream {
    async fn send(&mut self, obj: &EnvelopeObject) -> Result<(), TransportError> {
        self.out.sent.lock().unwrap().push(obj.clone());
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<EnvelopeObject>, TransportError> {
        Ok(self.incoming.pop_front())
    }

    async fn send_raw(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.out.raw.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    }

    async fn finish(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

const PEER: [u8; 32] = [1u8; 32];
const STRANGER: [u8; 32] = [2u8; 32];

fn peer_hex() -> String {
    PeerId(PEER).to_string()
}

fn hello() -> Hello {
    basic_hello(&["help"], None)
}

async fn test_node(config: NodeConfig) -> Arc<Node> {
    let conn = open_node_db_in_memory().expect("open db");
    seed_rsntr_defaults(&conn).expect("seed");
    conn.execute_batch(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT, secret TEXT);
         INSERT INTO notes (body, secret) VALUES ('one', 's1'), ('two', 's2'), ('three', 's3');",
    )
    .expect("fixture");
    Arc::new(Node::new(
        DbHandle::spawn(conn),
        Chain::with_builtin_tiers(),
        config,
    ))
}

async fn admit(node: &Node, peer: [u8; 32]) {
    let hex = PeerId(peer).to_string();
    node.db()
        .call(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO _peers (endpoint_id, added_at) VALUES (?1, 'now')",
                [hex],
            )
            .unwrap();
        })
        .await
        .unwrap();
}

async fn allow(node: &Node, peer: &str, table: &str, action: &str) {
    let (peer, table, action) = (peer.to_string(), table.to_string(), action.to_string());
    node.db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
                 VALUES (?1, ?2, ?3, 'allow')",
                (peer, table, action),
            )
            .unwrap();
        })
        .await
        .unwrap();
}

/// Drives one request through the node and returns the response frames.
async fn drive(
    node: &Node,
    peer: [u8; 32],
    first: EnvelopeObject,
    incoming: Vec<EnvelopeObject>,
) -> (Recorded, Vec<u8>) {
    let out = Recorded::default();
    let stream = LocalStream {
        out: out.clone(),
        incoming: incoming.into(),
    };
    node.handle(IncomingRequest {
        peer: PeerId(peer),
        peer_hello: hello(),
        first,
        stream,
    })
    .await
    .ok();
    let raw = out.raw_bytes();
    (out, raw)
}

fn query(modulation: &str, signal: &str) -> (EnvelopeObject, String) {
    let req = Request::new(RequestKind::Query, modulation, signal);
    (req.to_envelope(), req.id_string())
}

fn execute(modulation: &str, signal: &str) -> (EnvelopeObject, String) {
    let req = Request::new(RequestKind::Execute, modulation, signal);
    (req.to_envelope(), req.id_string())
}

async fn audit_rows(node: &Node) -> Vec<(String, String, String)> {
    node.db()
        .call(|conn| {
            let mut st = conn
                .prepare("SELECT decision, decided_by, signal FROM _audit ORDER BY id")
                .unwrap();
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        })
        .await
        .unwrap()
}

// ---------------------------------------------------------------------------
// SQL pipeline paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stranger_query_is_denied_and_audited() {
    let node = test_node(NodeConfig::default()).await;
    let (q, _) = query("sql-sqlite", "SELECT 1");
    let (out, _) = drive(&node, STRANGER, q, vec![]).await;
    let frames = out.frames();
    assert_eq!(frames.len(), 1);
    match &frames[0] {
        EnvelopeObject::Denied(d) => {
            assert!(d.reason.as_deref().unwrap().contains("unknown peer"));
        }
        other => panic!("expected Denied, got {other:?}"),
    }
    let audits = audit_rows(&node).await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].0, "deny");
    assert_eq!(audits[0].1, "peer-gate");
}

#[tokio::test]
async fn allowed_read_streams_result_rows_done() {
    let node = test_node(NodeConfig::default()).await;
    admit(&node, PEER).await;
    allow(&node, &peer_hex(), "notes", "read").await;

    let (q, id) = query("sql-sqlite", "SELECT id, body FROM notes ORDER BY id");
    let (out, _) = drive(&node, PEER, q, vec![]).await;
    let frames = out.frames();
    assert_eq!(frames.len(), 3, "frames: {frames:?}");
    match &frames[0] {
        EnvelopeObject::Result(h) => {
            assert_eq!(h.id, id);
            assert_eq!(h.columns, vec!["id", "body"]);
        }
        other => panic!("expected Result, got {other:?}"),
    }
    match &frames[1] {
        EnvelopeObject::Row(rows) => {
            assert_eq!(rows.len(), 3);
            assert_eq!(
                rows[0].cells[1],
                ("body".to_string(), Value::Text("one".into()))
            );
        }
        other => panic!("expected Row, got {other:?}"),
    }
    match &frames[2] {
        EnvelopeObject::Done(d) => {
            assert_eq!(d.row_count, Some(3));
            assert!(!d.truncated);
        }
        other => panic!("expected Done, got {other:?}"),
    }

    // The audit row recorded the allow and the outcome.
    let (decision, rows_out): (String, i64) = node
        .db()
        .call(|conn| {
            conn.query_row(
                "SELECT decision, rows_out FROM _audit ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        })
        .await
        .unwrap();
    assert_eq!(decision, "allow");
    assert_eq!(rows_out, 3);
}

#[tokio::test]
async fn policy_gap_falls_to_tail_deny() {
    let node = test_node(NodeConfig::default()).await;
    admit(&node, PEER).await;
    // No policy rows at all: the policy tier escalates, the tail denies.
    let (q, _) = query("sql-sqlite", "SELECT id FROM notes");
    let (out, _) = drive(&node, PEER, q, vec![]).await;
    match &out.frames()[0] {
        EnvelopeObject::Denied(d) => {
            assert!(d.reason.as_deref().unwrap().contains("no decider"));
        }
        other => panic!("expected Denied, got {other:?}"),
    }
    let audits = audit_rows(&node).await;
    assert_eq!(audits.last().unwrap().1, "default");
}

#[tokio::test]
async fn banned_statements_are_denied_at_prepare() {
    let node = test_node(NodeConfig::default()).await;
    admit(&node, PEER).await;
    allow(&node, &peer_hex(), "*", "read").await;
    for (sql, needle) in [
        ("ATTACH DATABASE ':memory:' AS x", "ATTACH"),
        ("PRAGMA journal_mode = DELETE", "not allowlisted"),
        ("DROP TABLE notes", "DDL"),
        ("BEGIN", "transaction control"),
    ] {
        let (q, _) = query("sql-sqlite", sql);
        let (out, _) = drive(&node, PEER, q, vec![]).await;
        match &out.frames()[0] {
            EnvelopeObject::Denied(d) => {
                assert!(
                    d.reason.as_deref().unwrap().contains(needle),
                    "{sql}: {:?}",
                    d.reason
                );
            }
            other => panic!("{sql}: expected Denied, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn row_limit_truncates() {
    let config = NodeConfig {
        max_rows: 2,
        ..NodeConfig::default()
    };
    let node = test_node(config).await;
    admit(&node, PEER).await;
    allow(&node, &peer_hex(), "notes", "read").await;

    let (q, _) = query("sql-sqlite", "SELECT id FROM notes ORDER BY id");
    let (out, _) = drive(&node, PEER, q, vec![]).await;
    let frames = out.frames();
    match frames.last().unwrap() {
        EnvelopeObject::Done(d) => {
            assert_eq!(d.row_count, Some(2));
            assert!(d.truncated);
        }
        other => panic!("expected Done, got {other:?}"),
    }
}

#[tokio::test]
async fn step_budget_answers_limit_exceeded() {
    let config = NodeConfig {
        vdbe_step_budget: 1_000,
        ..NodeConfig::default()
    };
    let node = test_node(config).await;
    admit(&node, PEER).await;
    allow(&node, &peer_hex(), "*", "read").await;

    let (q, _) = query(
        "sql-sqlite",
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 1000000) \
         SELECT count(*) FROM c",
    );
    let (out, _) = drive(&node, PEER, q, vec![]).await;
    let frames = out.frames();
    match frames.last().unwrap() {
        EnvelopeObject::Error(e) => {
            assert_eq!(e.code, "limit-exceeded");
            assert!(e.reason.as_deref().unwrap().contains("step budget"));
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn write_is_idempotent_via_applied() {
    let node = test_node(NodeConfig::default()).await;
    admit(&node, PEER).await;
    allow(&node, &peer_hex(), "notes", "write").await;

    let (e1, _) = execute("sql-sqlite", "INSERT INTO notes (body) VALUES ('new')");
    let e2 = e1.clone(); // same request id: a retransmit
    let (out1, _) = drive(&node, PEER, e1, vec![]).await;
    match out1.frames().last().unwrap() {
        EnvelopeObject::Done(d) => assert_eq!(d.affected_rows, Some(1)),
        other => panic!("expected Done, got {other:?}"),
    }
    let (out2, _) = drive(&node, PEER, e2, vec![]).await;
    match out2.frames().last().unwrap() {
        EnvelopeObject::Done(d) => assert_eq!(d.affected_rows, Some(1)),
        other => panic!("expected Done, got {other:?}"),
    }
    let n: i64 = node
        .db()
        .call(|conn| {
            conn.query_row("SELECT count(*) FROM notes WHERE body = 'new'", [], |r| {
                r.get(0)
            })
            .unwrap()
        })
        .await
        .unwrap();
    assert_eq!(n, 1, "the retransmit must not apply twice");
}

#[tokio::test]
async fn unknown_modulation_fast_fails() {
    let node = test_node(NodeConfig::default()).await;
    admit(&node, PEER).await;
    let (q, _) = query("no-such-mod", "whatever");
    let (out, _) = drive(&node, PEER, q, vec![]).await;
    match &out.frames()[0] {
        EnvelopeObject::Error(e) => assert_eq!(e.code, "mod-unsupported"),
        other => panic!("expected Error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Help / knock / projection / entrain / media
// ---------------------------------------------------------------------------

#[tokio::test]
async fn help_served_to_strangers_and_never_overstates() {
    let node = test_node(NodeConfig::default()).await;
    let (q, _) = query("help", "");
    let (out, _) = drive(&node, STRANGER, q, vec![]).await;
    match &out.frames()[0] {
        EnvelopeObject::Help(h) => {
            assert!(h.signal.contains("nothing yet"), "help: {}", h.signal);
            assert!(h.topics.iter().any(|t| t == "knock"));
        }
        other => panic!("expected Help, got {other:?}"),
    }

    // With a policy allow the fact line changes.
    allow(&node, &PeerId(STRANGER).to_string(), "notes", "read").await;
    let (q, _) = query("help", "tables");
    let (out, _) = drive(&node, STRANGER, q, vec![]).await;
    match &out.frames()[0] {
        EnvelopeObject::Help(h) => assert!(h.signal.contains("Readable now: notes.")),
        other => panic!("expected Help, got {other:?}"),
    }
}

#[tokio::test]
async fn knock_parks_pending_and_rate_limits() {
    let node = test_node(NodeConfig::default()).await;
    let knock = EnvelopeObject::Knock(Knock {
        id: None,
        message: "hi, I am a test".into(),
    });
    let (out, _) = drive(&node, STRANGER, knock.clone(), vec![]).await;
    match &out.frames()[0] {
        EnvelopeObject::Decision(d) => assert_eq!(d.decision, "pending"),
        other => panic!("expected Decision, got {other:?}"),
    }
    let parked: i64 = node
        .db()
        .call(|conn| {
            conn.query_row(
                "SELECT count(*) FROM _inbox WHERE decision IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap()
        })
        .await
        .unwrap();
    assert_eq!(parked, 1);

    // A second knock inside the window is dropped silently.
    let (out, _) = drive(&node, STRANGER, knock, vec![]).await;
    assert!(
        out.frames().is_empty(),
        "rate-limited knock must answer nothing"
    );
}

#[tokio::test]
async fn knock_auto_admit_inserts_peer() {
    let node = test_node(NodeConfig::default()).await;
    allow(&node, "*", "*", "knock").await;
    let knock = EnvelopeObject::Knock(Knock {
        id: None,
        message: "let me in".into(),
    });
    let (out, _) = drive(&node, STRANGER, knock, vec![]).await;
    match &out.frames()[0] {
        EnvelopeObject::Decision(d) => {
            assert_eq!(d.decision, "allow");
            assert_eq!(d.decided_by, "policy");
        }
        other => panic!("expected Decision, got {other:?}"),
    }
    let hex = PeerId(STRANGER).to_string();
    let admitted: i64 = node
        .db()
        .call(move |conn| {
            conn.query_row(
                "SELECT count(*) FROM _peers WHERE endpoint_id = ?1",
                [hex],
                |r| r.get(0),
            )
            .unwrap()
        })
        .await
        .unwrap();
    assert_eq!(admitted, 1);
}

#[tokio::test]
async fn projection_is_policy_filtered_and_never_lies() {
    let node = test_node(NodeConfig::default()).await;
    node.db()
        .call(|conn| {
            conn.execute_batch(
                "INSERT INTO _projection (point_iri, kind, label, path, ord, resource) VALUES
                   ('urn:x:public', 'bare', 'public point', '', 0, NULL),
                   ('urn:x:gated', 'radiant', 'gated point', '', 1, 'notes');",
            )
            .unwrap();
        })
        .await
        .unwrap();

    // Stranger: sees the public point and projection-changed, not the gated.
    let (q, _) = query("projection", "");
    let (out, _) = drive(&node, STRANGER, q, vec![]).await;
    match &out.frames()[0] {
        EnvelopeObject::Projection(p) => {
            let iris: Vec<&str> = p.offers.iter().map(|pt| pt.iri.as_str()).collect();
            assert!(iris.contains(&"urn:x:public"));
            assert!(!iris.contains(&"urn:x:gated"));
            assert!(iris.contains(&"urn:rsntr:projection-changed"));
        }
        other => panic!("expected Projection, got {other:?}"),
    }

    // With a read allow the gated point appears.
    allow(&node, &PeerId(PEER).to_string(), "notes", "read").await;
    let (q, _) = query("projection", "");
    let (out, _) = drive(&node, PEER, q, vec![]).await;
    match &out.frames()[0] {
        EnvelopeObject::Projection(p) => {
            assert!(p.offers.iter().any(|pt| pt.iri == "urn:x:gated"));
        }
        other => panic!("expected Projection, got {other:?}"),
    }

    // An unknown path answers point-unknown.
    let (q, _) = query("projection", "no/such/path");
    let (out, _) = drive(&node, PEER, q, vec![]).await;
    match &out.frames()[0] {
        EnvelopeObject::Error(e) => assert_eq!(e.code, "point-unknown"),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn entrain_gate_and_damp() {
    let node = test_node(NodeConfig::default()).await;
    let entrain = EnvelopeObject::Entrain(resonator_protocol::Entrain {
        id: "01JENTRAIN0000000000000001".into(),
        point: "urn:rsntr:projection-changed".into(),
    });

    // A stranger may not entrain.
    let (out, _) = drive(&node, STRANGER, entrain.clone(), vec![]).await;
    assert!(matches!(out.frames()[0], EnvelopeObject::Denied(_)));

    // Admitted peer: ack, then in-band damp confirms with a second Done.
    admit(&node, PEER).await;
    let damp = EnvelopeObject::Damp(resonator_protocol::Damp {
        id: None,
        point: "urn:rsntr:projection-changed".into(),
    });
    let (out, _) = drive(&node, PEER, entrain, vec![damp]).await;
    let frames = out.frames();
    assert!(
        matches!(frames[0], EnvelopeObject::Done(_)),
        "ack: {frames:?}"
    );
    assert!(matches!(frames.last().unwrap(), EnvelopeObject::Done(_)));
    assert_eq!(node.entrainments().active(), 0, "unsubscribed after damp");

    // An unknown point answers point-unknown.
    let entrain_bad = EnvelopeObject::Entrain(resonator_protocol::Entrain {
        id: "01JENTRAIN0000000000000002".into(),
        point: "urn:x:missing".into(),
    });
    let (out, _) = drive(&node, PEER, entrain_bad, vec![]).await;
    match &out.frames()[0] {
        EnvelopeObject::Error(e) => assert_eq!(e.code, "point-unknown"),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn media_streams_header_then_raw_bytes() {
    let node = test_node(NodeConfig::default()).await;
    admit(&node, PEER).await;
    node.db()
        .call(|conn| {
            conn.execute(
                "INSERT INTO _media (name, command, content_type) \
                 VALUES ('test', 'printf hello', 'text/plain')",
                [],
            )
            .unwrap();
        })
        .await
        .unwrap();
    allow(&node, &peer_hex(), "test", "media").await;

    let (q, _) = query("media", "test");
    let (out, raw) = drive(&node, PEER, q, vec![]).await;
    match &out.frames()[0] {
        EnvelopeObject::Media(m) => assert_eq!(m.content_type, "text/plain"),
        other => panic!("expected Media, got {other:?}"),
    }
    assert_eq!(raw, b"hello");

    // Without a media allow for another source: denied.
    let (q, _) = query("media", "other");
    let (out, _) = drive(&node, PEER, q, vec![]).await;
    assert!(matches!(out.frames()[0], EnvelopeObject::Denied(_)));
}

// ---------------------------------------------------------------------------
// The sparql modulation
// ---------------------------------------------------------------------------

async fn sparql_fixture(node: &Node) {
    node.db()
        .call(|conn| {
            resonator_sparql::store::load_turtle(
                conn,
                "@prefix ex: <http://ex.org/> .
                 ex:a ex:name \"alice\" ; ex:age 34 .
                 ex:b ex:name \"bob\" .",
                None,
            )
            .unwrap();
        })
        .await
        .unwrap();
}

async fn allow_sparql_read(node: &Node, peer: &str) {
    allow(node, peer, "rdf_triples", "read").await;
    allow(node, peer, "rdf_terms", "read").await;
}

#[tokio::test]
async fn sparql_select_and_ask() {
    let node = test_node(NodeConfig::default()).await;
    admit(&node, PEER).await;
    allow_sparql_read(&node, &peer_hex()).await;
    sparql_fixture(&node).await;

    let (q, _) = query(
        "sparql",
        "PREFIX ex: <http://ex.org/> SELECT ?n WHERE { ?s ex:name ?n } ORDER BY ?n",
    );
    let (out, _) = drive(&node, PEER, q, vec![]).await;
    let frames = out.frames();
    match &frames[0] {
        EnvelopeObject::Result(h) => assert_eq!(h.columns, vec!["n"]),
        other => panic!("expected Result, got {other:?}"),
    }
    match &frames[1] {
        EnvelopeObject::Row(rows) => {
            assert_eq!(rows.len(), 2);
            assert_eq!(
                rows[0].cells[0],
                ("n".to_string(), Value::Text("\"alice\"".into()))
            );
        }
        other => panic!("expected Row, got {other:?}"),
    }
    assert!(matches!(frames[2], EnvelopeObject::Done(_)));

    let (q, _) = query(
        "sparql",
        "PREFIX ex: <http://ex.org/> ASK { ex:a ex:age 34 }",
    );
    let (out, _) = drive(&node, PEER, q, vec![]).await;
    match &out.frames()[1] {
        EnvelopeObject::Row(rows) => {
            assert_eq!(rows[0].cells[0], ("ask".to_string(), Value::Integer(1)));
        }
        other => panic!("expected Row, got {other:?}"),
    }
}

#[tokio::test]
async fn sparql_construct_returns_graph_frames() {
    let node = test_node(NodeConfig::default()).await;
    admit(&node, PEER).await;
    allow_sparql_read(&node, &peer_hex()).await;
    sparql_fixture(&node).await;

    let (q, id) = query(
        "sparql",
        "PREFIX ex: <http://ex.org/> CONSTRUCT { ?s ex:name ?n } WHERE { ?s ex:name ?n }",
    );
    let (out, _) = drive(&node, PEER, q, vec![]).await;
    let frames = out.frames();
    assert_eq!(frames.len(), 2, "frames: {frames:?}");
    match &frames[0] {
        EnvelopeObject::Graph(g) => {
            assert_eq!(g.id, id);
            assert_eq!(g.seq, 0);
            assert_eq!(g.payload.len(), 2);
            assert!(
                g.payload
                    .iter()
                    .all(|t| t.predicate.as_str() == "http://ex.org/name")
            );
        }
        other => panic!("expected Graph, got {other:?}"),
    }
    match &frames[1] {
        EnvelopeObject::Done(d) => assert_eq!(d.row_count, Some(2)),
        other => panic!("expected Done, got {other:?}"),
    }
}

#[tokio::test]
async fn sparql_update_via_execute_is_idempotent() {
    let node = test_node(NodeConfig::default()).await;
    admit(&node, PEER).await;
    allow_sparql_read(&node, &peer_hex()).await;
    allow(&node, &peer_hex(), "rdf_triples", "write").await;
    allow(&node, &peer_hex(), "rdf_terms", "write").await;

    let (e1, _) = execute(
        "sparql",
        "PREFIX ex: <http://ex.org/> INSERT DATA { ex:c ex:name \"carol\" }",
    );
    let e2 = e1.clone();
    let (out, _) = drive(&node, PEER, e1, vec![]).await;
    match out.frames().last().unwrap() {
        EnvelopeObject::Done(d) => assert_eq!(d.affected_rows, Some(1)),
        other => panic!("expected Done, got {other:?}"),
    }
    // Retransmit: answered from _applied, not applied twice.
    let (out, _) = drive(&node, PEER, e2, vec![]).await;
    assert!(matches!(
        out.frames().last().unwrap(),
        EnvelopeObject::Done(_)
    ));
    let n: i64 = node
        .db()
        .call(|conn| {
            conn.query_row("SELECT count(*) FROM rdf_triples", [], |r| r.get(0))
                .unwrap()
        })
        .await
        .unwrap();
    assert_eq!(n, 1);

    // A read denied peer-side still audits: sparql without read policy.
    let (q, _) = query("sparql", "SELECT ?s WHERE { ?s ?p ?o }");
    let (out, _) = drive(&node, STRANGER, q, vec![]).await;
    assert!(matches!(out.frames()[0], EnvelopeObject::Denied(_)));
}

#[tokio::test]
async fn sparql_update_in_query_envelope_is_rejected() {
    let node = test_node(NodeConfig::default()).await;
    admit(&node, PEER).await;
    let (q, _) = query("sparql", "INSERT DATA { <urn:a> <urn:b> <urn:c> }");
    let (out, _) = drive(&node, PEER, q, vec![]).await;
    match &out.frames()[0] {
        EnvelopeObject::Error(e) => {
            assert_eq!(e.code, "protocol-error");
            assert!(e.reason.as_deref().unwrap().contains("rsntr:Execute"));
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The audio-duplex modulation
// ---------------------------------------------------------------------------

/// A stream double with a client write half: queued raw chunks, then Fin
/// (`recv_raw -> Ok(None)`). `closed()` resolves shortly after, standing
/// in for the client dropping the whole stream.
struct DuplexStream {
    out: Recorded,
    raw_incoming: VecDeque<Vec<u8>>,
    /// When true the client never signals departure, so only the
    /// handler's own half-open bound can end the session.
    never_closes: bool,
}

impl RequestStream for DuplexStream {
    async fn send(&mut self, obj: &EnvelopeObject) -> Result<(), TransportError> {
        self.out.sent.lock().unwrap().push(obj.clone());
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<EnvelopeObject>, TransportError> {
        Ok(None)
    }

    async fn send_raw(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.out.raw.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    }

    async fn recv_raw(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        Ok(self.raw_incoming.pop_front())
    }

    async fn closed(&mut self) {
        if self.never_closes {
            std::future::pending::<()>().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    async fn finish(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

async fn drive_duplex(
    node: &Node,
    peer: [u8; 32],
    first: EnvelopeObject,
    raw_incoming: Vec<Vec<u8>>,
) -> (Recorded, Vec<u8>) {
    drive_duplex_with(node, peer, first, raw_incoming, false).await
}

async fn drive_duplex_with(
    node: &Node,
    peer: [u8; 32],
    first: EnvelopeObject,
    raw_incoming: Vec<Vec<u8>>,
    never_closes: bool,
) -> (Recorded, Vec<u8>) {
    let out = Recorded::default();
    let stream = DuplexStream {
        out: out.clone(),
        raw_incoming: raw_incoming.into(),
        never_closes,
    };
    node.handle(IncomingRequest {
        peer: PeerId(peer),
        peer_hello: hello(),
        first,
        stream,
    })
    .await
    .ok();
    let raw = out.raw_bytes();
    (out, raw)
}

async fn duplex_source(node: &Node, name: &str, command: &str, accepts: Option<&str>) {
    let (name, command) = (name.to_string(), command.to_string());
    let accepts = accepts.map(str::to_string);
    node.db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO _media (name, command, content_type, accepts) \
                 VALUES (?1, ?2, 'application/octet-stream', ?3)",
                (name, command, accepts),
            )
            .unwrap();
        })
        .await
        .unwrap();
}

/// The `cat` echo proves the whole two-phase handoff: queued client
/// bytes reach the source's stdin, the Fin EOFs it, and the echoed
/// output drains through phase 2 until the source exits.
#[tokio::test]
async fn audio_duplex_echoes_raw_bytes_both_ways() {
    let node = test_node(NodeConfig::default()).await;
    admit(&node, PEER).await;
    duplex_source(&node, "echo", "cat", Some("application/octet-stream")).await;
    allow(&node, &peer_hex(), "echo", "audio-duplex").await;

    let (q, id) = query("audio-duplex", "echo");
    let (out, raw) = drive_duplex(&node, PEER, q, vec![b"hello ".to_vec(), b"door".to_vec()]).await;
    match &out.frames()[0] {
        EnvelopeObject::AudioDuplex(d) => {
            assert_eq!(d.id, id);
            assert_eq!(d.content_type.as_deref(), Some("application/octet-stream"));
            assert_eq!(d.accepts, "application/octet-stream");
        }
        other => panic!("expected AudioDuplex, got {other:?}"),
    }
    assert_eq!(raw, b"hello door");
}

#[tokio::test]
async fn audio_duplex_requires_accepts() {
    let node = test_node(NodeConfig::default()).await;
    admit(&node, PEER).await;
    duplex_source(&node, "oneway", "printf hi", None).await;
    allow(&node, &peer_hex(), "oneway", "audio-duplex").await;

    let (q, _) = query("audio-duplex", "oneway");
    let (out, _) = drive_duplex(&node, PEER, q, vec![]).await;
    match &out.frames()[0] {
        EnvelopeObject::Error(e) => {
            assert_eq!(e.code, "engine-error");
            assert!(e.reason.as_deref().unwrap().contains("not a duplex source"));
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn audio_duplex_denied_without_policy() {
    let node = test_node(NodeConfig::default()).await;
    admit(&node, PEER).await;
    duplex_source(&node, "echo", "cat", Some("application/octet-stream")).await;

    let (q, _) = query("audio-duplex", "echo");
    let (out, _) = drive_duplex(&node, PEER, q, vec![]).await;
    assert!(matches!(out.frames()[0], EnvelopeObject::Denied(_)));
}

/// The actions are deliberately separate: watching a place does not
/// grant talking into it.
#[tokio::test]
async fn media_policy_does_not_grant_audio_duplex() {
    let node = test_node(NodeConfig::default()).await;
    admit(&node, PEER).await;
    duplex_source(&node, "echo", "cat", Some("application/octet-stream")).await;
    allow(&node, &peer_hex(), "echo", "media").await;

    let (q, _) = query("audio-duplex", "echo");
    let (out, _) = drive_duplex(&node, PEER, q, vec![]).await;
    assert!(matches!(out.frames()[0], EnvelopeObject::Denied(_)));

    // And the duplex row still serves plain one-way media.
    let (q, _) = query("media", "echo");
    let (out, _) = drive(&node, PEER, q, vec![]).await;
    assert!(matches!(out.frames()[0], EnvelopeObject::Media(_)));
}

#[tokio::test]
async fn audio_duplex_teardown_kills_process_group() {
    let node = test_node(NodeConfig::default()).await;
    admit(&node, PEER).await;
    duplex_source(
        &node,
        "hang",
        "sleep 5987",
        Some("application/octet-stream"),
    )
    .await;
    allow(&node, &peer_hex(), "hang", "audio-duplex").await;

    let (q, _) = query("audio-duplex", "hang");
    // No client bytes: Fin immediately, then closed() fires and the
    // handler must reap the silent source's whole process group.
    let (out, _) = drive_duplex(&node, PEER, q, vec![]).await;
    assert!(matches!(out.frames()[0], EnvelopeObject::AudioDuplex(_)));
    let left = std::process::Command::new("pgrep")
        .args(["-f", "sleep 5987"])
        .output()
        .expect("pgrep runs");
    assert!(
        left.stdout.is_empty(),
        "source survived teardown: {}",
        String::from_utf8_lossy(&left.stdout)
    );
}

/// A talk sink that never writes and never exits (its own output is
/// blocked) must not hold the session open after the caller finishes:
/// the half-open phase is bounded, so the process group is always
/// released.
#[tokio::test]
async fn audio_duplex_half_open_phase_is_bounded() {
    let node = test_node(NodeConfig::default()).await;
    admit(&node, PEER).await;
    duplex_source(
        &node,
        "wedged",
        "cat > /dev/null; sleep 5993",
        Some("application/octet-stream"),
    )
    .await;
    allow(&node, &peer_hex(), "wedged", "audio-duplex").await;

    let (q, _) = query("audio-duplex", "wedged");
    let started = std::time::Instant::now();
    // One chunk, then Fin. The client never signals departure, so the
    // only way out is the handler's own bound.
    let (out, _) = drive_duplex_with(&node, PEER, q, vec![b"hi".to_vec()], true).await;
    assert!(matches!(out.frames()[0], EnvelopeObject::AudioDuplex(_)));
    let elapsed = started.elapsed();
    assert!(
        elapsed >= std::time::Duration::from_secs(4),
        "the session ended before the half-open bound: {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "the half-open phase did not end: {elapsed:?}"
    );
    let left = std::process::Command::new("pgrep")
        .args(["-f", "sleep 5993"])
        .output()
        .expect("pgrep runs");
    assert!(
        left.stdout.is_empty(),
        "the wedged source survived: {}",
        String::from_utf8_lossy(&left.stdout)
    );
}

/// A source that never drains its stdin must not wedge the session: the
/// caller keeps talking, the audio is dropped, and the hangup is still
/// processed promptly (the failure this guards against left a live
/// process behind on every call).
#[tokio::test]
async fn audio_duplex_survives_a_source_that_never_reads() {
    let node = test_node(NodeConfig::default()).await;
    admit(&node, PEER).await;
    // Reads nothing, writes nothing, outlives the call if left alone.
    duplex_source(
        &node,
        "deaf",
        "sleep 5995",
        Some("application/octet-stream"),
    )
    .await;
    allow(&node, &peer_hex(), "deaf", "audio-duplex").await;

    // Far more audio than a pipe buffer holds.
    let chunks: Vec<Vec<u8>> = (0..64).map(|_| vec![0u8; 32 * 1024]).collect();
    let (q, _) = query("audio-duplex", "deaf");
    let started = std::time::Instant::now();
    let (out, _) = drive_duplex_with(&node, PEER, q, chunks, true).await;
    assert!(matches!(out.frames()[0], EnvelopeObject::AudioDuplex(_)));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "the session wedged on a source that never reads: {:?}",
        started.elapsed()
    );
    let left = std::process::Command::new("pgrep")
        .args(["-f", "sleep 5995"])
        .output()
        .expect("pgrep runs");
    assert!(
        left.stdout.is_empty(),
        "the deaf source survived: {}",
        String::from_utf8_lossy(&left.stdout)
    );
}
