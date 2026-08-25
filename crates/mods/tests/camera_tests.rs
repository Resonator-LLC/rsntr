//! End-to-end tests for the camera mod (examples/camera-mod), the
//! config-only hologram companion: hologram chunk streaming, the
//! sources verb from `_modulations.config`, and the error paths. The
//! media byte streams themselves are the builtin media lane, covered by
//! the node pipeline tests; this mod never touches them.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine as _;

use resonator_authenticator::Chain;
use resonator_mods::{ModsHost, mod_add, mod_set_enabled};
use resonator_node::{DbHandle, Node, NodeConfig, open_node_db_in_memory, seed_rsntr_defaults};
use resonator_protocol::{EnvelopeObject, Generic, MAX_FRAME_LEN, Request, RequestKind, Value};
use resonator_transport::{IncomingRequest, PeerId, RequestStream, TransportError, basic_hello};

const APP_HTML: &str = include_str!("../../../examples/camera-mod/app/index.html");

fn camera_wasm() -> Option<&'static [u8]> {
    static WASM: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    WASM.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/camera-mod");
        let artifact = root.join("target/wasm32-unknown-unknown/release/camera_mod.wasm");
        let build = std::process::Command::new("cargo")
            .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
            .current_dir(&root)
            .output();
        match build {
            Ok(out) if out.status.success() => {}
            Ok(out) => eprintln!(
                "camera-mod build failed (falling back to any existing artifact):\n{}",
                String::from_utf8_lossy(&out.stderr)
            ),
            Err(e) => eprintln!("camera-mod build could not run: {e}"),
        }
        std::fs::read(&artifact).ok()
    })
    .as_deref()
}

macro_rules! require_wasm {
    () => {
        match camera_wasm() {
            Some(w) => w,
            None => {
                eprintln!(
                    "SKIPPED: camera_mod.wasm unavailable (needs the \
                     wasm32-unknown-unknown target; run `cargo build --release \
                     --target wasm32-unknown-unknown` in examples/camera-mod)"
                );
                return;
            }
        }
    };
}

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

const VIEWER: [u8; 32] = [3u8; 32];

/// Node with the camera mod registered under the given config JSON.
async fn camera_node(wasm: &'static [u8], config: Option<&str>) -> Arc<Node> {
    let conn = open_node_db_in_memory().expect("open db");
    seed_rsntr_defaults(&conn).expect("seed defaults");
    let node = Arc::new(Node::new(
        DbHandle::spawn(conn),
        Chain::with_builtin_tiers(),
        NodeConfig::default(),
    ));
    let config = config.map(str::to_string);
    node.db()
        .call(move |conn| {
            mod_add(conn, "cameras", wasm, &[], None).expect("mod add");
            assert!(mod_set_enabled(conn, "cameras", true).expect("enable"));
            if let Some(cfg) = config {
                conn.execute(
                    "UPDATE _modulations SET config = ?1 WHERE name = 'cameras'",
                    [cfg],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT OR IGNORE INTO _peers (endpoint_id, added_at) VALUES (?1, 'now')",
                [PeerId(VIEWER).to_string()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
                 VALUES ('*', '*', 'mod:cameras', 'allow')",
                [],
            )
            .unwrap();
        })
        .await
        .unwrap();
    let refused = ModsHost::install(&node).await.expect("mods host load");
    assert!(refused.is_empty(), "{refused:?}");
    node
}

async fn drive(node: &Node, first: EnvelopeObject) -> Vec<EnvelopeObject> {
    let out = Recorded::default();
    let stream = LocalStream {
        out: out.clone(),
        incoming: VecDeque::new(),
    };
    node.handle(IncomingRequest {
        peer: PeerId(VIEWER),
        peer_hello: basic_hello(&["help"], None),
        first,
        stream,
    })
    .await
    .expect("handle");
    out.sent.lock().unwrap().clone()
}

fn generic<'f>(frame: &'f EnvelopeObject, class: &str) -> Option<&'f Generic> {
    match frame {
        EnvelopeObject::Generic(g) if g.class == class => Some(g),
        _ => None,
    }
}

fn prop<'g>(g: &'g Generic, local: &str) -> Option<&'g str> {
    let want = format!("#{local}");
    g.props.iter().find_map(|(p, t)| {
        if !p.as_str().ends_with(&want) {
            return None;
        }
        match t {
            oxrdf::Term::Literal(l) => Some(l.value()),
            _ => None,
        }
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn hologram_stream_reassembles_under_the_frame_cap() {
    let wasm = require_wasm!();
    let node = camera_node(
        wasm,
        Some(r#"{"cameras":[{"name":"door","label":"entrance"}]}"#),
    )
    .await;

    let req = Request::new(RequestKind::Query, "cameras", "hologram");
    let frames = drive(&node, req.to_envelope()).await;

    let header = generic(&frames[0], "Hologram")
        .unwrap_or_else(|| panic!("expected a Hologram header, got {:?}", frames[0]));
    assert_eq!(
        prop(header, "contentType"),
        Some("text/html; charset=utf-8")
    );
    let mut body: Vec<u8> = Vec::new();
    for frame in &frames[1..frames.len() - 1] {
        let chunk = generic(frame, "Chunk").expect("chunk frames");
        assert!(frame.to_turtle().expect("serialize").len() <= MAX_FRAME_LEN);
        body.extend(
            base64::engine::general_purpose::STANDARD
                .decode(prop(chunk, "data").expect("data"))
                .expect("base64"),
        );
    }
    assert_eq!(body, APP_HTML.as_bytes());
    assert!(matches!(frames.last(), Some(EnvelopeObject::Done(_))));
}

#[tokio::test(flavor = "multi_thread")]
async fn sources_come_from_the_mod_config() {
    let wasm = require_wasm!();
    let node = camera_node(
        wasm,
        Some(r#"{"cameras":[{"name":"door","label":"entrance door"},{"name":"nvr/39"}]}"#),
    )
    .await;

    let req = Request::new(RequestKind::Query, "cameras", "sources");
    let frames = drive(&node, req.to_envelope()).await;
    let EnvelopeObject::Result(header) = &frames[0] else {
        panic!("expected Result, got {:?}", frames[0]);
    };
    assert_eq!(
        header.columns,
        vec!["name".to_string(), "label".to_string(), "talk".to_string()]
    );
    let rows: Vec<resonator_protocol::Row> = frames
        .iter()
        .filter_map(|f| match f {
            EnvelopeObject::Row(rs) => Some(rs.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(rows.len(), 2);
    let cell = |i: usize, name: &str| {
        rows[i]
            .cells
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    assert_eq!(cell(0, "name"), Value::Text("door".into()));
    assert_eq!(cell(0, "label"), Value::Text("entrance door".into()));
    // A camera without a label falls back to its name.
    assert_eq!(cell(1, "name"), Value::Text("nvr/39".into()));
    assert_eq!(cell(1, "label"), Value::Text("nvr/39".into()));
}

#[tokio::test(flavor = "multi_thread")]
async fn unconfigured_and_unknown_signals_error() {
    let wasm = require_wasm!();
    let node = camera_node(wasm, None).await;

    let req = Request::new(RequestKind::Query, "cameras", "sources");
    let frames = drive(&node, req.to_envelope()).await;
    let EnvelopeObject::Error(e) = &frames[0] else {
        panic!("expected Error, got {:?}", frames[0]);
    };
    assert_eq!(e.code, "engine-error");
    assert!(
        e.reason
            .as_deref()
            .unwrap()
            .contains("no cameras configured")
    );

    let req = Request::new(RequestKind::Query, "cameras", "ptz");
    let frames = drive(&node, req.to_envelope()).await;
    let EnvelopeObject::Error(e) = &frames[0] else {
        panic!("expected Error, got {:?}", frames[0]);
    };
    assert_eq!(e.code, "protocol-error");

    let req = Request::new(RequestKind::Query, "cameras", "hologram style.css");
    let frames = drive(&node, req.to_envelope()).await;
    let EnvelopeObject::Error(e) = &frames[0] else {
        panic!("expected Error, got {:?}", frames[0]);
    };
    assert_eq!(e.code, "point-unknown");
}
