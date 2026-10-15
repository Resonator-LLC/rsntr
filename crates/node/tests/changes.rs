//! M1: commit-time signalling and the foreign-write watcher.
//!
//! Two pipelines over one file database: what one commits, the other
//! sees within a couple of watcher ticks; a rolled-back write vibrates
//! nothing; own writes signal exactly once (the commit hook), never a
//! second time through the watcher.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use resonator_authenticator::Chain;
use resonator_node::{DbHandle, Node, NodeConfig, open_node_db, seed_rsntr_defaults};
use resonator_protocol::vocab::PROJECTION_CHANGED;
use resonator_protocol::{EnvelopeObject, Request, RequestKind};
use resonator_transport::{RequestStream, TransportError};

#[derive(Clone, Default)]
struct Recorded {
    sent: Arc<Mutex<Vec<EnvelopeObject>>>,
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

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rsntr-changes-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fast_config() -> NodeConfig {
    NodeConfig {
        change_watch_interval_ms: 25,
        ..NodeConfig::default()
    }
}

async fn node_at(path: &std::path::Path, seed: bool) -> Arc<Node> {
    let conn = open_node_db(path).expect("open");
    if seed {
        seed_rsntr_defaults(&conn).expect("seed");
        resonator_node::set_rsntr(&conn, "endpoint_id", &"ee".repeat(32)).expect("id");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, body TEXT); \
             CREATE TABLE IF NOT EXISTS uniq (id INTEGER PRIMARY KEY, k TEXT UNIQUE);",
        )
        .expect("fixture");
    }
    let node = Arc::new(Node::new(
        DbHandle::spawn(conn),
        Chain::with_builtin_tiers(),
        fast_config(),
    ));
    node.install_vibration_hook().await.expect("hook");
    node
}

async fn owner_execute(node: &Node, sql: &str) -> Vec<EnvelopeObject> {
    let req = Request::new(RequestKind::Execute, "sql-sqlite", sql);
    let out = Recorded::default();
    let mut stream = LocalStream {
        out: out.clone(),
        incoming: VecDeque::new(),
    };
    node.handle_owner(req.to_envelope(), &mut stream)
        .await
        .expect("dispatch");
    out.sent.lock().unwrap().clone()
}

/// Drains signals for `window`, returning how many arrived.
async fn count_signals(rx: &mut tokio::sync::mpsc::Receiver<()>, window: Duration) -> usize {
    let deadline = tokio::time::Instant::now() + window;
    let mut n = 0;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(())) => n += 1,
            Ok(None) => break,
            Err(_) => break,
        }
    }
    n
}

#[tokio::test(flavor = "multi_thread")]
async fn own_writes_signal_once_and_foreign_writes_signal_through_the_watcher() {
    let dir = scratch("watch");
    let path = dir.join("n.db");
    let a = node_at(&path, true).await;
    a.start_change_watcher().await;
    let (_id, mut rx) = a
        .entrainments()
        .subscribe("urn:test:notes".into(), Some("notes".into()));
    // Prime: the first tick only snapshots.
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(count_signals(&mut rx, Duration::from_millis(100)).await, 0);

    // Own write: exactly one signal (the commit hook), none from the
    // watcher afterwards.
    let frames = owner_execute(&a, "INSERT INTO notes (body) VALUES ('mine')").await;
    assert!(
        frames.iter().any(|f| matches!(f, EnvelopeObject::Done(_))),
        "{frames:?}"
    );
    assert_eq!(count_signals(&mut rx, Duration::from_millis(300)).await, 1);

    // Foreign write on a raw connection (another app, the CLI with
    // --local): the watcher signals within a few ticks.
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute("INSERT INTO notes (body) VALUES ('theirs')", [])
        .unwrap();
    assert!(
        count_signals(&mut rx, Duration::from_millis(500)).await >= 1,
        "foreign write vibrates"
    );
    // Quiet again: no signal without a commit.
    assert_eq!(count_signals(&mut rx, Duration::from_millis(200)).await, 0);

    // A second pipeline on the same file sees the first one's commits.
    let b = node_at(&path, false).await;
    b.start_change_watcher().await;
    let (_id, mut rx_b) = b
        .entrainments()
        .subscribe("urn:test:notes".into(), Some("notes".into()));
    tokio::time::sleep(Duration::from_millis(80)).await;
    let _ = count_signals(&mut rx_b, Duration::from_millis(50)).await;
    owner_execute(&a, "INSERT INTO notes (body) VALUES ('from a')").await;
    assert!(count_signals(&mut rx_b, Duration::from_millis(500)).await >= 1);
    assert!(a.is_watching_changes());
    a.stop_change_watcher();
    assert!(!a.is_watching_changes());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn rolled_back_writes_vibrate_nothing_and_commits_are_visible_when_signalled() {
    let dir = scratch("rollback");
    let path = dir.join("n.db");
    let a = node_at(&path, true).await;
    let (_id, mut rx) = a
        .entrainments()
        .subscribe("urn:test:uniq".into(), Some("uniq".into()));
    // Row 3 violates UNIQUE: the whole statement rolls back after the
    // update hook already saw rows 1 and 2.
    let frames = owner_execute(&a, "INSERT INTO uniq (k) VALUES ('a'), ('b'), ('a')").await;
    assert!(
        frames.iter().any(|f| matches!(f, EnvelopeObject::Error(_))),
        "{frames:?}"
    );
    assert_eq!(count_signals(&mut rx, Duration::from_millis(200)).await, 0);
    let n: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT count(*) FROM uniq", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "nothing committed");

    // A good write: one signal, and by the time it arrives a second
    // connection already sees the row (commit-time, not per-row).
    let raw = rusqlite::Connection::open(&path).unwrap();
    owner_execute(&a, "INSERT INTO uniq (k) VALUES ('c')").await;
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("signal")
        .expect("open");
    let n: i64 = raw
        .query_row("SELECT count(*) FROM uniq", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn projection_changed_is_gated_by_the_fingerprint() {
    let dir = scratch("projection");
    let path = dir.join("n.db");
    let a = node_at(&path, true).await;
    a.start_change_watcher().await;
    let (_id, mut rx) = a.entrainments().subscribe(PROJECTION_CHANGED.into(), None);
    tokio::time::sleep(Duration::from_millis(80)).await;
    let _ = count_signals(&mut rx, Duration::from_millis(50)).await;

    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute("INSERT INTO notes (body) VALUES ('unrelated')", [])
        .unwrap();
    assert_eq!(
        count_signals(&mut rx, Duration::from_millis(300)).await,
        0,
        "a foreign write elsewhere does not claim the projection changed"
    );
    raw.execute(
        "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
         VALUES ('x', 'notes', 'read', 'allow')",
        [],
    )
    .unwrap();
    assert!(
        count_signals(&mut rx, Duration::from_millis(500)).await >= 1,
        "a foreign policy change does"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
