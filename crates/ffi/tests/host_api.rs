//! Host-side exercise of the mobile FFI surface over a temp dir: init,
//! owner-channel execute/query, sparql, chat send + log, and the
//! vibration callback driven by a plain table write. Plain `#[test]`
//! throughout: the FFI is a blocking surface backed by its own runtime,
//! exactly as Swift/Kotlin call it.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use resonator_ffi::{FfiValue, Node, VibrationListener};

fn text(v: &str) -> FfiValue {
    FfiValue::Text { v: v.to_string() }
}

/// Collects callbacks and wakes waiters.
#[derive(Default)]
struct Recorder {
    state: Mutex<RecorderState>,
    cond: Condvar,
}

#[derive(Default)]
struct RecorderState {
    vibrations: Vec<(String, i64)>,
    ended: Option<Option<String>>,
}

impl Recorder {
    fn wait_for<F: Fn(&RecorderState) -> bool>(&self, pred: F, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().expect("recorder lock");
        while !pred(&state) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            let (s, _timed_out) = self.cond.wait_timeout(state, left).expect("recorder wait");
            state = s;
        }
        true
    }
}

impl VibrationListener for Recorder {
    fn on_vibration(&self, point: String, seq: i64, _at: Option<String>) {
        self.state
            .lock()
            .expect("recorder lock")
            .vibrations
            .push((point, seq));
        self.cond.notify_all();
    }

    fn on_end(&self, reason: Option<String>) {
        self.state.lock().expect("recorder lock").ended = Some(reason);
        self.cond.notify_all();
    }
}

#[test]
fn full_mobile_surface_over_a_temp_dir() {
    let tmp = rsntr::testutil::TempDir::new("ffi-host");
    let dir = tmp.path().to_string_lossy().into_owned();

    // Constructing initializes the directory; reopening keeps the id.
    let node = Node::new(dir.clone(), true).expect("init node");
    let id = node.endpoint_id().expect("endpoint id");
    assert_eq!(id.len(), 64);
    let node2 = Node::new(dir.clone(), true).expect("reopen");
    assert_eq!(node2.endpoint_id().expect("id again"), id);
    drop(node2);
    assert!(node.db_path().ends_with("rsntr.db"));
    assert!(!node.is_serving());

    // Owner-channel SQL: DDL is permitted, params bind, rows come back
    // aligned to columns.
    node.local_execute(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT)".into(),
        vec![],
    )
    .expect("create table");
    let done = node
        .local_execute(
            "INSERT INTO items (name) VALUES (?1)".into(),
            vec![text("torch")],
        )
        .expect("insert");
    assert_eq!(done.affected_rows, Some(1));
    let result = node
        .local_query("SELECT id, name FROM items".into(), vec![])
        .expect("select");
    assert_eq!(result.columns, vec!["id", "name"]);
    assert_eq!(result.rows.len(), 1);
    assert!(matches!(
        &result.rows[0].values[0],
        FfiValue::Integer { v: 1 }
    ));
    assert!(matches!(&result.rows[0].values[1], FfiValue::Text { v } if v == "torch"));

    // SPARQL over the same channel: update then select.
    let up = node
        .sparql(
            "INSERT DATA { <http://example.org/alice> <http://example.org/name> \"Alice\" }".into(),
        )
        .expect("insert data");
    assert_eq!(up.affected_rows, Some(1));
    let sel = node
        .sparql(
            "SELECT ?o WHERE { <http://example.org/alice> <http://example.org/name> ?o }".into(),
        )
        .expect("sparql select");
    assert_eq!(sel.rows.len(), 1);
    assert!(matches!(&sel.rows[0].values[0], FfiValue::Text { v } if v == "\"Alice\""));
    let graph = node
        .sparql("CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }".into())
        .expect("construct");
    assert!(graph.turtle.expect("turtle").contains("Alice"));

    // Chat: scaffold, register a peer (a real endpoint id from a second
    // node directory), send, and read the log back.
    let tmp_peer = rsntr::testutil::TempDir::new("ffi-peer");
    let peer_node =
        Node::new(tmp_peer.path().to_string_lossy().into_owned(), true).expect("peer node");
    let peer_id = peer_node.endpoint_id().expect("peer id");
    node.chat_init().expect("chat init");
    node.add_peer("bob".into(), peer_id.clone())
        .expect("add peer");
    let receipt = node
        .chat_send("bob".into(), "hello from the ffi".into())
        .expect("chat send");
    assert_eq!(receipt.scope, peer_id);
    assert_eq!(receipt.queued_to, vec![peer_id.clone()]);
    let log = node.chat_log(None, 10).expect("chat log");
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].body, "hello from the ffi");
    assert!(log[0].outgoing);
    assert_eq!(log[0].id, receipt.message_id);
    let scoped = node.chat_log(Some("bob".into()), 10).expect("scoped log");
    assert_eq!(scoped.len(), 1);

    // Vibrations: register a Sympathetic point over plain SQL, entrain
    // it, write the watched table, and expect the callback.
    node.local_execute(
        "INSERT INTO _projection (point_iri, kind, label, resource) \
         VALUES ('urn:test:items-changed', 'sympathetic', 'items changed', 'items')"
            .into(),
        vec![],
    )
    .expect("register point");
    let recorder = Arc::new(Recorder::default());
    let listener: Arc<dyn VibrationListener> = recorder.clone();
    let session = node
        .entrain("urn:test:items-changed".into(), listener)
        .expect("entrain");
    assert_eq!(session.point(), "urn:test:items-changed");

    node.local_execute(
        "INSERT INTO items (name) VALUES (?1)".into(),
        vec![text("bell")],
    )
    .expect("write watched table");
    assert!(
        recorder.wait_for(|s| !s.vibrations.is_empty(), Duration::from_secs(10)),
        "no vibration arrived within 10s"
    );
    {
        let state = recorder.state.lock().expect("recorder lock");
        assert_eq!(state.vibrations[0].0, "urn:test:items-changed");
        assert_eq!(state.vibrations[0].1, 0);
        assert!(state.ended.is_none(), "entrainment ended prematurely");
    }

    // Damp ends the session cleanly; damp is idempotent.
    session.damp();
    assert!(
        recorder.wait_for(|s| s.ended.is_some(), Duration::from_secs(10)),
        "no on_end within 10s of damping"
    );
    assert_eq!(
        recorder.state.lock().expect("recorder lock").ended,
        Some(None),
        "expected a clean end"
    );
    session.damp();

    // An unknown point refuses instead of hanging.
    let missing = node.entrain("urn:test:nope".into(), recorder.clone() as _);
    assert!(missing.is_err());
}

/// A listener that calls back into the node from inside `on_vibration`
/// (allowed: callbacks run on the node's dispatcher thread).
struct Reentrant {
    node: Arc<Node>,
    rows: Mutex<Option<usize>>,
    cond: Condvar,
}

impl VibrationListener for Reentrant {
    fn on_vibration(&self, _point: String, _seq: i64, _at: Option<String>) {
        let n = self
            .node
            .local_query("SELECT id FROM a_notes".into(), vec![])
            .map(|r| r.rows.len())
            .unwrap_or(usize::MAX);
        *self.rows.lock().unwrap() = Some(n);
        self.cond.notify_all();
    }
    fn on_end(&self, _reason: Option<String>) {}
}

#[test]
fn two_apps_share_one_directory_and_hand_serving_over() {
    use resonator_ffi::{AppInfo, FfiError, RouteKind, ServePolicy, TableDecl};
    let tmp = rsntr::testutil::TempDir::new("ffi-shared");
    let dir = tmp.path().to_string_lossy().into_owned();
    let app = |id: &str| {
        Some(AppInfo {
            id: id.into(),
            name: Some(id.rsplit('.').next().unwrap().to_uppercase()),
            version: Some("1.0".into()),
        })
    };
    let a = Node::open(
        dir.clone(),
        true,
        app("org.example.a"),
        ServePolicy::ServeIfFree,
    )
    .expect("open a");
    let b = Node::open(
        dir.clone(),
        true,
        app("org.example.b"),
        ServePolicy::ServeIfFree,
    )
    .expect("open b");
    assert_eq!(
        a.endpoint_id().unwrap(),
        b.endpoint_id().unwrap(),
        "one identity"
    );

    // Both registered; A declares a table B can then find.
    let ids: Vec<String> = a.apps().unwrap().into_iter().map(|r| r.app_id).collect();
    assert!(
        ids.contains(&"org.example.a".to_string()) && ids.contains(&"org.example.b".to_string())
    );
    a.local_execute(
        "CREATE TABLE a_notes (id INTEGER PRIMARY KEY, body TEXT)".into(),
        vec![],
    )
    .unwrap();
    a.declare_tables(vec![TableDecl {
        name: "a_notes".into(),
        comment: "notes kept by app A".into(),
    }])
    .unwrap();
    let a_row = b
        .apps()
        .unwrap()
        .into_iter()
        .find(|r| r.app_id == "org.example.a")
        .unwrap();
    assert_eq!(a_row.tables, vec!["a_notes"]);
    assert_eq!(a.current_route().unwrap(), RouteKind::Local);

    // A serves; B is refused the lease and routes over the owner channel.
    a.serve().expect("a serves");
    match b.serve() {
        Err(FfiError::AlreadyServing { holder: Some(h) }) => {
            assert_eq!(h.app_id, "org.example.a");
            assert_eq!(h.pid, std::process::id());
        }
        other => panic!("expected AlreadyServing, got {other:?}"),
    }
    assert_eq!(b.current_route().unwrap(), RouteKind::Socket);
    assert_eq!(a.current_route().unwrap(), RouteKind::Serving);
    assert_eq!(a.serving_info().unwrap().app_id, "org.example.a");

    // A entrains on its table; B's write (over the channel) vibrates it.
    a.local_execute(
        "INSERT INTO _projection (point_iri, kind, label, resource) \
         VALUES ('urn:test:a-notes', 'sympathetic', 'notes changed', 'a_notes')"
            .into(),
        vec![],
    )
    .unwrap();
    let rec_a = Arc::new(Recorder::default());
    let _ea = a
        .entrain("urn:test:a-notes".into(), rec_a.clone() as _)
        .expect("a entrains");
    b.local_execute(
        "INSERT INTO a_notes (body) VALUES ('from b')".into(),
        vec![],
    )
    .unwrap();
    assert!(rec_a.wait_for(|s| !s.vibrations.is_empty(), Duration::from_secs(10)));

    // B entrains over the channel; A's write vibrates it; a re-entrant
    // listener may call the node from the callback.
    let rec_b = Arc::new(Recorder::default());
    let _eb = b
        .entrain("urn:test:a-notes".into(), rec_b.clone() as _)
        .expect("b entrains over the socket");
    let reentrant = Arc::new(Reentrant {
        node: b.clone(),
        rows: Mutex::new(None),
        cond: Condvar::new(),
    });
    let _er = b
        .entrain("urn:test:a-notes".into(), reentrant.clone() as _)
        .expect("reentrant entrains");
    a.local_execute(
        "INSERT INTO a_notes (body) VALUES ('from a')".into(),
        vec![],
    )
    .unwrap();
    assert!(rec_b.wait_for(|s| !s.vibrations.is_empty(), Duration::from_secs(10)));
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut rows = reentrant.rows.lock().unwrap();
        while rows.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "re-entrant callback never ran");
            rows = reentrant.cond.wait_timeout(rows, left).unwrap().0;
        }
        assert_eq!(*rows, Some(2), "the callback read the table");
    }

    // A suspends: its own entrainment ends "closed", B's socket
    // entrainment ends "server gone", and B takes the lease on resume.
    a.suspend();
    assert!(rec_a.wait_for(|s| s.ended.is_some(), Duration::from_secs(10)));
    assert_eq!(
        rec_a.state.lock().unwrap().ended,
        Some(Some("closed".into()))
    );
    assert!(rec_b.wait_for(|s| s.ended.is_some(), Duration::from_secs(10)));
    let reason = rec_b
        .state
        .lock()
        .unwrap()
        .ended
        .clone()
        .flatten()
        .unwrap_or_default();
    assert!(reason.contains("server gone"), "{reason}");
    assert!(!a.is_serving());
    let info = b.resume().expect("resume").expect("someone serves");
    assert_eq!(info.app_id, "org.example.b");
    assert!(b.is_serving());
    assert_eq!(a.current_route().unwrap(), RouteKind::Socket);

    // close() ends entrainments with "closed" without stopping serving.
    let rec_c = Arc::new(Recorder::default());
    let _ec = b
        .entrain("urn:test:a-notes".into(), rec_c.clone() as _)
        .expect("entrain on b");
    b.close();
    assert!(rec_c.wait_for(|s| s.ended.is_some(), Duration::from_secs(10)));
    assert_eq!(
        rec_c.state.lock().unwrap().ended,
        Some(Some("closed".into()))
    );
    assert!(b.is_serving());
    b.suspend();
    assert!(b.serving_info().is_none());
    // ClientOnly never takes the lease.
    let c = Node::open(dir, true, app("org.example.c"), ServePolicy::ClientOnly).unwrap();
    assert!(c.resume().unwrap().is_none());
    assert!(!c.is_serving());
}
