//! End-to-end tests for the mods host: the time mod through the full
//! node pipeline, registry refusal rules, capability gating, policy
//! gating, and audit rows.
//!
//! The time-mod wasm is built on demand (once) from examples/time-mod;
//! when neither the build nor a previously built artifact is available
//! the wasm-dependent tests skip with a message.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::mpsc;

use resonator_authenticator::Chain;
use resonator_mods::{ModRegistry, ModsHost, mod_add, mod_set_enabled};
use resonator_node::{
    DbHandle, ModHandler, Node, NodeConfig, open_node_db_in_memory, seed_rsntr_defaults,
};
use resonator_protocol::{EnvelopeObject, Request, RequestKind, Value};
use resonator_transport::{IncomingRequest, PeerId, RequestStream, TransportError, basic_hello};

// ---------------------------------------------------------------------------
// The time-mod wasm, built once
// ---------------------------------------------------------------------------

fn time_wasm() -> Option<&'static [u8]> {
    static WASM: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    WASM.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/time-mod");
        let artifact = root.join("target/wasm32-unknown-unknown/release/time_mod.wasm");
        let build = std::process::Command::new("cargo")
            .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
            .current_dir(&root)
            .output();
        match build {
            Ok(out) if out.status.success() => {}
            Ok(out) => eprintln!(
                "time-mod build failed (falling back to any existing artifact):\n{}",
                String::from_utf8_lossy(&out.stderr)
            ),
            Err(e) => eprintln!("time-mod build could not run: {e}"),
        }
        std::fs::read(&artifact).ok()
    })
    .as_deref()
}

macro_rules! require_wasm {
    () => {
        match time_wasm() {
            Some(w) => w,
            None => {
                eprintln!(
                    "SKIPPED: time_mod.wasm unavailable (needs the \
                     wasm32-unknown-unknown target; run `cargo build --release \
                     --target wasm32-unknown-unknown` in examples/time-mod)"
                );
                return;
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Local stream double (same shape as the node pipeline tests)
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

const PEER: [u8; 32] = [1u8; 32];
const STRANGER: [u8; 32] = [9u8; 32];

fn peer_hex() -> String {
    PeerId(PEER).to_string()
}

async fn test_node() -> Arc<Node> {
    let conn = open_node_db_in_memory().expect("open db");
    seed_rsntr_defaults(&conn).expect("seed");
    Arc::new(Node::new(
        DbHandle::spawn(conn),
        Chain::with_builtin_tiers(),
        NodeConfig::default(),
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

/// Adds the time mod row with the given caps and enables it.
async fn register_time_mod(node: &Node, wasm: &'static [u8], caps: &[&str]) {
    let caps: Vec<String> = caps.iter().map(|s| s.to_string()).collect();
    node.db()
        .call(move |conn| {
            mod_add(conn, "time", wasm, &caps, None).expect("mod add");
            assert!(mod_set_enabled(conn, "time", true).expect("enable"));
        })
        .await
        .unwrap();
}

/// Loads the host from the node's tables and registers it.
async fn install_host(node: &Arc<Node>) -> Vec<(String, String)> {
    ModsHost::install(node).await.expect("mods host load")
}

/// Drives one request through the full pipeline and returns the frames.
async fn drive(node: &Node, peer: [u8; 32], first: EnvelopeObject) -> Vec<EnvelopeObject> {
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
    .expect("handle");
    out.frames()
}

fn time_request(signal: &str) -> (Request, String) {
    let req = Request::new(RequestKind::Query, "time", signal);
    let id = req.id_string();
    (req, id)
}

async fn audit_rows(node: &Node, request_id: &str) -> Vec<(String, String)> {
    let id = request_id.to_string();
    node.db()
        .call(move |conn| {
            let mut stmt = conn
                .prepare("SELECT decision, signal FROM _audit WHERE request_id = ?1 ORDER BY id")
                .unwrap();
            stmt.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<Result<Vec<(String, String)>, _>>()
                .unwrap()
        })
        .await
        .unwrap()
}

// ---------------------------------------------------------------------------
// Registry rules
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn registry_verifies_sha_and_caps() {
    let wasm = require_wasm!();
    let node = test_node().await;
    register_time_mod(&node, wasm, &["clock"]).await;

    // 1. Corrupt hash: refused.
    node.db()
        .call(|conn| {
            conn.execute("UPDATE _modulations SET sha256 = 'deadbeef'", [])
                .unwrap();
        })
        .await
        .unwrap();
    let (registry, refused) = node
        .db()
        .call(|conn| ModRegistry::load(conn, 30_000))
        .await
        .unwrap()
        .unwrap();
    assert!(registry.is_empty());
    assert_eq!(refused.len(), 1);
    assert!(refused[0].1.contains("sha256 mismatch"), "{:?}", refused);

    // 2. Fix the hash but revoke the caps: needs are not covered.
    let sha = resonator_mods::sha256_hex(wasm);
    node.db()
        .call(move |conn| {
            conn.execute("UPDATE _modulations SET sha256 = ?1, caps = '[]'", [&sha])
                .unwrap();
        })
        .await
        .unwrap();
    let (registry, refused) = node
        .db()
        .call(|conn| ModRegistry::load(conn, 30_000))
        .await
        .unwrap()
        .unwrap();
    assert!(registry.is_empty());
    assert!(refused[0].1.contains("clock"), "{:?}", refused);

    // 3. Grant the need: loads, name and descriptor line up.
    node.db()
        .call(|conn| {
            conn.execute("UPDATE _modulations SET caps = '[\"clock\"]'", [])
                .unwrap();
        })
        .await
        .unwrap();
    let (registry, refused) = node
        .db()
        .call(|conn| ModRegistry::load(conn, 30_000))
        .await
        .unwrap()
        .unwrap();
    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(registry.names(), vec!["time".to_string()]);
    let entry = registry.find("time").expect("entry");
    assert_eq!(entry.descriptor.name, "time");
    assert_eq!(entry.descriptor.abi, 1);
    assert!(entry.descriptor.needs.contains(&"clock".to_string()));
}

// ---------------------------------------------------------------------------
// End to end through the pipeline
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn time_mod_end_to_end() {
    let wasm = require_wasm!();
    let node = test_node().await;
    register_time_mod(&node, wasm, &["clock"]).await;
    let refused = install_host(&node).await;
    assert!(refused.is_empty(), "{refused:?}");

    // The hello advertises the enabled mod next to the builtins.
    let hello = node.hello().await.unwrap();
    assert!(hello.mods.iter().any(|m| m == "time"), "{:?}", hello.mods);
    assert!(hello.mods.iter().any(|m| m == "sql-sqlite"));

    admit(&node, PEER).await;
    allow(&node, &peer_hex(), "*", "mod:time").await;

    let (req, id) = time_request("now");
    let frames = drive(&node, PEER, req.to_envelope()).await;
    assert_eq!(frames.len(), 3, "{frames:?}");

    let EnvelopeObject::Result(header) = &frames[0] else {
        panic!("expected Result first, got {:?}", frames[0]);
    };
    assert_eq!(header.id, id);
    assert_eq!(header.columns, vec!["now".to_string()]);

    let EnvelopeObject::Row(rows) = &frames[1] else {
        panic!("expected Row, got {:?}", frames[1]);
    };
    assert_eq!(rows.len(), 1);
    // The host renumbers the ABI's 1-based seq onto the wire's dense
    // 0-based sequence.
    assert_eq!(rows[0].seq, 0);
    assert_eq!(rows[0].cells.len(), 1);
    assert_eq!(rows[0].cells[0].0, "now");
    let Value::Text(ts) = &rows[0].cells[0].1 else {
        panic!("expected a text timestamp, got {:?}", rows[0].cells[0].1);
    };
    // "YYYY-MM-DDThh:mm:ss.nnnnnnnnnZ"
    assert_eq!(ts.len(), 30, "{ts}");
    assert_eq!(&ts[4..5], "-");
    assert_eq!(&ts[10..11], "T");
    assert!(ts.ends_with('Z'), "{ts}");
    assert!(ts.starts_with("20"), "{ts}");

    let EnvelopeObject::Done(done) = &frames[2] else {
        panic!("expected Done, got {:?}", frames[2]);
    };
    assert_eq!(done.id, id);
    assert_eq!(done.row_count, Some(1));

    // Exactly one invocation audit row, decision allow.
    let audits = audit_rows(&node, &id).await;
    assert_eq!(audits.len(), 1, "{audits:?}");
    assert_eq!(audits[0].0, "allow");
}

#[tokio::test(flavor = "multi_thread")]
async fn policy_denies_mod_action() {
    let wasm = require_wasm!();
    let node = test_node().await;
    register_time_mod(&node, wasm, &["clock"]).await;
    install_host(&node).await;
    admit(&node, PEER).await;
    // No mod:time policy row: the chain's tail denies.

    let (req, id) = time_request("now");
    let frames = drive(&node, PEER, req.to_envelope()).await;
    assert_eq!(frames.len(), 1, "{frames:?}");
    let EnvelopeObject::Denied(d) = &frames[0] else {
        panic!("expected Denied, got {:?}", frames[0]);
    };
    assert_eq!(d.id.as_deref(), Some(id.as_str()));

    let audits = audit_rows(&node, &id).await;
    assert_eq!(audits.len(), 1, "{audits:?}");
    assert_eq!(audits[0].0, "deny");
}

#[tokio::test(flavor = "multi_thread")]
async fn stranger_is_denied_before_the_plugin() {
    let wasm = require_wasm!();
    let node = test_node().await;
    register_time_mod(&node, wasm, &["clock"]).await;
    install_host(&node).await;

    let (req, _) = time_request("now");
    let frames = drive(&node, STRANGER, req.to_envelope()).await;
    assert_eq!(frames.len(), 1, "{frames:?}");
    let EnvelopeObject::Denied(d) = &frames[0] else {
        panic!("expected Denied, got {:?}", frames[0]);
    };
    assert!(d.reason.as_deref().unwrap().contains("unknown peer"));
}

// ---------------------------------------------------------------------------
// db_query capability gate and gated statement path
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn db_query_without_cap_traps_cleanly() {
    let wasm = require_wasm!();
    let node = test_node().await;
    // clock granted, db_read not.
    register_time_mod(&node, wasm, &["clock"]).await;
    install_host(&node).await;
    admit(&node, PEER).await;
    allow(&node, &peer_hex(), "*", "mod:time").await;

    let (req, id) = time_request("db");
    let frames = drive(&node, PEER, req.to_envelope()).await;
    assert_eq!(frames.len(), 1, "{frames:?}");
    let EnvelopeObject::Error(e) = &frames[0] else {
        panic!("expected Error, got {:?}", frames[0]);
    };
    assert_eq!(e.id.as_deref(), Some(id.as_str()));
    assert_eq!(e.code, "engine-error");
    assert!(
        e.reason.as_deref().unwrap().contains("db_read"),
        "{:?}",
        e.reason
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn db_query_with_cap_goes_through_the_statement_gate() {
    let wasm = require_wasm!();
    let node = test_node().await;
    register_time_mod(&node, wasm, &["clock", "db_read"]).await;
    install_host(&node).await;
    admit(&node, PEER).await;
    allow(&node, &peer_hex(), "*", "mod:time").await;

    // Without a read policy the statement itself is denied and the
    // plugin traps with the deny reason.
    let (req, id) = time_request("db");
    let frames = drive(&node, PEER, req.to_envelope()).await;
    let EnvelopeObject::Error(e) = &frames[0] else {
        panic!("expected Error, got {:?}", frames[0]);
    };
    assert!(
        e.reason.as_deref().unwrap().contains("denied"),
        "{:?}",
        e.reason
    );
    // The statement wrote its own audit row (deny), next to the
    // invocation's allow row.
    let audits = audit_rows(&node, &id).await;
    assert_eq!(audits.len(), 2, "{audits:?}");
    assert_eq!(audits[0], ("allow".to_string(), "db".to_string()));
    assert_eq!(audits[1].0, "deny");
    assert_eq!(audits[1].1, "SELECT 1 AS one");

    // With a read allow the same request answers rows.
    allow(&node, &peer_hex(), "*", "read").await;
    let (req, id) = time_request("db");
    let frames = drive(&node, PEER, req.to_envelope()).await;
    assert_eq!(frames.len(), 3, "{frames:?}");
    let EnvelopeObject::Result(header) = &frames[0] else {
        panic!("expected Result, got {:?}", frames[0]);
    };
    assert_eq!(header.columns, vec!["one".to_string()]);
    let EnvelopeObject::Row(rows) = &frames[1] else {
        panic!("expected Row, got {:?}", frames[1]);
    };
    assert_eq!(rows[0].cells, vec![("one".to_string(), Value::Integer(1))]);
    let EnvelopeObject::Done(done) = &frames[2] else {
        panic!("expected Done, got {:?}", frames[2]);
    };
    assert_eq!(done.row_count, Some(1));
    let audits = audit_rows(&node, &id).await;
    assert_eq!(audits.len(), 2, "{audits:?}");
    assert!(audits.iter().all(|(d, _)| d == "allow"), "{audits:?}");
}

// ---------------------------------------------------------------------------
// Handler-level behavior without the pipeline
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn unmatched_mod_answers_mod_unsupported() {
    let wasm = require_wasm!();
    let node = test_node().await;
    register_time_mod(&node, wasm, &["clock"]).await;
    let (host, _) = ModsHost::load(
        node.db().clone(),
        node.chain().clone(),
        node.config().clone(),
    )
    .await
    .unwrap();
    assert_eq!(host.registry().names(), vec!["time".to_string()]);

    let (ftx, mut frx) = mpsc::channel(16);
    let req = Request::new(RequestKind::Query, "weather", "now");
    host.handle(peer_hex(), req, ftx).await;
    let frame = frx.recv().await.expect("one frame");
    let EnvelopeObject::Error(e) = frame else {
        panic!("expected Error, got {frame:?}");
    };
    assert_eq!(e.code, "mod-unsupported");
}
