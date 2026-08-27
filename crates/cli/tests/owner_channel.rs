//! Owner-channel CLI integration (docs/owner-channel.md): the local
//! commands generate ordinary envelopes and, while a node serves, ride
//! the control socket onto the serving connection. Proves the two
//! recorded findings stay fixed from the command layer: socket-delivered
//! writes vibrate Sympathetic points in the serving process, and the
//! peer registry is live for the serving node without a restart.
#![cfg(unix)]

use std::path::Path;
use std::time::Duration;

use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use resonator_protocol::EnvelopeObject;
use rsntr::channel::{OwnerChannel, Prefer};
use rsntr::testutil::TempDir;
use rsntr::{chat, csvcmd, serve, store};

const IO_TIMEOUT: Duration = Duration::from_secs(20);

async fn send_frame(conn: &mut UnixStream, obj: &EnvelopeObject) {
    let mut out = BytesMut::new();
    resonator_protocol::encode_envelope(obj, &mut out).expect("encode frame");
    tokio::time::timeout(IO_TIMEOUT, conn.write_all(&out))
        .await
        .expect("write timed out")
        .expect("write frame");
}

async fn read_frame(conn: &mut UnixStream, buf: &mut BytesMut) -> Option<EnvelopeObject> {
    loop {
        if let Some(obj) = resonator_protocol::decode_envelope(buf).expect("decode frame") {
            return Some(obj);
        }
        let n = tokio::time::timeout(IO_TIMEOUT, conn.read_buf(buf))
            .await
            .expect("read timed out")
            .expect("read");
        if n == 0 {
            assert!(buf.is_empty(), "connection closed mid-frame");
            return None;
        }
    }
}

/// The count of `_audit` rows on the serving node matching a signal
/// prefix with the owner-channel stamp.
async fn local_audit_count(node: &serve::RunningNode, signal_prefix: &str) -> i64 {
    let like = format!("{signal_prefix}%");
    node.node()
        .db()
        .call(move |conn| {
            conn.query_row(
                "SELECT count(*) FROM _audit WHERE signal LIKE ?1 \
                 AND direction = 'local' AND decided_by = 'owner' AND decision = 'allow'",
                [&like],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
        })
        .await
        .unwrap()
}

/// Polls the node dir's database until `f` holds.
async fn wait_db(
    dir: &Path,
    what: &str,
    f: impl Fn(&rusqlite::Connection) -> bool + Send + 'static,
) {
    let dir = dir.to_path_buf();
    let what = what.to_string();
    tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let conn = store::open_db(&dir).expect("open db");
            if f(&conn) {
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!("timed out waiting for {what}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    })
    .await
    .expect("wait task");
}

/// peer add rides the live socket: the upsert lands in `_peers` audited
/// as an owner op, and the serving node dials the new peer immediately,
/// woken by the socket-delivered `_outbox` insert alone (no wake call,
/// no restart).
#[tokio::test(flavor = "multi_thread")]
async fn peer_add_over_socket_is_audited_and_immediately_dialable() {
    let ta = TempDir::new("och-peer-a");
    let tc = TempDir::new("och-peer-c");
    let a_id = store::init_dir(ta.path()).expect("init a");
    store::init_dir(tc.path()).expect("init c");
    chat::chat_init(ta.path()).expect("chat init a");
    chat::chat_init(tc.path()).expect("chat init c");

    let c = serve::start_node(tc.path(), true).await.expect("serve c");
    {
        let a_hex = a_id.to_string();
        c.node()
            .db()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO _peers (endpoint_id, added_at) \
                     VALUES (?1, datetime('now'))",
                    [&a_hex],
                )
                .expect("admit a");
            })
            .await
            .expect("db call");
    }
    let a = serve::start_node(ta.path(), true).await.expect("serve a");

    // Auto selects the socket while a node serves the directory.
    let ch = OwnerChannel::open(ta.path(), Prefer::Auto)
        .await
        .expect("open channel");
    assert!(ch.is_socket(), "a serving directory selects the socket");
    drop(ch);

    // The command under test: a live-socket peer add with C's ticket.
    let ticket = c.ready_ticket(Duration::from_secs(3)).await;
    let (c_peer, addrs) = store::peer_add(ta.path(), "c", &ticket, &[]).expect("peer add");
    assert!(!addrs.is_empty(), "the ticket carries dial addresses");

    // The upsert executed on the serving connection and is on the ledger.
    assert_eq!(local_audit_count(&a, "INSERT INTO _peers").await, 1);
    let stored: i64 = {
        let hex = c_peer.to_string();
        a.node()
            .db()
            .call(move |conn| {
                conn.query_row(
                    "SELECT count(*) FROM _peers WHERE endpoint_id = ?1 AND name = 'c'",
                    [&hex],
                    |r| r.get(0),
                )
                .unwrap()
            })
            .await
            .unwrap()
    };
    assert_eq!(stored, 1);

    // Immediate dial: the send's socket-delivered _outbox insert wakes
    // the outbox worker through the table observer, and the dial reads
    // the fresh _peers row. No wake_outbox, no restart.
    let report = chat::chat_send(ta.path(), "c", "ping over the owner channel", None)
        .await
        .expect("send queues");
    let rid = report.message_id.clone();
    wait_db(ta.path(), "delivery marked done", move |conn| {
        conn.query_row(
            "SELECT count(*) FROM _outbox WHERE request_id = ?1 AND status = 'done'",
            [rid.as_str()],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
            == 1
    })
    .await;
    let rid = report.message_id.clone();
    wait_db(tc.path(), "message landed on c", move |conn| {
        conn.query_row(
            "SELECT count(*) FROM chat_messages WHERE id = ?1",
            [rid.as_str()],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
            == 1
    })
    .await;

    a.shutdown().await;
    c.shutdown().await;
}

/// csv import rides the owner channel: three rows through one chunked
/// Execute (plus the --create DDL), all stamped local/owner on the
/// serving node's ledger.
#[tokio::test(flavor = "multi_thread")]
async fn csv_import_three_rows_through_the_owner_channel() {
    let tmp = TempDir::new("och-csv");
    store::init_dir(tmp.path()).expect("init");
    let node = serve::start_node(tmp.path(), true).await.expect("serve");

    let doc = "name,note\r\nfern,shade\r\nrex,sun\r\nmoss,damp\r\n";
    let report = csvcmd::csv_import_with(tmp.path(), Prefer::Auto, "plants", doc, true)
        .await
        .expect("import runs")
        .expect("import allowed");
    assert!(report.created);
    assert_eq!(report.rows_inserted, 3);

    // The rows are visible on the serving connection.
    let n: i64 = node
        .node()
        .db()
        .call(|conn| {
            conn.query_row("SELECT count(*) FROM plants", [], |r| r.get(0))
                .unwrap()
        })
        .await
        .unwrap();
    assert_eq!(n, 3);

    // Create DDL and insert chunk both audited as owner ops.
    assert_eq!(local_audit_count(&node, "CREATE TABLE \"plants\"").await, 1);
    assert_eq!(local_audit_count(&node, "INSERT INTO \"plants\"").await, 1);

    // Round trip: the export reads back the same document.
    let out = csvcmd::csv_export_with(tmp.path(), Prefer::Auto, "plants")
        .await
        .expect("export runs")
        .expect("export allowed");
    assert_eq!(out, doc);

    node.shutdown().await;
}

/// chat send's local append, delivered over the socket, vibrates the
/// scaffold's own inbox point for a locally entrained watcher: the
/// end-to-end proof of recorded finding 1 at the command layer.
#[tokio::test(flavor = "multi_thread")]
async fn chat_send_vibrates_local_inbox_watcher_while_serving() {
    let tmp = TempDir::new("och-chat-vib");
    store::init_dir(tmp.path()).expect("init");
    chat::chat_init(tmp.path()).expect("chat init");
    let node = serve::start_node(tmp.path(), true).await.expect("serve");

    // A named recipient (rides the socket too; not the point under test).
    let bob_hex = "ab".repeat(32);
    store::peer_add(tmp.path(), "bob", &bob_hex, &[]).expect("peer add");

    // Entrain the chat scaffold's Sympathetic inbox point over the
    // socket; the ack is a bare Done.
    let mut watcher = rsntr::owner_socket::connect(tmp.path())
        .await
        .expect("connect watcher");
    let mut buf = BytesMut::new();
    send_frame(
        &mut watcher,
        &EnvelopeObject::Entrain(resonator_protocol::Entrain {
            id: "01JOCHCHATWATCH00000000001".into(),
            point: resonator_node::chat::CHAT_INBOX_POINT.into(),
        }),
    )
    .await;
    match read_frame(&mut watcher, &mut buf).await {
        Some(EnvelopeObject::Done(_)) => {}
        other => panic!("expected the entrain ack Done, got {other:?}"),
    }

    // The send: its chat_messages append commits on the serving
    // connection, so the one update hook fires and the watcher vibrates.
    let report = chat::chat_send(tmp.path(), "bob", "hello from the owner channel", None)
        .await
        .expect("send");
    match read_frame(&mut watcher, &mut buf).await {
        Some(EnvelopeObject::Vibration(v)) => {
            assert_eq!(v.point, resonator_node::chat::CHAT_INBOX_POINT);
        }
        other => panic!("expected a Vibration, got {other:?}"),
    }
    drop(watcher);

    // The append itself: owner-audited, outgoing, correctly scoped.
    let (scope, outgoing): (String, i64) = {
        let mid = report.message_id.clone();
        node.node()
            .db()
            .call(move |conn| {
                conn.query_row(
                    "SELECT scope, outgoing FROM chat_messages WHERE id = ?1",
                    [&mid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap()
            })
            .await
            .unwrap()
    };
    assert_eq!(scope, bob_hex);
    assert_eq!(outgoing, 1);
    assert_eq!(
        local_audit_count(&node, "INSERT INTO chat_messages").await,
        1
    );
    assert_eq!(local_audit_count(&node, "INSERT INTO _outbox").await, 1);

    node.shutdown().await;
}

/// chat send --file while the sender is serving: recorded finding 2.
/// The serving process holds the iroh-blobs redb lock, so the CLI hashes
/// natively and delegates the import over the socket (the chat Execute's
/// owner-channel-only path parameter); the serving node imports into its
/// own store and can provide the bytes immediately.
#[tokio::test(flavor = "multi_thread")]
async fn file_send_while_serving_imports_through_the_node() {
    let ta = TempDir::new("och-file-a");
    let tb = TempDir::new("och-file-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    store::init_dir(tb.path()).expect("init b");
    chat::chat_init(ta.path()).expect("chat init a");
    chat::chat_init(tb.path()).expect("chat init b");

    let data: Vec<u8> = (0u32..200 * 1024)
        .map(|i| (i.wrapping_mul(2654435761) >> 11) as u8)
        .collect();
    let src = ta.path().join("cuttings.png");
    std::fs::write(&src, &data).expect("write source");

    // A serves first: its process holds the blob store lock, which used
    // to deadlock a CLI-side import.
    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    {
        let a_hex = a_id.to_string();
        b.node()
            .db()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO _peers (endpoint_id, added_at) \
                     VALUES (?1, datetime('now'))",
                    [&a_hex],
                )
                .expect("admit a");
            })
            .await
            .expect("db call");
    }
    let ticket = b.ready_ticket(Duration::from_secs(3)).await;
    store::peer_add(ta.path(), "b", &ticket, &[]).expect("a learns b");

    let report = chat::chat_send(ta.path(), "b", "cuttings attached", Some(&src))
        .await
        .expect("file send while serving");
    let (hash, bytes) = report.blob.clone().expect("blob attached");
    assert!(hash.starts_with("blake3:"));
    assert_eq!(bytes, data.len() as u64);

    // Exactly one append: the delegated import's chat Execute deduped on
    // the message id instead of adding a note-to-self row.
    let appended: i64 = a
        .node()
        .db()
        .call(|conn| {
            conn.query_row("SELECT count(*) FROM chat_messages", [], |r| r.get(0))
                .unwrap()
        })
        .await
        .unwrap();
    assert_eq!(appended, 1);

    // The bytes are in the serving process's store: B fetches them from
    // A over iroh-blobs, hash-verified.
    store::peer_add(tb.path(), "a", &a_id.to_string(), &a.direct_addrs()).expect("b learns a");
    let out = tb.path().join("fetched.png");
    let outcome = rsntr::client::run_fetch(
        tb.path(),
        "a",
        &hash,
        Some(&out),
        true,
        Duration::from_secs(20),
    )
    .await
    .expect("fetch");
    match outcome {
        rsntr::client::FetchOutcome::Written { bytes, .. } => {
            assert_eq!(bytes, data.len() as u64);
        }
        other => panic!("expected Written, got {other:?}"),
    }
    assert_eq!(std::fs::read(&out).expect("read fetched"), data);

    a.shutdown().await;
    b.shutdown().await;
}

/// mod add + enable ride the socket while serving; the registry row
/// flips live, and (as documented) the serving registry itself still
/// loads at start, so the handler picks the mod up at the next serve.
#[cfg(feature = "mods")]
#[tokio::test(flavor = "multi_thread")]
async fn mod_add_and_enable_over_socket() {
    let tmp = TempDir::new("och-mod");
    store::init_dir(tmp.path()).expect("init");
    let node = serve::start_node(tmp.path(), true).await.expect("serve");

    let wasm = b"\0asm not really wasm".to_vec();
    let sha = rsntr::modcmd::mod_add(tmp.path(), Prefer::Auto, "fakemod", &wasm, &[], None)
        .expect("mod add");
    assert_eq!(sha, resonator_mods::sha256_hex(&wasm));
    assert!(
        rsntr::modcmd::mod_set_enabled(tmp.path(), Prefer::Auto, "fakemod", true).expect("enable"),
        "the row exists and flips"
    );
    assert!(
        !rsntr::modcmd::mod_set_enabled(tmp.path(), Prefer::Auto, "nosuch", true).expect("runs"),
        "a missing row reports false"
    );

    // The flip is visible on the serving connection and audited.
    let (enabled, stored_sha): (i64, String) = node
        .node()
        .db()
        .call(|conn| {
            conn.query_row(
                "SELECT enabled, sha256 FROM _modulations WHERE name = 'fakemod'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        })
        .await
        .unwrap();
    assert_eq!(enabled, 1);
    assert_eq!(stored_sha, sha);
    // Two UPDATE Executes ran (the hit and the miss); both are ledgered.
    assert_eq!(
        local_audit_count(&node, "UPDATE _modulations SET enabled").await,
        2
    );

    // The listing rides an envelope too.
    let rows = rsntr::modcmd::mod_list(tmp.path(), Prefer::Auto).expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "fakemod");
    assert!(rows[0].enabled);

    // The live handler set is unchanged until the next serve start (the
    // registry loads once); the enabled row is nevertheless already in
    // the hello the node would mint now.
    let hello = node.node().hello().await.expect("hello");
    assert!(hello.mods.iter().any(|m| m == "fakemod"));

    node.shutdown().await;
}

/// `rsntr sql --file` semantics: statements split on the semicolon,
/// comment-only segments skipped, DDL permitted, everything audited as
/// owner ops. The runner behind example-mod seeds (shop-mod's seed.sql).
#[tokio::test(flavor = "multi_thread")]
async fn sql_command_applies_a_seed_file() {
    let tmp = TempDir::new("och-sql");
    store::init_dir(tmp.path()).expect("init");

    let source = "\
-- a tiny seed
CREATE TABLE IF NOT EXISTS seeds (name TEXT PRIMARY KEY, note TEXT);

INSERT OR IGNORE INTO seeds (name, note) VALUES ('fern', 'shade');
INSERT OR IGNORE INTO seeds (name, note) VALUES ('rex', 'sun');

-- trailing comment block, skipped by the runner
";
    let dir = tmp.path().to_path_buf();
    let outcome = tokio::task::spawn_blocking({
        let dir = dir.clone();
        move || rsntr::sqlcmd::run_sql(&dir, source, Prefer::Auto)
    })
    .await
    .expect("join")
    .expect("seed applies");
    assert_eq!(outcome.statements, 3);

    // Idempotent re-run: same statements, still two rows.
    let outcome = tokio::task::spawn_blocking({
        let dir = dir.clone();
        move || rsntr::sqlcmd::run_sql(&dir, source, Prefer::Auto)
    })
    .await
    .expect("join")
    .expect("re-run applies");
    assert_eq!(outcome.statements, 3);
    let conn = store::open_db(&dir).expect("open db");
    let n: i64 = conn
        .query_row("SELECT count(*) FROM seeds", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2);
    drop(conn);

    // A read statement returns its rows (the agent-facing SELECT path).
    let outcome = tokio::task::spawn_blocking({
        let dir = dir.clone();
        move || {
            rsntr::sqlcmd::run_sql(
                &dir,
                "SELECT name, note FROM seeds ORDER BY name",
                Prefer::Auto,
            )
        }
    })
    .await
    .expect("join")
    .expect("select runs");
    let (columns, rows, done) = outcome.rows.expect("a read reports rows");
    assert_eq!(columns, vec!["name", "note"]);
    assert_eq!(rows.len(), 2);
    assert_eq!(done.row_count, Some(2));

    // A failing statement reports which one.
    let err = tokio::task::spawn_blocking({
        let dir = dir.clone();
        move || rsntr::sqlcmd::run_sql(&dir, "INSERT INTO nope VALUES (1)", Prefer::Auto)
    })
    .await
    .expect("join")
    .expect_err("bad statement fails");
    assert!(format!("{err:#}").contains("INSERT INTO nope"), "{err:#}");
}
