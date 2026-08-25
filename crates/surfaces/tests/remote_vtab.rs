//! The SQL vtab surfaces over two in-process offline nodes: a serving
//! node behind a real (offline) iroh transport, and a local node
//! connection with `remote_query`/`iroh_remote` registered over a client
//! transport.
//!
//! Covered:
//!
//! - remote_query returns the peer's rows (JSON row/cells columns), with
//!   typed parameter pushdown;
//! - iroh_remote SELECT (predicate pushed down to the peer) + INSERT/
//!   UPDATE/DELETE round trip under an allow policy;
//! - a denied statement surfaces as a SQL error naming the denial;
//! - the timeout path (a transport that never answers).

use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection;

use resonator_authenticator::Chain;
use resonator_node::{
    DbHandle, Node, NodeConfig, node_hello, open_node_db_in_memory, seed_rsntr_defaults,
};
use resonator_protocol::{EnvelopeObject, Hello};
use resonator_surfaces::{RemoteContext, register_remote_vtabs};
use resonator_transport::{
    IrohConfig, IrohTransport, PeerId, RequestStream, Transport, TransportError, basic_hello,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(60);

/// A serving node with a notes table: reads and writes on `notes` are
/// allowed for everyone, `journal` has no policy rows (tail deny).
async fn serving_node(admit: PeerId) -> (Arc<Node>, Arc<IrohTransport>, PeerId) {
    let conn = open_node_db_in_memory().expect("open server db");
    seed_rsntr_defaults(&conn).expect("seed");
    conn.execute_batch(&format!(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT, n INTEGER);
         INSERT INTO notes (body, n) VALUES ('alpha', 1), ('beta', 2), ('gamma', 3);
         CREATE TABLE journal (id INTEGER PRIMARY KEY, entry TEXT);
         INSERT INTO _peers (endpoint_id, name, added_at) VALUES ('{admit}', 'tester', datetime('now'));
         INSERT INTO _policy (peer_or_group, table_name, action, effect) VALUES
           ('*', 'notes', 'read', 'allow'),
           ('*', 'notes', 'write', 'allow');",
    ))
    .expect("fixture");
    let hello = node_hello(&conn);
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
    (node, server, server_id)
}

#[tokio::test(flavor = "multi_thread")]
async fn vtab_select_write_and_denied_round_trips() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (client, _client_rx) =
            IrohTransport::bind(IrohConfig::offline(basic_hello(&["help"], None)))
                .await
                .expect("bind client");
        let (node, server, server_id) = serving_node(client.peer_id()).await;
        client.add_peer_addrs(server_id, server.direct_addrs());

        let ctx = Arc::new(
            RemoteContext::new(client.clone(), tokio::runtime::Handle::current())
                .with_timeout(Duration::from_secs(20)),
        );

        // The local side is an ordinary node connection with the vtabs
        // registered (what the owner channel queries when serving).
        let conn = open_node_db_in_memory().expect("open local db");
        register_remote_vtabs(&conn, ctx).expect("register");

        let peer_hex = server_id.to_string();
        // The vtab blocks its calling thread on the network round trip,
        // so drive all SQL from a blocking thread, never a runtime worker.
        let conn = tokio::task::spawn_blocking(move || {
            // 1. remote_query: rows come back as JSON.
            let rows: Vec<(String, String, String)> = conn
                .prepare(
                    "SELECT row, cells, columns \
                     FROM remote_query(?1, 'SELECT id, body FROM notes ORDER BY id')",
                )
                .unwrap()
                .query_map([&peer_hex], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].0, r#"[1,"alpha"]"#);
            assert_eq!(rows[0].2, r#"["id","body"]"#);
            let cells: serde_json::Value = serde_json::from_str(&rows[2].1).unwrap();
            assert_eq!(cells["body"], "gamma");

            // 2. remote_query with a typed positional parameter.
            let body: String = conn
                .query_row(
                    "SELECT cells ->> 'body' \
                     FROM remote_query(?1, 'SELECT body FROM notes WHERE n = ?1', 2)",
                    [&peer_hex],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(body, "beta");

            // 3. remote_query against an unpolicied table is denied.
            let err = conn
                .query_row(
                    "SELECT row FROM remote_query(?1, 'SELECT entry FROM journal')",
                    [&peer_hex],
                    |r| r.get::<_, String>(0),
                )
                .unwrap_err()
                .to_string();
            assert!(err.contains("denied"), "unexpected error: {err}");

            // 4. iroh_remote: schema probed from the peer at CREATE.
            conn.execute_batch(&format!(
                "CREATE VIRTUAL TABLE rnotes USING iroh_remote(peer='{peer_hex}', table=notes)"
            ))
            .unwrap();

            // SELECT with a pushed-down predicate.
            let bodies: Vec<String> = conn
                .prepare("SELECT body FROM rnotes WHERE n > 1 ORDER BY id")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(bodies, vec!["beta", "gamma"]);

            // INSERT / UPDATE / DELETE map to remote Execute statements.
            conn.execute("INSERT INTO rnotes (body, n) VALUES ('delta', 4)", [])
                .unwrap();
            assert_eq!(conn.last_insert_rowid(), 4);
            conn.execute("UPDATE rnotes SET body = 'beta2' WHERE id = 2", [])
                .unwrap();
            conn.execute("DELETE FROM rnotes WHERE id = 1", []).unwrap();

            // The vtab now reflects the writes.
            let all: Vec<(i64, String)> = conn
                .prepare("SELECT id, body FROM rnotes ORDER BY id")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(
                all,
                vec![
                    (2, "beta2".to_string()),
                    (3, "gamma".to_string()),
                    (4, "delta".to_string())
                ]
            );

            // 5. CREATE over an unpolicied table fails at the schema probe.
            let err = conn
                .execute_batch(&format!(
                    "CREATE VIRTUAL TABLE rjournal USING iroh_remote(peer='{peer_hex}', table=journal)"
                ))
                .unwrap_err()
                .to_string();
            assert!(err.contains("denied"), "unexpected error: {err}");
            conn
        })
        .await
        .expect("local SQL thread");
        drop(conn);

        // The serving database really changed (the writes were remote).
        let state: Vec<(i64, String)> = node
            .db()
            .call(|conn| {
                conn.prepare("SELECT id, body FROM notes ORDER BY id")
                    .unwrap()
                    .query_map([], |r| Ok((r.get(0).unwrap(), r.get(1).unwrap())))
                    .unwrap()
                    .collect::<Result<_, _>>()
                    .unwrap()
            })
            .await
            .expect("server state");
        assert_eq!(
            state,
            vec![
                (2, "beta2".to_string()),
                (3, "gamma".to_string()),
                (4, "delta".to_string())
            ]
        );

        // The pushed-down predicate reached the peer as SQL (the audit
        // ledger records the deparsed signal).
        let pushed: i64 = node
            .db()
            .call(|conn| {
                conn.query_row(
                    "SELECT count(*) FROM _audit WHERE signal LIKE '%\"n\" > ?%'",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
            })
            .await
            .expect("audit read");
        assert!(pushed >= 1, "expected the deparsed WHERE clause in _audit");

        server.shutdown().await;
        client.shutdown().await;
    })
    .await
    .expect("test timed out");
}

// ---------------------------------------------------------------------------
// Timeout path: a transport that never answers.
// ---------------------------------------------------------------------------

struct NeverStream;

impl RequestStream for NeverStream {
    async fn send(&mut self, _obj: &EnvelopeObject) -> Result<(), TransportError> {
        unreachable!("NeverTransport never yields a stream")
    }
    async fn recv(&mut self) -> Result<Option<EnvelopeObject>, TransportError> {
        unreachable!("NeverTransport never yields a stream")
    }
    async fn finish(&mut self) -> Result<(), TransportError> {
        unreachable!("NeverTransport never yields a stream")
    }
}

/// `open` hangs forever: the peer is a black hole.
struct NeverTransport;

impl Transport for NeverTransport {
    type Stream = NeverStream;

    fn local_id(&self) -> PeerId {
        PeerId([0xAA; 32])
    }

    async fn open(&self, _peer: PeerId) -> Result<(NeverStream, Hello), TransportError> {
        std::future::pending().await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_query_times_out_against_a_black_hole() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let ctx = Arc::new(
            RemoteContext::new(Arc::new(NeverTransport), tokio::runtime::Handle::current())
                .with_timeout(Duration::from_millis(250)),
        );
        let conn = Connection::open_in_memory().expect("open");
        register_remote_vtabs(&conn, ctx).expect("register");

        let err = tokio::task::spawn_blocking(move || {
            conn.query_row(
                &format!(
                    "SELECT row FROM remote_query('{}', 'SELECT 1')",
                    "ab".repeat(32)
                ),
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap_err()
            .to_string()
        })
        .await
        .expect("local SQL thread");
        assert!(err.contains("timed out"), "unexpected error: {err}");
    })
    .await
    .expect("test timed out");
}
