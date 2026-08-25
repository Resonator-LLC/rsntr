//! Owner channel tests (docs/owner-channel.md): the ungated dispatch
//! entry beside the pipeline. Driven through the same local stream
//! double as the pipeline tests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use resonator_authenticator::Chain;
use resonator_node::{DbHandle, Node, NodeConfig, open_node_db_in_memory, seed_rsntr_defaults};
use resonator_protocol::{EnvelopeObject, Request, RequestKind, Value};
use resonator_transport::{IncomingRequest, PeerId, RequestStream, TransportError, basic_hello};

// ---------------------------------------------------------------------------
// Local stream double
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Recorded {
    sent: Arc<Mutex<Vec<EnvelopeObject>>>,
}

impl Recorded {
    fn frames(&self) -> Vec<EnvelopeObject> {
        self.sent.lock().unwrap().clone()
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

    async fn finish(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

const REMOTE: [u8; 32] = [1u8; 32];

/// The node's own endpoint id, seeded into `_rsntr` (what audit rows
/// must carry as the owner peer).
fn own_hex() -> String {
    "ee".repeat(32)
}

async fn test_node(config: NodeConfig) -> Arc<Node> {
    let conn = open_node_db_in_memory().expect("open db");
    seed_rsntr_defaults(&conn).expect("seed");
    resonator_node::set_rsntr(&conn, "endpoint_id", &own_hex()).expect("endpoint id");
    conn.execute_batch(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);
         INSERT INTO notes (body) VALUES ('one'), ('two'), ('three');",
    )
    .expect("fixture");
    Arc::new(Node::new(
        DbHandle::spawn(conn),
        Chain::with_builtin_tiers(),
        config,
    ))
}

/// Drives one envelope through the owner channel.
async fn drive_owner(
    node: &Node,
    first: EnvelopeObject,
    incoming: Vec<EnvelopeObject>,
) -> Recorded {
    let out = Recorded::default();
    let mut stream = LocalStream {
        out: out.clone(),
        incoming: incoming.into(),
    };
    node.handle_owner(first, &mut stream).await.ok();
    out
}

/// Drives one envelope through the gated remote pipeline.
async fn drive_remote(node: &Node, peer: [u8; 32], first: EnvelopeObject) -> Recorded {
    let out = Recorded::default();
    let stream = LocalStream {
        out: out.clone(),
        incoming: VecDeque::new(),
    };
    node.handle(IncomingRequest {
        peer: PeerId(peer),
        peer_hello: basic_hello(&["help"], None),
        first,
        stream,
    })
    .await
    .ok();
    out
}

fn query(modulation: &str, signal: &str) -> EnvelopeObject {
    Request::new(RequestKind::Query, modulation, signal).to_envelope()
}

fn execute(modulation: &str, signal: &str) -> EnvelopeObject {
    Request::new(RequestKind::Execute, modulation, signal).to_envelope()
}

/// The most recent `_audit` row's (direction, peer, decision, decided_by).
async fn last_audit(node: &Node) -> (String, String, String, String) {
    node.db()
        .call(|conn| {
            conn.query_row(
                "SELECT direction, peer, decision, decided_by \
                 FROM _audit ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
        })
        .await
        .unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn owner_ddl_allowed_where_remote_is_banned() {
    let node = test_node(NodeConfig::default()).await;

    // Remote regression: an admitted peer with a blanket write allow is
    // still banned from DDL.
    let remote_hex = PeerId(REMOTE).to_string();
    node.db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO _peers (endpoint_id, added_at) VALUES (?1, 'now')",
                [&remote_hex],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
                 VALUES (?1, '*', 'write', 'allow')",
                [&remote_hex],
            )
            .unwrap();
        })
        .await
        .unwrap();
    let out = drive_remote(
        &node,
        REMOTE,
        execute("sql-sqlite", "CREATE TABLE evil (x)"),
    )
    .await;
    match &out.frames()[0] {
        EnvelopeObject::Denied(d) => {
            assert!(d.reason.as_deref().unwrap().contains("DDL"));
        }
        other => panic!("remote DDL should be Denied, got {other:?}"),
    }

    // The same statement through the owner channel succeeds.
    let out = drive_owner(
        &node,
        execute("sql-sqlite", "CREATE TABLE gadgets (id, name)"),
        vec![],
    )
    .await;
    let frames = out.frames();
    assert!(
        matches!(frames.last(), Some(EnvelopeObject::Done(_))),
        "owner DDL should answer Done, got {frames:?}"
    );

    // The table exists and is usable through the owner channel.
    let out = drive_owner(
        &node,
        execute("sql-sqlite", "INSERT INTO gadgets VALUES (1, 'sprocket')"),
        vec![],
    )
    .await;
    match out.frames().last().unwrap() {
        EnvelopeObject::Done(d) => assert_eq!(d.affected_rows, Some(1)),
        other => panic!("expected Done, got {other:?}"),
    }

    let (direction, peer, decision, decided_by) = last_audit(&node).await;
    assert_eq!(direction, "local");
    assert_eq!(peer, own_hex());
    assert_eq!(decision, "allow");
    assert_eq!(decided_by, "owner");
}

#[tokio::test]
async fn owner_query_needs_no_policy_and_audits_owner() {
    let node = test_node(NodeConfig::default()).await;
    let out = drive_owner(
        &node,
        query("sql-sqlite", "SELECT id, body FROM notes"),
        vec![],
    )
    .await;
    let frames = out.frames();
    match &frames[1] {
        EnvelopeObject::Row(rows) => assert_eq!(rows.len(), 3),
        other => panic!("expected Row, got {other:?}"),
    }
    let (direction, _, decision, decided_by) = last_audit(&node).await;
    assert_eq!(
        (direction.as_str(), decision.as_str(), decided_by.as_str()),
        ("local", "allow", "owner")
    );

    // The footprint is still collected for the ledger.
    let footprint: String = node
        .db()
        .call(|conn| {
            conn.query_row(
                "SELECT footprint FROM _audit ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap()
        })
        .await
        .unwrap();
    assert!(footprint.contains("notes"), "footprint: {footprint}");
}

#[tokio::test]
async fn owner_residual_bans_answer_denied() {
    let node = test_node(NodeConfig::default()).await;
    for sql in [
        "ATTACH DATABASE ':memory:' AS other",
        "SELECT load_extension('evil')",
    ] {
        let out = drive_owner(&node, query("sql-sqlite", sql), vec![]).await;
        match &out.frames()[0] {
            EnvelopeObject::Denied(_) => {}
            other => panic!("{sql}: expected Denied, got {other:?}"),
        }
        let (direction, _, decision, decided_by) = last_audit(&node).await;
        assert_eq!(
            (direction.as_str(), decision.as_str(), decided_by.as_str()),
            ("local", "deny", "owner"),
            "{sql}"
        );
    }
}

#[tokio::test]
async fn owner_pragma_allowed() {
    let node = test_node(NodeConfig::default()).await;
    let out = drive_owner(&node, query("sql-sqlite", "PRAGMA user_version"), vec![]).await;
    let frames = out.frames();
    assert!(
        matches!(frames.last(), Some(EnvelopeObject::Done(_))),
        "owner PRAGMA should answer Done, got {frames:?}"
    );
}

#[tokio::test]
async fn owner_transaction_control_savepoint_works_bare_begin_fails() {
    let node = test_node(NodeConfig::default()).await;

    // SAVEPOINT (the reason transaction control is permitted at
    // prepare) passes the authorizer and runs inside the per-request
    // transaction; the savepoint does not outlive the request.
    let out = drive_owner(&node, execute("sql-sqlite", "SAVEPOINT s1"), vec![]).await;
    let frames = out.frames();
    assert!(
        matches!(frames.last(), Some(EnvelopeObject::Done(_))),
        "SAVEPOINT: expected Done, got {frames:?}"
    );
    let out = drive_owner(&node, execute("sql-sqlite", "RELEASE s1"), vec![]).await;
    match &out.frames().last().unwrap() {
        EnvelopeObject::Error(e) => {
            // Not a Denied: the authorizer permits it; the savepoint is
            // simply gone with the previous request's transaction.
            assert_eq!(e.code, "engine-error");
        }
        other => panic!("cross-request RELEASE: expected engine-error, got {other:?}"),
    }

    // A bare BEGIN prepares (no Denied) but fails at step: the request
    // already runs inside its own implicit transaction, and sqlite
    // refuses to nest. Cross-request owner transactions are out of
    // scope (docs/owner-channel.md sec 2).
    let out = drive_owner(&node, execute("sql-sqlite", "BEGIN"), vec![]).await;
    match out.frames().last().unwrap() {
        EnvelopeObject::Error(e) => assert_eq!(e.code, "engine-error"),
        other => panic!("bare BEGIN: expected engine-error, got {other:?}"),
    }
}

#[tokio::test]
async fn owner_limits_still_clamp() {
    let node = test_node(NodeConfig {
        max_rows: 2,
        ..NodeConfig::default()
    })
    .await;
    let out = drive_owner(&node, query("sql-sqlite", "SELECT id FROM notes"), vec![]).await;
    match out.frames().last().unwrap() {
        EnvelopeObject::Done(d) => {
            assert_eq!(d.row_count, Some(2));
            assert!(d.truncated, "the NodeConfig ceiling clamps the owner too");
        }
        other => panic!("expected Done, got {other:?}"),
    }
}

#[tokio::test]
async fn owner_write_is_idempotent_via_applied() {
    let node = test_node(NodeConfig::default()).await;
    let e1 = execute("sql-sqlite", "INSERT INTO notes (body) VALUES ('again')");
    let e2 = e1.clone();
    drive_owner(&node, e1, vec![]).await;
    drive_owner(&node, e2, vec![]).await;
    let n: i64 = node
        .db()
        .call(|conn| {
            conn.query_row("SELECT count(*) FROM notes WHERE body = 'again'", [], |r| {
                r.get(0)
            })
            .unwrap()
        })
        .await
        .unwrap();
    assert_eq!(n, 1, "a retransmitted owner Execute must not apply twice");
}

#[tokio::test]
async fn owner_sparql_update_without_policy() {
    let node = test_node(NodeConfig::default()).await;
    let out = drive_owner(
        &node,
        execute(
            "sparql",
            "PREFIX ex: <http://ex.org/> INSERT DATA { ex:a ex:name \"alice\" }",
        ),
        vec![],
    )
    .await;
    match out.frames().last().unwrap() {
        EnvelopeObject::Done(d) => assert_eq!(d.affected_rows, Some(1)),
        other => panic!("expected Done, got {other:?}"),
    }
    let (direction, _, decision, decided_by) = last_audit(&node).await;
    assert_eq!(
        (direction.as_str(), decision.as_str(), decided_by.as_str()),
        ("local", "allow", "owner")
    );

    let out = drive_owner(
        &node,
        query(
            "sparql",
            "PREFIX ex: <http://ex.org/> SELECT ?n WHERE { ?s ex:name ?n }",
        ),
        vec![],
    )
    .await;
    match &out.frames()[1] {
        EnvelopeObject::Row(rows) => assert_eq!(rows.len(), 1),
        other => panic!("expected Row, got {other:?}"),
    }
}

#[tokio::test]
async fn owner_chat_appends_without_policy() {
    let node = test_node(NodeConfig::default()).await;
    let signal = "[] a rsntr:Message ; rsntr:id \"01K1CH4TOWNER000000000001\" ; \
                  rsntr:body \"note to self\" .";
    let out = drive_owner(&node, execute("chat", signal), vec![]).await;
    match out.frames().last().unwrap() {
        EnvelopeObject::Done(d) => assert_eq!(d.affected_rows, Some(1)),
        other => panic!("expected Done, got {other:?}"),
    }
    let (direction, peer, decision, decided_by) = last_audit(&node).await;
    assert_eq!(
        (direction.as_str(), decision.as_str(), decided_by.as_str()),
        ("local", "allow", "owner")
    );
    assert_eq!(peer, own_hex(), "the owner append is authored by the node");
    let sender: String = node
        .db()
        .call(|conn| {
            conn.query_row("SELECT sender FROM chat_messages LIMIT 1", [], |r| r.get(0))
                .unwrap()
        })
        .await
        .unwrap();
    assert_eq!(sender, own_hex());
}

#[tokio::test]
async fn owner_projection_is_unfiltered() {
    let node = test_node(NodeConfig::default()).await;
    node.db()
        .call(|conn| {
            conn.execute_batch(
                "INSERT INTO _projection (point_iri, kind, label, path, ord, resource) VALUES
                   ('urn:x:gated', 'radiant', 'gated point', '', 0, 'notes');",
            )
            .unwrap();
        })
        .await
        .unwrap();

    // No policy rows at all: a remote stranger cannot see the gated
    // point, the owner sees it regardless.
    let out = drive_remote(&node, REMOTE, query("projection", "")).await;
    match &out.frames()[0] {
        EnvelopeObject::Projection(p) => {
            assert!(!p.offers.iter().any(|pt| pt.iri == "urn:x:gated"));
        }
        other => panic!("expected Projection, got {other:?}"),
    }
    let out = drive_owner(&node, query("projection", ""), vec![]).await;
    match &out.frames()[0] {
        EnvelopeObject::Projection(p) => {
            assert!(p.offers.iter().any(|pt| pt.iri == "urn:x:gated"));
        }
        other => panic!("expected Projection, got {other:?}"),
    }
}

#[tokio::test]
async fn owner_entrain_needs_no_policy_and_damps() {
    let node = test_node(NodeConfig::default()).await;
    node.db()
        .call(|conn| {
            conn.execute_batch(
                "INSERT INTO _projection (point_iri, kind, path, ord, resource) VALUES
                   ('urn:x:pulse', 'sympathetic', '', 0, 'notes');",
            )
            .unwrap();
        })
        .await
        .unwrap();
    let entrain = EnvelopeObject::Entrain(resonator_protocol::Entrain {
        id: "01JOWNERENTRAIN00000000001".into(),
        point: "urn:x:pulse".into(),
    });
    let damp = EnvelopeObject::Damp(resonator_protocol::Damp {
        id: None,
        point: "urn:x:pulse".into(),
    });
    let out = drive_owner(&node, entrain, vec![damp]).await;
    let frames = out.frames();
    assert!(matches!(frames[0], EnvelopeObject::Done(_)), "{frames:?}");
    assert!(matches!(frames.last().unwrap(), EnvelopeObject::Done(_)));
    assert_eq!(node.entrainments().active(), 0);
}

#[tokio::test]
async fn owner_media_is_unsupported_and_knock_is_protocol_error() {
    let node = test_node(NodeConfig::default()).await;
    let out = drive_owner(&node, query("media", "cam"), vec![]).await;
    match &out.frames()[0] {
        EnvelopeObject::Error(e) => assert_eq!(e.code, "mod-unsupported"),
        other => panic!("expected Error, got {other:?}"),
    }
    let out = drive_owner(
        &node,
        EnvelopeObject::Knock(resonator_protocol::Knock {
            id: None,
            message: "why would the owner knock".into(),
        }),
        vec![],
    )
    .await;
    match &out.frames()[0] {
        EnvelopeObject::Error(e) => assert_eq!(e.code, "protocol-error"),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn remote_gate_still_intact() {
    let node = test_node(NodeConfig::default()).await;
    // An owner interaction first, so any lane leakage would surface.
    drive_owner(&node, query("sql-sqlite", "SELECT id FROM notes"), vec![]).await;

    // A stranger is still turned away at the peer gate.
    let out = drive_remote(&node, REMOTE, query("sql-sqlite", "SELECT id FROM notes")).await;
    match &out.frames()[0] {
        EnvelopeObject::Denied(d) => {
            assert!(d.reason.as_deref().unwrap().contains("unknown peer"));
        }
        other => panic!("expected Denied, got {other:?}"),
    }
    let (direction, _, decision, decided_by) = last_audit(&node).await;
    assert_eq!(
        (direction.as_str(), decision.as_str(), decided_by.as_str()),
        ("in", "deny", "peer-gate")
    );
}

#[tokio::test]
async fn chat_params_are_refused_on_the_gated_surface() {
    let node = test_node(NodeConfig::default()).await;
    let remote_hex = PeerId(REMOTE).to_string();
    node.db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO _peers (endpoint_id, added_at) VALUES (?1, 'now')",
                [&remote_hex],
            )
            .unwrap();
        })
        .await
        .unwrap();
    let mut req = Request::new(
        RequestKind::Execute,
        "chat",
        "[] a rsntr:Message ; rsntr:body \"hi\" .",
    );
    req.params = vec![Value::Text("/etc/passwd".into())];
    let out = drive_remote(&node, REMOTE, req.to_envelope()).await;
    match &out.frames()[0] {
        EnvelopeObject::Error(e) => assert_eq!(e.code, "protocol-error"),
        other => panic!("expected Error, got {other:?}"),
    }
}
