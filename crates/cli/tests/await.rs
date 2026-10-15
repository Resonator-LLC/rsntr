//! `rsntr await` over the real binary and two live offline nodes: one
//! event per run, durable cursor, backlog drain without a daemon, inbox
//! events on a knock, flock exclusion, timeout, own sends skipped.

#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rsntr::testutil::TempDir;
use rsntr::{chat, client, serve, store};

async fn admit(node: &serve::RunningNode, peer_hex: &str) {
    let peer = peer_hex.to_string();
    node.node()
        .db()
        .call(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO _peers (endpoint_id, added_at) VALUES (?1, datetime('now'))",
                [&peer],
            )
            .expect("admit");
        })
        .await
        .expect("db call");
}

/// Runs `rsntr await` to completion: (exit code, stdout line, stderr).
fn run_await(dir: &Path, cursor: &Path, extra: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(["await", "-d"])
        .arg(dir)
        .arg("--cursor-file")
        .arg(cursor)
        .args(extra)
        .stdin(Stdio::null())
        .output()
        .expect("run await");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn cursor_of(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("cursor file")).expect("cursor json")
}

async fn wait_delivered(dir: &Path, scope: &str, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let entries = chat::chat_log(dir, scope, 50, None).expect("log");
        if entries.iter().any(|e| e.id == id) {
            return;
        }
        assert!(Instant::now() < deadline, "message {id} not delivered");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn await_end_to_end() {
    let ta = TempDir::new("await-a");
    let tb = TempDir::new("await-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    store::init_dir(tb.path()).expect("init b");
    chat::chat_init(ta.path()).expect("chat a");
    chat::chat_init(tb.path()).expect("chat b");
    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    store::peer_add(
        ta.path(),
        "b",
        &b.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("a learns b");
    store::peer_add(
        tb.path(),
        "a",
        &a.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("b learns a");
    let a_hex = a_id.to_string();
    admit(&b, &a_hex).await;
    let cursor = tb.path().join("agent.cursor");
    let bdir = tb.path().to_path_buf();

    // 1. Timeout with nothing new: exit 3, one JSON line, cursor created at "now".
    let (code, line, _) = {
        let (b2, c2) = (bdir.clone(), cursor.clone());
        tokio::task::spawn_blocking(move || run_await(&b2, &c2, &["--timeout", "1"]))
            .await
            .unwrap()
    };
    assert_eq!(code, 3, "timeout exit: {line}");
    let v: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert_eq!(v["event"], "timeout");
    assert!(cursor.exists());

    // 2. Blocks, then emits exactly one message event on a live send.
    let waiter = {
        let (b2, c2) = (bdir.clone(), cursor.clone());
        tokio::task::spawn_blocking(move || run_await(&b2, &c2, &["--timeout", "30"]))
    };
    tokio::time::sleep(Duration::from_millis(1200)).await; // let it entrain
    let first = chat::chat_send(ta.path(), "b", "wake one", None)
        .await
        .expect("send");
    a.wake_outbox();
    let (code, line, err) = waiter.await.unwrap();
    assert_eq!(code, 0, "stderr: {err}");
    let v: serde_json::Value = serde_json::from_str(&line).expect("event json");
    assert_eq!(v["event"], "message");
    assert_eq!(v["id"], first.message_id);
    assert_eq!(v["from"], a_hex);
    assert!(v["cursor"]["chat_rowid"].as_i64().unwrap() > 0);
    let after_one = cursor_of(&cursor);

    // 3. Backlog drains oldest-first WITHOUT a daemon, one per run.
    let m2 = chat::chat_send(ta.path(), "b", "two", None)
        .await
        .expect("send 2");
    a.wake_outbox();
    wait_delivered(tb.path(), &a_hex, &m2.message_id).await;
    let m3 = chat::chat_send(ta.path(), "b", "three", None)
        .await
        .expect("send 3");
    a.wake_outbox();
    wait_delivered(tb.path(), &a_hex, &m3.message_id).await;
    b.shutdown().await; // no daemon from here on
    let (code, line, _) = {
        let (b2, c2) = (bdir.clone(), cursor.clone());
        tokio::task::spawn_blocking(move || run_await(&b2, &c2, &["--timeout", "5"]))
            .await
            .unwrap()
    };
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["id"], m2.message_id, "oldest first");
    assert!(cursor_of(&cursor)["chat_rowid"].as_i64() > after_one["chat_rowid"].as_i64());
    let (code, line, _) = {
        let (b2, c2) = (bdir.clone(), cursor.clone());
        tokio::task::spawn_blocking(move || run_await(&b2, &c2, &["--timeout", "5"]))
            .await
            .unwrap()
    };
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["id"], m3.message_id, "then the newer");
    // Caught up + no daemon: a clear error naming rsntr serve.
    let (code, _, err) = {
        let (b2, c2) = (bdir.clone(), cursor.clone());
        tokio::task::spawn_blocking(move || run_await(&b2, &c2, &["--timeout", "5"]))
            .await
            .unwrap()
    };
    assert_eq!(code, 1);
    assert!(err.contains("rsntr serve"), "stderr: {err}");

    a.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn await_inbox_event_flock_and_own_sends() {
    let tb = TempDir::new("await2-b");
    let tc = TempDir::new("await2-c");
    store::init_dir(tb.path()).expect("init b");
    let c_id = store::init_dir(tc.path()).expect("init c");
    chat::chat_init(tb.path()).expect("chat b");
    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    store::peer_add(
        tc.path(),
        "b",
        &b.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("c learns b");
    let cursor = tb.path().join("agent.cursor");
    let bdir = tb.path().to_path_buf();

    // Establish the cursor at now.
    let (code, _, _) = {
        let (b2, c2) = (bdir.clone(), cursor.clone());
        tokio::task::spawn_blocking(move || run_await(&b2, &c2, &["--timeout", "1"]))
            .await
            .unwrap()
    };
    assert_eq!(code, 3);

    // A stranger's knock parks in the inbox -> an inbox event.
    let waiter = {
        let (b2, c2) = (bdir.clone(), cursor.clone());
        tokio::task::spawn_blocking(move || run_await(&b2, &c2, &["--timeout", "30"]))
    };
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let knock = client::knock(tc.path(), "b", "let me in please", true)
        .await
        .expect("knock");
    assert_eq!(
        knock.decision.as_ref().map(|d| d.decision.as_str()),
        Some("pending")
    );
    let (code, line, err) = waiter.await.unwrap();
    assert_eq!(code, 0, "stderr: {err}");
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["event"], "inbox");
    assert_eq!(v["kind"], "knock");
    assert_eq!(v["peer"], c_id.to_string());
    assert!(v["message"].as_str().unwrap().contains("let me in"));

    // flock: a second awaiter on the same cursor is refused immediately.
    let holder = {
        let (b2, c2) = (bdir.clone(), cursor.clone());
        tokio::task::spawn_blocking(move || run_await(&b2, &c2, &["--timeout", "6"]))
    };
    tokio::time::sleep(Duration::from_millis(800)).await;
    let (code, _, err) = {
        let (b2, c2) = (bdir.clone(), cursor.clone());
        tokio::task::spawn_blocking(move || run_await(&b2, &c2, &["--timeout", "1"]))
            .await
            .unwrap()
    };
    assert_eq!(code, 1, "second awaiter must be refused: {err}");
    assert!(err.contains("already holds"), "stderr: {err}");
    let (code, _, _) = holder.await.unwrap();
    assert_eq!(code, 3, "holder times out normally");

    // Own sends never wake, but the cursor advances over them.
    let before = cursor_of(&cursor)["chat_rowid"].as_i64().unwrap();
    store::peer_add(tb.path(), "c", &c_id.to_string(), &[]).expect("b names c");
    chat::chat_send(tb.path(), "c", "outgoing is silent", None)
        .await
        .expect("own send");
    let (code, line, _) = {
        let (b2, c2) = (bdir.clone(), cursor.clone());
        tokio::task::spawn_blocking(move || run_await(&b2, &c2, &["--timeout", "2"]))
            .await
            .unwrap()
    };
    assert_eq!(code, 3, "own send must not wake: {line}");
    assert!(cursor_of(&cursor)["chat_rowid"].as_i64().unwrap() > before);

    b.shutdown().await;
}

/// A first run replays inbox rows still awaiting a decision (a knock
/// that landed before the agent started), skips rows already decided,
/// and starts chat at now.
#[tokio::test(flavor = "multi_thread")]
async fn first_await_replays_pending_inbox_rows() {
    let tb = TempDir::new("await3-b");
    let tc = TempDir::new("await3-c");
    store::init_dir(tb.path()).expect("init b");
    let c_id = store::init_dir(tc.path()).expect("init c");
    chat::chat_init(tb.path()).expect("chat b");
    // An older, already decided row: history, not a wake.
    {
        let conn = store::open_db(tb.path()).expect("open");
        conn.execute(
            "INSERT INTO _inbox (request_id, peer, sql, params, received_at, decision, decided_by) \
             VALUES ('01OLDKNOCK', 'someone', '', 'knock: long ago', '2026-01-01T00:00:00Z', \
                     'deny', 'human')",
            [],
        )
        .expect("decided row");
        // Old chat is not replayed either.
        conn.execute(
            "INSERT INTO chat_messages (id, scope, sender, at, body) \
             VALUES ('01OLDMSG', 'x', 'y', '2026-01-01T00:00:00Z', 'old chatter')",
            [],
        )
        .expect("old message");
    }
    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    store::peer_add(
        tc.path(),
        "b",
        &b.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("c learns b");

    // The knock lands BEFORE the agent's first await.
    let knock = client::knock(tc.path(), "b", "anyone home?", true)
        .await
        .expect("knock");
    let pending_id = knock
        .decision
        .as_ref()
        .and_then(|d| d.id.clone())
        .expect("pending id");
    b.shutdown().await;

    // First run, no cursor file, no daemon: the open knock is the event.
    let cursor = tb.path().join("late-agent.cursor");
    let bdir = tb.path().to_path_buf();
    let (code, line, err) = {
        let (b2, c2) = (bdir.clone(), cursor.clone());
        tokio::task::spawn_blocking(move || run_await(&b2, &c2, &["--timeout", "2"]))
            .await
            .unwrap()
    };
    assert_eq!(code, 0, "stderr: {err}");
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["event"], "inbox", "{v}");
    assert_eq!(v["kind"], "knock");
    assert_eq!(v["id"], serde_json::json!(pending_id));
    assert_eq!(v["peer"], c_id.to_string());
    let c = cursor_of(&cursor);
    assert_eq!(c["inbox_rowid"], 2, "past the decided and the pending row");
    assert_eq!(c["chat_rowid"], 1, "chat started at now");

    // Nothing else is owed: the decided row and the old message stay
    // silent (exit 1: caught up and nothing serving).
    let (code, line, err) = {
        let (b2, c2) = (bdir.clone(), cursor.clone());
        tokio::task::spawn_blocking(move || run_await(&b2, &c2, &["--timeout", "2"]))
            .await
            .unwrap()
    };
    assert_eq!(code, 1, "stdout: {line}");
    assert!(err.contains("rsntr serve"), "stderr: {err}");
}

/// A lost wake-up costs seconds, never the whole timeout: with the
/// serving node's foreign-write watcher stopped and a row written on a
/// raw connection (so no vibration can reach the awaiter), the periodic
/// re-scan still returns the event.
#[tokio::test(flavor = "multi_thread")]
async fn a_lost_notification_is_bounded_by_the_rescan() {
    let tb = TempDir::new("await4-b");
    store::init_dir(tb.path()).expect("init b");
    chat::chat_init(tb.path()).expect("chat b");
    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    b.node().stop_change_watcher();
    let cursor = tb.path().join("agent.cursor");
    let cursor_s = cursor.to_str().unwrap().to_string();
    let bdir = tb.path().to_path_buf();
    let started = std::time::Instant::now();
    let waiter = std::thread::spawn(move || {
        let out = Command::new(env!("CARGO_BIN_EXE_rsntr"))
            .args(["await", "-d"])
            .arg(&bdir)
            .args(["--cursor-file", &cursor_s, "--timeout", "30"])
            .env("RSNTR_AWAIT_RESCAN_MS", "400")
            .stdin(Stdio::null())
            .output()
            .expect("run await");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    });
    tokio::time::sleep(Duration::from_millis(1500)).await;
    // No hook (raw connection) and no watcher: nothing vibrates.
    let raw = rusqlite::Connection::open(store::db_path(tb.path())).unwrap();
    raw.execute(
        "INSERT INTO chat_messages (id, scope, sender, at, body) \
         VALUES ('01LOSTWAKE0000000000000001', 'x', 'peer', '2026-09-09T00:00:00Z', 'silent')",
        [],
    )
    .unwrap();
    let (code, line, err) = waiter.join().unwrap();
    assert_eq!(code, 0, "stderr: {err}");
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["event"], "message");
    assert_eq!(v["id"], "01LOSTWAKE0000000000000001");
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "the re-scan bounds a lost wake: took {:?}",
        started.elapsed()
    );
    b.shutdown().await;
}
