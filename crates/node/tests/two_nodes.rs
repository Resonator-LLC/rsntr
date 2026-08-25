//! In-process two-endpoint test: a serving node behind an offline iroh
//! transport, a bare client transport dialing it. Exercises query, help,
//! knock admission, projection, and entrain round-trips over the real
//! wire path.

use std::sync::Arc;
use std::time::Duration;

use resonator_authenticator::Chain;
use resonator_node::{
    DbHandle, Node, NodeConfig, node_hello, open_node_db_in_memory, seed_rsntr_defaults, set_rsntr,
};
use resonator_protocol::{Damp, Entrain, EnvelopeObject, Knock, Request, RequestKind, Value};
use resonator_transport::{IrohConfig, IrohTransport, RequestStream, Transport, basic_hello};

const TEST_TIMEOUT: Duration = Duration::from_secs(60);

async fn recv_frame<S: RequestStream>(stream: &mut S) -> EnvelopeObject {
    tokio::time::timeout(Duration::from_secs(20), stream.recv())
        .await
        .expect("recv timed out")
        .expect("recv failed")
        .expect("stream closed early")
}

fn query(modulation: &str, signal: &str) -> EnvelopeObject {
    Request::new(RequestKind::Query, modulation, signal).to_envelope()
}

#[tokio::test(flavor = "multi_thread")]
async fn two_endpoint_round_trips() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        // Serving node: in-memory db, defaults seeded, auto-admit knocks,
        // wildcard reads for everyone.
        let conn = open_node_db_in_memory().expect("open db");
        seed_rsntr_defaults(&conn).expect("seed");
        set_rsntr(&conn, "hint", "query mod 'help' for usage").expect("hint");
        set_rsntr(&conn, "help_text", "Welcome to the test node.").expect("help_text");
        conn.execute_batch(
            "CREATE TABLE greetings (id INTEGER PRIMARY KEY, text TEXT);
             INSERT INTO greetings (text) VALUES ('hello'), ('world');
             INSERT INTO _policy (peer_or_group, table_name, action, effect) VALUES
               ('*', '*', 'knock', 'allow'),
               ('*', '*', 'read', 'allow'),
               ('*', 'greetings', 'read', 'allow');",
        )
        .expect("fixture");
        let hello = node_hello(&conn);
        assert!(
            ["help", "sql-sqlite", "sparql", "projection", "media"]
                .iter()
                .all(|m| hello.mods.iter().any(|x| x == m)),
            "hello mods: {:?}",
            hello.mods
        );

        let node = Arc::new(Node::new(
            DbHandle::spawn(conn),
            Chain::with_builtin_tiers(),
            NodeConfig::default(),
        ));

        let (server, server_rx) = IrohTransport::bind(IrohConfig::offline(hello))
            .await
            .expect("bind server");
        let server_id = server.peer_id();
        tokio::spawn(node.clone().run(server_rx));

        let (client, _client_rx) =
            IrohTransport::bind(IrohConfig::offline(basic_hello(&["help"], None)))
                .await
                .expect("bind client");
        client.add_peer_addrs(server_id, server.direct_addrs());

        // 1. Help round-trip (pre-admission).
        let (mut stream, peer_hello) = client.open(server_id).await.expect("open help");
        assert!(peer_hello.mods.iter().any(|m| m == "sparql"));
        stream.send(&query("help", "")).await.expect("send help");
        stream.finish().await.expect("finish");
        match recv_frame(&mut stream).await {
            EnvelopeObject::Help(h) => {
                assert!(h.signal.contains("Welcome to the test node."));
                assert!(h.topics.iter().any(|t| t == "knock"));
            }
            other => panic!("expected Help, got {other:?}"),
        }

        // 2. A query before admission is denied at the peer gate.
        let (mut stream, _) = client.open(server_id).await.expect("open early query");
        stream
            .send(&query("sql-sqlite", "SELECT text FROM greetings"))
            .await
            .expect("send");
        stream.finish().await.expect("finish");
        match recv_frame(&mut stream).await {
            EnvelopeObject::Denied(d) => {
                assert!(d.reason.as_deref().unwrap().contains("unknown peer"));
            }
            other => panic!("expected Denied, got {other:?}"),
        }

        // 3. Knock: auto-admitted by the wildcard policy row.
        let (mut stream, _) = client.open(server_id).await.expect("open knock");
        stream
            .send(&EnvelopeObject::Knock(Knock {
                id: None,
                message: "test client requesting admission".into(),
            }))
            .await
            .expect("send knock");
        stream.finish().await.expect("finish");
        match recv_frame(&mut stream).await {
            EnvelopeObject::Decision(d) => {
                assert_eq!(d.decision, "allow");
                assert_eq!(d.decided_by, "policy");
            }
            other => panic!("expected Decision, got {other:?}"),
        }

        // 4. The same query now streams Result/Row/Done.
        let (mut stream, _) = client.open(server_id).await.expect("open query");
        stream
            .send(&query(
                "sql-sqlite",
                "SELECT text FROM greetings ORDER BY id",
            ))
            .await
            .expect("send");
        stream.finish().await.expect("finish");
        match recv_frame(&mut stream).await {
            EnvelopeObject::Result(h) => assert_eq!(h.columns, vec!["text"]),
            other => panic!("expected Result, got {other:?}"),
        }
        match recv_frame(&mut stream).await {
            EnvelopeObject::Row(rows) => {
                assert_eq!(rows.len(), 2);
                assert_eq!(
                    rows[0].cells[0],
                    ("text".to_string(), Value::Text("hello".into()))
                );
            }
            other => panic!("expected Row, got {other:?}"),
        }
        match recv_frame(&mut stream).await {
            EnvelopeObject::Done(d) => assert_eq!(d.row_count, Some(2)),
            other => panic!("expected Done, got {other:?}"),
        }

        // 5. Projection: the well-known projection-changed point is there.
        let (mut stream, _) = client.open(server_id).await.expect("open projection");
        stream.send(&query("projection", "")).await.expect("send");
        stream.finish().await.expect("finish");
        match recv_frame(&mut stream).await {
            EnvelopeObject::Projection(p) => {
                assert!(
                    p.offers
                        .iter()
                        .any(|pt| pt.iri == "urn:rsntr:projection-changed")
                );
            }
            other => panic!("expected Projection, got {other:?}"),
        }

        // 6. Entrain projection-changed: ack, vibration on a _policy
        //    write, then damp and the confirming Done.
        let point = "urn:rsntr:projection-changed".to_string();
        let (mut stream, _) = client.open(server_id).await.expect("open entrain");
        stream
            .send(&EnvelopeObject::Entrain(Entrain {
                id: "01JENTRAIN00000000000000AA".into(),
                point: point.clone(),
            }))
            .await
            .expect("send entrain");
        match recv_frame(&mut stream).await {
            EnvelopeObject::Done(_) => {}
            other => panic!("expected Done ack, got {other:?}"),
        }
        // Server-side change to _policy fires the update hook.
        node.db()
            .call(|conn| {
                conn.execute(
                    "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
                     VALUES ('*', 'greetings', 'write', 'allow')",
                    [],
                )
                .unwrap();
            })
            .await
            .expect("policy write");
        match recv_frame(&mut stream).await {
            EnvelopeObject::Vibration(v) => {
                assert_eq!(v.point, point);
                assert_eq!(v.seq, 0);
            }
            other => panic!("expected Vibration, got {other:?}"),
        }
        stream
            .send(&EnvelopeObject::Damp(Damp {
                id: None,
                point: point.clone(),
            }))
            .await
            .expect("send damp");
        // The damp confirmation is a Done; a coalesced second vibration
        // may arrive first.
        loop {
            match recv_frame(&mut stream).await {
                EnvelopeObject::Done(_) => break,
                EnvelopeObject::Vibration(_) => continue,
                other => panic!("expected Done, got {other:?}"),
            }
        }
        let _ = stream.finish().await;

        // The audit ledger saw every decision.
        let audits: i64 = node
            .db()
            .call(|conn| {
                conn.query_row("SELECT count(*) FROM _audit", [], |r| r.get(0))
                    .unwrap()
            })
            .await
            .expect("audit count");
        assert!(audits >= 5, "expected at least 5 audit rows, got {audits}");

        server.shutdown().await;
        client.shutdown().await;
    })
    .await
    .expect("test timed out");
}

/// Audio-duplex over the real iroh wire: the client keeps its write half
/// open after the request frame, sends raw bytes upstream, receives the
/// echo downstream, and the Fin winds the source down (envelope doc 4.3).
#[tokio::test(flavor = "multi_thread")]
async fn audio_duplex_over_iroh() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let conn = open_node_db_in_memory().expect("open db");
        seed_rsntr_defaults(&conn).expect("seed");
        conn.execute_batch(
            "INSERT INTO _media (name, command, content_type, accepts) VALUES
               ('echo', 'cat', 'application/octet-stream', 'application/octet-stream');
             INSERT INTO _policy (peer_or_group, table_name, action, effect) VALUES
               ('*', 'echo', 'audio-duplex', 'allow');",
        )
        .expect("fixture");
        let hello = node_hello(&conn);
        assert!(
            hello.mods.iter().any(|m| m == "audio-duplex"),
            "hello mods: {:?}",
            hello.mods
        );
        let node = Arc::new(Node::new(
            DbHandle::spawn(conn),
            Chain::with_builtin_tiers(),
            NodeConfig::default(),
        ));

        let (server, server_rx) = IrohTransport::bind(IrohConfig::offline(hello))
            .await
            .expect("bind server");
        let server_id = server.peer_id();
        tokio::spawn(node.clone().run(server_rx));

        let (client, _client_rx) =
            IrohTransport::bind(IrohConfig::offline(basic_hello(&["help"], None)))
                .await
                .expect("bind client");
        client.add_peer_addrs(server_id, server.direct_addrs());

        // Admit the client directly (this test is about the duplex lane).
        let client_hex = client.peer_id().to_string();
        node.db()
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO _peers (endpoint_id, added_at) VALUES (?1, 'now')",
                    [client_hex],
                )
                .unwrap();
            })
            .await
            .unwrap();

        let (mut stream, _) = client.open(server_id).await.expect("open duplex");
        stream
            .send(&query("audio-duplex", "echo"))
            .await
            .expect("send query");
        // NO finish: the write half stays open for upstream audio.

        match recv_frame(&mut stream).await {
            EnvelopeObject::AudioDuplex(d) => {
                assert_eq!(d.accepts, "application/octet-stream");
            }
            other => panic!("expected AudioDuplex, got {other:?}"),
        }

        // Upstream after the header, in two writes; then Fin.
        stream.send_raw(b"ring ").await.expect("send raw 1");
        stream.send_raw(b"ring").await.expect("send raw 2");
        stream.finish().await.expect("fin");

        // Downstream: the echo, then end-of-stream when cat exits.
        let mut got = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(20), stream.recv_raw())
                .await
                .expect("recv_raw timed out")
                .expect("recv_raw failed")
            {
                Some(chunk) => got.extend(chunk),
                None => break,
            }
        }
        assert_eq!(got, b"ring ring");
    })
    .await
    .expect("test timed out");
}
