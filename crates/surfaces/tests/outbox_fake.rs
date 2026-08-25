//! Outbox worker state machine, fully offline: a fake [`Transport`] plays
//! the serving peer with scripted response frames.
//!
//! Covered:
//!
//! - a happy read streams rows into `_results` and flips the row to `done`;
//! - a denial / an error surfaces its reason and the right status;
//! - an unreachable peer keeps the row `queued` without a busy loop;
//! - a truncated response leaves the row `sent` and the retry carries the
//!   same wire request id (idempotency);
//! - a row already `sent` at startup is resumed (restart safety);
//! - an over-age row expires;
//! - sparql CONSTRUCT graph frames land in `_results` as N-Triples JSON.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use resonator_node::DbHandle;
use resonator_protocol::{
    Done, EnvelopeObject, ErrorEnvelope, Graph, Hello, ResultHeader, Row, Value,
};
use resonator_surfaces::outbox::{OutboxConfig, OutboxWorker, enqueue};
use resonator_transport::{PeerId, RequestStream, Transport, TransportError, basic_hello};

// ---------------------------------------------------------------------------
// Fake transport
// ---------------------------------------------------------------------------

/// Scripted responder: maps one request frame to its response frames.
type Handler = dyn Fn(&EnvelopeObject) -> Vec<EnvelopeObject> + Send + Sync;

struct FakeTransport {
    local: PeerId,
    /// The one peer this transport can reach.
    serve: PeerId,
    handler: Arc<Handler>,
    opens: AtomicU64,
    /// Every request frame the fake server received, in order.
    requests: Arc<Mutex<Vec<EnvelopeObject>>>,
}

impl FakeTransport {
    fn new(serve: PeerId, handler: Arc<Handler>) -> Arc<Self> {
        Arc::new(Self {
            local: PeerId([0xAA; 32]),
            serve,
            handler,
            opens: AtomicU64::new(0),
            requests: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn open_count(&self) -> u64 {
        self.opens.load(Ordering::Relaxed)
    }

    fn received(&self) -> Vec<EnvelopeObject> {
        self.requests.lock().expect("requests lock").clone()
    }
}

struct FakeStream {
    handler: Arc<Handler>,
    requests: Arc<Mutex<Vec<EnvelopeObject>>>,
    queue: VecDeque<EnvelopeObject>,
}

impl RequestStream for FakeStream {
    async fn send(&mut self, obj: &EnvelopeObject) -> Result<(), TransportError> {
        self.requests
            .lock()
            .expect("requests lock")
            .push(obj.clone());
        self.queue.extend((self.handler)(obj));
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<EnvelopeObject>, TransportError> {
        Ok(self.queue.pop_front())
    }

    async fn finish(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

impl Transport for FakeTransport {
    type Stream = FakeStream;

    fn local_id(&self) -> PeerId {
        self.local
    }

    async fn open(&self, peer: PeerId) -> Result<(FakeStream, Hello), TransportError> {
        self.opens.fetch_add(1, Ordering::Relaxed);
        if peer != self.serve {
            return Err(TransportError::Dial(format!("no route to {peer}")));
        }
        Ok((
            FakeStream {
                handler: self.handler.clone(),
                requests: self.requests.clone(),
                queue: VecDeque::new(),
            },
            basic_hello(&["help", "sql-sqlite", "sparql"], None),
        ))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn serve_id() -> PeerId {
    PeerId([0x5E; 32])
}

fn request_id_of(obj: &EnvelopeObject) -> String {
    match obj {
        EnvelopeObject::Query(st) | EnvelopeObject::Execute(st) => st.id.clone(),
        other => panic!("expected a statement frame, got {other:?}"),
    }
}

fn client_db() -> DbHandle {
    let conn = Connection::open_in_memory().expect("open db");
    // _peers so petname resolution has its table; not strictly required.
    conn.execute_batch(resonator_node::PEERS_DDL).expect("ddl");
    DbHandle::spawn(conn)
}

fn fast_config() -> OutboxConfig {
    OutboxConfig {
        poll_interval: Duration::from_millis(150),
        base_backoff: Duration::from_millis(80),
        max_backoff: Duration::from_millis(400),
        expiry: Duration::from_secs(3600),
        open_timeout: Duration::from_millis(500),
        install_update_hook: true,
    }
}

async fn wait_status(db: &DbHandle, rid: &str, want: &str, dur: Duration) -> String {
    let deadline = Instant::now() + dur;
    let mut last = String::new();
    loop {
        let rid_owned = rid.to_string();
        let s: Option<String> = db
            .call(move |conn| {
                conn.query_row(
                    "SELECT status FROM _outbox WHERE request_id = ?1",
                    [&rid_owned],
                    |r| r.get(0),
                )
                .ok()
            })
            .await
            .ok()
            .flatten();
        if let Some(s) = s {
            last = s.clone();
            if s == want {
                return s;
            }
        }
        if Instant::now() >= deadline {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn results_of(db: &DbHandle, rid: &str) -> Vec<String> {
    let rid = rid.to_string();
    db.call(move |conn| {
        let mut stmt = conn
            .prepare("SELECT row FROM _results WHERE request_id = ?1 ORDER BY row_no")
            .expect("prepare");
        stmt.query_map([&rid], |r| r.get::<_, String>(0))
            .expect("query")
            .filter_map(Result::ok)
            .collect()
    })
    .await
    .expect("db call")
}

async fn error_of(db: &DbHandle, rid: &str) -> Option<String> {
    let rid = rid.to_string();
    db.call(move |conn| {
        conn.query_row(
            "SELECT error FROM _outbox WHERE request_id = ?1",
            [&rid],
            |r| r.get::<_, Option<String>>(0),
        )
        .expect("row present")
    })
    .await
    .expect("db call")
}

/// A scripted happy SELECT answer: header, one row batch, done.
fn rows_response(req: &EnvelopeObject) -> Vec<EnvelopeObject> {
    let id = request_id_of(req);
    vec![
        EnvelopeObject::Result(ResultHeader {
            id: id.clone(),
            columns: vec!["id".to_string(), "body".to_string()],
            decl_types: vec![],
        }),
        EnvelopeObject::Row(vec![
            Row {
                seq: 0,
                cells: vec![
                    ("id".to_string(), Value::Integer(1)),
                    ("body".to_string(), Value::Text("over the wire".into())),
                ],
            },
            Row {
                seq: 1,
                cells: vec![("id".to_string(), Value::Integer(2))],
            },
        ]),
        EnvelopeObject::Done(Done {
            id,
            row_count: Some(2),
            affected_rows: None,
            last_insert_rowid: None,
            truncated: false,
        }),
    ]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn happy_read_streams_results_and_marks_done() {
    let transport = FakeTransport::new(serve_id(), Arc::new(rows_response));
    let db = client_db();
    let handle = OutboxWorker::spawn(db.clone(), transport.clone(), fast_config())
        .await
        .expect("spawn");

    let peer = serve_id().to_string();
    let rid = db
        .call(move |conn| {
            enqueue(
                conn,
                &peer,
                "sql-sqlite",
                "SELECT id, body FROM notes",
                "[]",
            )
        })
        .await
        .expect("db call")
        .expect("enqueue");

    assert_eq!(
        wait_status(&db, &rid, "done", Duration::from_secs(5)).await,
        "done"
    );
    let rows = results_of(&db, &rid).await;
    assert_eq!(rows, vec![r#"[1,"over the wire"]"#, r#"[2,null]"#]);
    assert!(error_of(&db, &rid).await.is_none());

    // The wire id equals the stored ULID.
    let reqs = transport.received();
    assert_eq!(reqs.len(), 1);
    assert_eq!(request_id_of(&reqs[0]), rid);

    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn denied_and_error_are_terminal_with_reasons() {
    let handler: Arc<Handler> = Arc::new(|req: &EnvelopeObject| {
        let id = request_id_of(req);
        let signal = match req {
            EnvelopeObject::Query(st) | EnvelopeObject::Execute(st) => st.signal.clone(),
            _ => String::new(),
        };
        if signal.contains("secret") {
            vec![EnvelopeObject::Denied(resonator_protocol::Denied {
                id: Some(id),
                reason: Some("not for you".to_string()),
            })]
        } else {
            vec![EnvelopeObject::Error(ErrorEnvelope {
                id: Some(id),
                code: "engine-error".to_string(),
                reason: Some("no such table".to_string()),
            })]
        }
    });
    let transport = FakeTransport::new(serve_id(), handler);
    let db = client_db();
    let handle = OutboxWorker::spawn(db.clone(), transport.clone(), fast_config())
        .await
        .expect("spawn");

    let peer = serve_id().to_string();
    let (rid_denied, rid_error) = db
        .call(move |conn| {
            let a = enqueue(conn, &peer, "sql-sqlite", "SELECT * FROM secret", "[]")?;
            let b = enqueue(conn, &peer, "sql-sqlite", "SELECT * FROM missing", "[]")?;
            Ok::<_, rusqlite::Error>((a, b))
        })
        .await
        .expect("db call")
        .expect("enqueue");

    assert_eq!(
        wait_status(&db, &rid_denied, "denied", Duration::from_secs(5)).await,
        "denied"
    );
    assert_eq!(
        error_of(&db, &rid_denied).await.as_deref(),
        Some("not for you")
    );

    assert_eq!(
        wait_status(&db, &rid_error, "error", Duration::from_secs(5)).await,
        "error"
    );
    let err = error_of(&db, &rid_error).await.expect("error text");
    assert!(err.contains("engine-error"), "got {err:?}");
    assert!(err.contains("no such table"), "got {err:?}");

    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreachable_peer_stays_queued_without_busy_loop() {
    let transport = FakeTransport::new(serve_id(), Arc::new(rows_response));
    let db = client_db();
    let handle = OutboxWorker::spawn(db.clone(), transport.clone(), fast_config())
        .await
        .expect("spawn");

    // A valid 64-hex id the fake cannot route to.
    let nowhere = PeerId([0x77; 32]).to_string();
    let rid = db
        .call(move |conn| enqueue(conn, &nowhere, "sql-sqlite", "SELECT 1", "[]"))
        .await
        .expect("db call")
        .expect("enqueue");

    // Give the worker time to try and back off repeatedly.
    tokio::time::sleep(Duration::from_millis(1200)).await;

    assert_eq!(
        wait_status(&db, &rid, "queued", Duration::from_millis(1)).await,
        "queued",
        "offline row must stay queued"
    );
    let opens = transport.open_count();
    assert!(opens >= 1, "worker should have attempted at least one dial");
    assert!(
        opens < 40,
        "runaway dialing ({opens}) indicates a busy loop"
    );

    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn truncated_response_retries_with_same_wire_id() {
    // First attempt: header only, no terminal frame (truncated). Second
    // attempt onward: the full response.
    let calls = Arc::new(AtomicU64::new(0));
    let calls_h = calls.clone();
    let handler: Arc<Handler> = Arc::new(move |req: &EnvelopeObject| {
        let n = calls_h.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            let id = request_id_of(req);
            vec![EnvelopeObject::Result(ResultHeader {
                id,
                columns: vec!["x".to_string()],
                decl_types: vec![],
            })]
        } else {
            rows_response(req)
        }
    });
    let transport = FakeTransport::new(serve_id(), handler);
    let db = client_db();
    let handle = OutboxWorker::spawn(db.clone(), transport.clone(), fast_config())
        .await
        .expect("spawn");

    let peer = serve_id().to_string();
    let rid = db
        .call(move |conn| enqueue(conn, &peer, "sql-sqlite", "SELECT 1", "[]"))
        .await
        .expect("db call")
        .expect("enqueue");

    assert_eq!(
        wait_status(&db, &rid, "done", Duration::from_secs(5)).await,
        "done"
    );

    // The resend carried the identical wire id: the serving node's _applied
    // idempotency would answer it without re-applying.
    let reqs = transport.received();
    assert!(
        reqs.len() >= 2,
        "expected a resend, got {} sends",
        reqs.len()
    );
    let ids: Vec<String> = reqs.iter().map(request_id_of).collect();
    assert!(ids.iter().all(|i| i == &rid), "wire ids drifted: {ids:?}");

    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sent_row_is_resumed_after_restart() {
    let transport = FakeTransport::new(serve_id(), Arc::new(rows_response));
    let db = client_db();

    // Simulate a pre-crash state: the row is already 'sent' before any
    // worker runs (tables ensured manually, as a previous process would
    // have).
    let peer = serve_id().to_string();
    let rid = db
        .call(move |conn| {
            resonator_surfaces::ensure_outbox_tables(conn)?;
            let rid = enqueue(conn, &peer, "sql-sqlite", "SELECT 1", "[]")?;
            conn.execute(
                "UPDATE _outbox SET status = 'sent' WHERE request_id = ?1",
                [&rid],
            )?;
            Ok::<_, rusqlite::Error>(rid)
        })
        .await
        .expect("db call")
        .expect("setup");

    let handle = OutboxWorker::spawn(db.clone(), transport.clone(), fast_config())
        .await
        .expect("spawn");

    assert_eq!(
        wait_status(&db, &rid, "done", Duration::from_secs(5)).await,
        "done"
    );
    let reqs = transport.received();
    assert_eq!(request_id_of(&reqs[0]), rid);

    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn over_age_row_expires() {
    let transport = FakeTransport::new(serve_id(), Arc::new(rows_response));
    let db = client_db();

    let peer = serve_id().to_string();
    let rid = db
        .call(move |conn| {
            resonator_surfaces::ensure_outbox_tables(conn)?;
            let rid = enqueue(conn, &peer, "sql-sqlite", "SELECT 1", "[]")?;
            conn.execute(
                "UPDATE _outbox SET created_at = datetime('now', '-2 hours') \
                 WHERE request_id = ?1",
                [&rid],
            )?;
            Ok::<_, rusqlite::Error>(rid)
        })
        .await
        .expect("db call")
        .expect("setup");

    let handle = OutboxWorker::spawn(db.clone(), transport.clone(), fast_config())
        .await
        .expect("spawn");

    assert_eq!(
        wait_status(&db, &rid, "expired", Duration::from_secs(5)).await,
        "expired"
    );
    // Nothing was ever sent for it.
    assert!(transport.received().is_empty());

    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sparql_construct_graphs_land_as_ntriples_json() {
    use oxrdf::{Literal, NamedNode, Triple};

    let handler: Arc<Handler> = Arc::new(|req: &EnvelopeObject| {
        let id = request_id_of(req);
        let triple = Triple::new(
            NamedNode::new("http://example.org/s").unwrap(),
            NamedNode::new("http://example.org/p").unwrap(),
            Literal::new_simple_literal("o"),
        );
        vec![
            EnvelopeObject::Graph(Graph {
                id: id.clone(),
                seq: 0,
                payload: vec![triple],
            }),
            EnvelopeObject::Done(Done {
                id,
                row_count: Some(1),
                affected_rows: None,
                last_insert_rowid: None,
                truncated: false,
            }),
        ]
    });
    let transport = FakeTransport::new(serve_id(), handler);
    let db = client_db();
    let handle = OutboxWorker::spawn(db.clone(), transport.clone(), fast_config())
        .await
        .expect("spawn");

    let peer = serve_id().to_string();
    let rid = db
        .call(move |conn| {
            enqueue(
                conn,
                &peer,
                "sparql",
                "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }",
                "[]",
            )
        })
        .await
        .expect("db call")
        .expect("enqueue");

    assert_eq!(
        wait_status(&db, &rid, "done", Duration::from_secs(5)).await,
        "done"
    );
    let rows = results_of(&db, &rid).await;
    assert_eq!(rows.len(), 1);
    let parsed: Vec<String> = serde_json::from_str(&rows[0]).expect("json");
    assert_eq!(
        parsed,
        vec!["<http://example.org/s> <http://example.org/p> \"o\" ."]
    );

    // A CONSTRUCT was labelled a Query on the wire.
    let reqs = transport.received();
    assert!(matches!(reqs[0], EnvelopeObject::Query(_)));

    handle.shutdown().await;
}
