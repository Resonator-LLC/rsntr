//! The M4 acceptance demo: a scripted two-node (plus one stranger)
//! offline session covering every modulation.
//!
//! Node A (alice) and node B (bob) live in fresh directories and admit
//! each other; node C is a stranger. Everything runs in-process on
//! localhost with no relays and no discovery:
//!
//! 1. sql-sqlite SELECT round-trip A -> B.
//! 2. sparql SELECT and CONSTRUCT (rsntr:Graph frames) round-trips.
//! 3. help.
//! 4. Projection listing; an Excitable point built into an invocation
//!    and fired.
//! 5. Entrain: a vibration arrives when the watched table changes, then
//!    an in-band damp ends the entrainment.
//! 6. Knock: the stranger is denied, knocks, is parked in `_inbox`,
//!    then admission unblocks its queries.
//! 7. Media: a shell-command source streams bytes to a watch call.
//! 8. Outbox: a row enqueued in A's `_outbox` is driven to `done` with
//!    `_results` by the worker `rsntr serve` started.
//! 9. Presence: beacons over the shared-endpoint gossip refresh
//!    `_peers.last_seen` on both sides.

use std::collections::BTreeMap;
use std::time::Duration;

use rsntr::client::{self, EntrainItem, MediaChunk, ProjectionOutcome, QueryOutcome};
use rsntr::serve::start_node_with;
use rsntr::store;
use rsntr::teletype::{Invocation, build_invocation};
use rsntr::testutil::TempDir;

use resonator_protocol::{EnvelopeObject, Knock, PointKind, Value};
use resonator_surfaces::{PresenceConfig, presence::is_stale};
use resonator_transport::{IrohConfig, IrohTransport, RequestStream, Transport, basic_hello};

const TEST_TIMEOUT: Duration = Duration::from_secs(180);
/// Fast beacon cadence so presence propagates within seconds.
const CADENCE: Duration = Duration::from_millis(300);

const DEMO_TURTLE: &str = "\
@prefix ex: <http://example.org/> .
ex:luna ex:name \"moon\" .
ex:sol ex:name \"sun\" .
";

async fn wait_for<F>(what: &str, deadline: Duration, mut cond: F)
where
    F: AsyncFnMut() -> bool,
{
    let start = std::time::Instant::now();
    loop {
        if cond().await {
            return;
        }
        assert!(start.elapsed() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn m4_two_node_offline_demo() {
    tokio::time::timeout(TEST_TIMEOUT, demo())
        .await
        .expect("demo timed out");
}

async fn demo() {
    let ta = TempDir::new("demo-a");
    let tb = TempDir::new("demo-b");
    let tc = TempDir::new("demo-c");
    let a_id = store::init_dir(ta.path()).expect("init a");
    let b_id = store::init_dir(tb.path()).expect("init b");
    let c_id = store::init_dir(tc.path()).expect("init c");
    let a_hex = a_id.to_string();
    let c_hex = c_id.to_string();

    // Seed B before serving: admit A, allow it reads everywhere and
    // writes on `notes`, create the demo table, load the RDF store, and
    // author the projection points. (Admitting A pre-serve also makes
    // both nodes derive the same presence topic at startup.)
    {
        let conn = resonator_node::open_node_db(&store::db_path(tb.path())).expect("open b");
        conn.execute(
            "INSERT INTO _peers (endpoint_id, name, added_at) VALUES (?1, 'a', datetime('now'))",
            [&a_hex],
        )
        .expect("admit a");
        conn.execute_batch(&format!(
            "INSERT INTO _policy (peer_or_group, table_name, action, effect) VALUES
               ('{a_hex}', '*', 'read', 'allow'),
               ('{a_hex}', 'notes', 'write', 'allow');
             CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT);
             INSERT INTO notes (title) VALUES ('hello from b');
             INSERT INTO _projection
               (point_iri, kind, label, modulation, signal, params_order, fields, resource, ord)
             VALUES
               ('urn:demo:add-note', 'excitable', 'add a note', 'sql-sqlite',
                'INSERT INTO notes (title) VALUES (?1)', '[\"title\"]',
                '[{{\"name\":\"title\",\"required\":true}}]', 'notes', 0),
               ('urn:demo:notes-changed', 'sympathetic', 'notes changed',
                NULL, NULL, NULL, NULL, 'notes', 1);"
        ))
        .expect("seed b");
        let loaded: i64 = conn
            .query_row("SELECT rdf_load_turtle(?1)", [DEMO_TURTLE], |r| r.get(0))
            .expect("load turtle");
        assert_eq!(loaded, 2, "demo turtle carries two triples");
        // The media source: a shell command whose stdout is the feed.
        store::media_add(
            tb.path(),
            "feed",
            "printf 'demo media bytes'",
            "text/plain",
            Some("demo"),
        )
        .expect("media add");
        store::media_allow(tb.path(), &a_hex, "feed").expect("media allow");
    }

    let presence_config = || PresenceConfig {
        cadence: CADENCE,
        status: Some("demo".to_string()),
        ..PresenceConfig::default()
    };
    let b = start_node_with(tb.path(), true, presence_config())
        .await
        .expect("serve b");
    assert_eq!(b.peer_id(), b_id);
    let ticket_b = b.ready_ticket(Duration::from_secs(3)).await;

    // A and C learn B via its live ticket; A's `_peers` row is also its
    // admission of B (peers admit each other).
    store::peer_add(ta.path(), "b", &ticket_b, &[]).expect("a: peer add b");
    store::peer_add(tc.path(), "b", &ticket_b, &[]).expect("c: peer add b");
    let a = start_node_with(ta.path(), true, presence_config())
        .await
        .expect("serve a");

    // 1. sql-sqlite SELECT round-trip with a positional parameter.
    let report = client::run_query(
        ta.path(),
        "b",
        "sql-sqlite",
        "SELECT id, title FROM notes WHERE title = ?",
        &["hello from b".to_string()],
        true,
        None,
    )
    .await
    .expect("sql query");
    match &report.outcome {
        QueryOutcome::Rows {
            columns,
            rows,
            done,
        } => {
            assert_eq!(columns, &["id", "title"]);
            assert_eq!(rows.len(), 1);
            assert_eq!(done.row_count, Some(1));
            assert_eq!(
                rows[0]
                    .cells
                    .iter()
                    .find(|(n, _)| n == "title")
                    .map(|(_, v)| v),
                Some(&Value::Text("hello from b".to_string()))
            );
        }
        other => panic!("expected rows, got {other:?}"),
    }

    // 2a. sparql SELECT: cells arrive as N-Triples lexical forms.
    let select = client::run_query(
        ta.path(),
        "b",
        "sparql",
        "SELECT ?name WHERE { ?s <http://example.org/name> ?name } ORDER BY ?name",
        &[],
        true,
        None,
    )
    .await
    .expect("sparql select");
    match &select.outcome {
        QueryOutcome::Rows {
            columns,
            rows,
            done,
        } => {
            assert_eq!(columns, &["name"]);
            assert_eq!(done.row_count, Some(2));
            let names: Vec<&Value> = rows
                .iter()
                .filter_map(|r| r.cells.iter().find(|(n, _)| n == "name").map(|(_, v)| v))
                .collect();
            assert_eq!(
                names,
                vec![
                    &Value::Text("\"moon\"".to_string()),
                    &Value::Text("\"sun\"".to_string())
                ]
            );
        }
        other => panic!("expected sparql rows, got {other:?}"),
    }

    // 2b. sparql CONSTRUCT: the answer is rsntr:Graph frames.
    let construct = client::run_query(
        ta.path(),
        "b",
        "sparql",
        "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }",
        &[],
        true,
        None,
    )
    .await
    .expect("sparql construct");
    match &construct.outcome {
        QueryOutcome::Graph { triples, done } => {
            assert_eq!(triples.len(), 2);
            assert_eq!(done.row_count, Some(2));
            assert!(
                triples.iter().any(|t| t.to_string().contains("\"moon\"")),
                "graph carries the moon triple: {triples:?}"
            );
        }
        other => panic!("expected a graph, got {other:?}"),
    }

    // 3. help.
    let help = client::run_help(ta.path(), "b", None, true)
        .await
        .expect("help");
    match &help.outcome {
        QueryOutcome::Help { text, .. } => {
            assert!(text.contains("resonator node"), "help text: {text:?}");
        }
        other => panic!("expected help, got {other:?}"),
    }

    // 4. Projection listing, then fire the Excitable through the
    //    teletype invocation builder.
    let projection = client::run_projection(ta.path(), "b", "", true)
        .await
        .expect("projection");
    let ProjectionOutcome::Projection(p) = projection else {
        panic!("expected a projection, got {projection:?}");
    };
    assert!(
        p.offers
            .iter()
            .any(|pt| pt.iri == "urn:rsntr:projection-changed"),
        "root projection carries the well-known point"
    );
    assert!(
        p.offers
            .iter()
            .any(|pt| pt.iri == "urn:demo:notes-changed" && pt.kind == PointKind::Sympathetic),
        "offers: {:?}",
        p.offers.iter().map(|pt| &pt.iri).collect::<Vec<_>>()
    );
    let add_note = p
        .offers
        .iter()
        .find(|pt| pt.iri == "urn:demo:add-note")
        .expect("the Excitable is offered (write policy makes it visible)");
    assert_eq!(add_note.kind, PointKind::Excitable);
    let mut values = BTreeMap::new();
    values.insert("title".to_string(), "note from the projection".to_string());
    let invocation = build_invocation(add_note, &values).expect("build invocation");
    let Invocation::Statement {
        kind,
        modulation,
        text,
        params,
    } = invocation
    else {
        panic!("expected a statement invocation, got {invocation:?}");
    };
    let fired = client::run_statement(ta.path(), "b", kind, &modulation, &text, params, true, None)
        .await
        .expect("fire excitable");
    assert!(
        matches!(&fired.outcome, QueryOutcome::Rows { .. }),
        "expected the excitable write to complete, got {:?}",
        fired.outcome
    );
    let note_count: i64 = b
        .node()
        .db()
        .call(|conn| {
            conn.query_row(
                "SELECT count(*) FROM notes WHERE title = 'note from the projection'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        })
        .await
        .expect("count notes");
    assert_eq!(note_count, 1, "the Excitable invocation landed");

    // 5. Entrain the Sympathetic point; a server-side write to the
    //    watched table vibrates it; an in-band damp ends it.
    let (ent_tx, mut ent_rx) = tokio::sync::mpsc::channel(16);
    let (damp_tx, damp_rx) = tokio::sync::oneshot::channel();
    let entrain_task = tokio::spawn(client::run_entrain(
        ta.path().to_path_buf(),
        "b".to_string(),
        "urn:demo:notes-changed".to_string(),
        true,
        ent_tx,
        damp_rx,
    ));
    match ent_rx.recv().await {
        Some(EntrainItem::Entrained) => {}
        other => panic!("expected the entrained ack, got {other:?}"),
    }
    b.node()
        .db()
        .call(|conn| {
            conn.execute("INSERT INTO notes (title) VALUES ('vibration trigger')", [])
                .unwrap();
        })
        .await
        .expect("trigger write");
    match ent_rx.recv().await {
        Some(EntrainItem::Vibration(v)) => assert_eq!(v.point, "urn:demo:notes-changed"),
        other => panic!("expected a vibration, got {other:?}"),
    }
    damp_tx.send(()).expect("fire damp");
    loop {
        match ent_rx.recv().await {
            // A coalesced second vibration may still arrive before the
            // confirming Done.
            Some(EntrainItem::Vibration(_)) => continue,
            Some(EntrainItem::Damped) => break,
            other => panic!("expected the damp confirmation, got {other:?}"),
        }
    }
    entrain_task
        .await
        .expect("join entrain")
        .expect("entrain clean end");

    // 6. The stranger: denied, knocks, parks in _inbox, is admitted,
    //    then queries fine.
    let denied = client::run_query(
        tc.path(),
        "b",
        "sql-sqlite",
        "SELECT title FROM notes",
        &[],
        true,
        None,
    )
    .await
    .expect("stranger query completes the protocol");
    match &denied.outcome {
        QueryOutcome::Denied(d) => {
            assert!(d.reason.as_deref().unwrap_or("").contains("unknown peer"));
        }
        other => panic!("expected the stranger to be denied, got {other:?}"),
    }
    // Knock over a raw transport bound with C's key.
    {
        let hello = basic_hello(&["help"], None);
        let config = IrohConfig {
            hello,
            secret_key: Some(store::load_secret(tc.path()).expect("c key")),
            offline: true,
            relays: Vec::new(),
            gossip: false,
            blobs: None,
        };
        let (client_t, _rx) = IrohTransport::bind(config).await.expect("bind c");
        client_t.add_peer_addrs(b.peer_id(), b.direct_addrs());
        let (mut stream, _hello) = client_t.open(b.peer_id()).await.expect("open knock");
        stream
            .send(&EnvelopeObject::Knock(Knock {
                id: None,
                message: "stranger c asking in".to_string(),
            }))
            .await
            .expect("send knock");
        stream.finish().await.expect("finish");
        match stream.recv().await.expect("recv decision") {
            Some(EnvelopeObject::Decision(d)) => {
                assert_eq!(d.decision, "pending", "no policy tier decides: parked");
            }
            other => panic!("expected a Decision, got {other:?}"),
        }
        client_t.shutdown().await;
    }
    let parked: (String, String) = b
        .node()
        .db()
        .call(|conn| {
            conn.query_row(
                "SELECT peer, params FROM _inbox WHERE decision IS NULL",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("one parked knock")
        })
        .await
        .expect("read _inbox");
    assert_eq!(parked.0, c_hex);
    assert!(parked.1.contains("stranger c asking in"));
    // The owner admits C; the same query now round-trips.
    b.node()
        .db()
        .call({
            let c_hex = c_hex.clone();
            move |conn| {
                conn.execute(
                    "INSERT INTO _peers (endpoint_id, name, added_at) \
                     VALUES (?1, 'c', datetime('now'))",
                    [&c_hex],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
                     VALUES (?1, 'notes', 'read', 'allow')",
                    [&c_hex],
                )
                .unwrap();
            }
        })
        .await
        .expect("admit c");
    let unblocked = client::run_query(
        tc.path(),
        "b",
        "sql-sqlite",
        "SELECT count(*) AS n FROM notes",
        &[],
        true,
        None,
    )
    .await
    .expect("admitted stranger query");
    match &unblocked.outcome {
        QueryOutcome::Rows { rows, .. } => {
            assert_eq!(rows[0].cells[0], ("n".to_string(), Value::Integer(3)));
        }
        other => panic!("expected rows after admission, got {other:?}"),
    }

    // 7. Media: the registered shell command streams its stdout to a
    //    watch call.
    let (media_tx, mut media_rx) = tokio::sync::mpsc::channel(16);
    client::run_media_channel(ta.path(), "b", "feed", true, media_tx)
        .await
        .expect("media watch");
    match media_rx.recv().await {
        Some(MediaChunk::Header { content_type }) => assert_eq!(content_type, "text/plain"),
        other => panic!("expected the media header, got {other:?}"),
    }
    let mut feed = Vec::new();
    while let Some(chunk) = media_rx.recv().await {
        match chunk {
            MediaChunk::Data(bytes) => feed.extend_from_slice(&bytes),
            other => panic!("expected raw data, got {other:?}"),
        }
    }
    assert_eq!(feed, b"demo media bytes");

    // 8. Outbox: enqueue on A's serving connection; the worker started
    //    by serve drives it to done and lands _results.
    let request_id = a
        .node()
        .db()
        .call(|conn| {
            resonator_surfaces::enqueue(
                conn,
                "b",
                "sql-sqlite",
                "SELECT title FROM notes WHERE id = 1",
                "[]",
            )
            .expect("enqueue")
        })
        .await
        .expect("enqueue call");
    wait_for(
        "the outbox row to complete",
        Duration::from_secs(30),
        async || {
            let rid = request_id.clone();
            a.node()
                .db()
                .call(move |conn| {
                    conn.query_row(
                        "SELECT status FROM _outbox WHERE request_id = ?1",
                        [&rid],
                        |r| r.get::<_, String>(0),
                    )
                    .unwrap()
                })
                .await
                .expect("outbox status")
                == "done"
        },
    )
    .await;
    let result_row: String = a
        .node()
        .db()
        .call({
            let rid = request_id.clone();
            move |conn| {
                conn.query_row(
                    "SELECT row FROM _results WHERE request_id = ?1 AND row_no = 0",
                    [&rid],
                    |r| r.get(0),
                )
                .unwrap()
            }
        })
        .await
        .expect("read _results");
    assert_eq!(result_row, "[\"hello from b\"]");

    // 9. Presence: beacons over the shared endpoint refresh last_seen on
    //    both sides.
    let b_id_copy = b_id;
    let a_id_copy = a_id;
    wait_for(
        "presence to propagate",
        Duration::from_secs(30),
        async || {
            let a_sees_b = a
                .node()
                .db()
                .call(move |conn| !is_stale(conn, &b_id_copy, Duration::from_secs(10)).unwrap())
                .await
                .expect("a is_stale");
            let b_sees_a = b
                .node()
                .db()
                .call(move |conn| !is_stale(conn, &a_id_copy, Duration::from_secs(10)).unwrap())
                .await
                .expect("b is_stale");
            a_sees_b && b_sees_a
        },
    )
    .await;

    a.shutdown().await;
    b.shutdown().await;
}
