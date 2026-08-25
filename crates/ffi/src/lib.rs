//! Mobile bindings for the resonator node (uniffi -> Swift/Kotlin): a
//! blocking, callback-driven surface over the `rsntr` CLI library, the
//! same primitives the Python package wraps. One crate-owned
//! multi-thread tokio runtime backs every call; no extism mods host and
//! no web server ride along (both are off for mobile targets).
//!
//! Surface:
//! - `Node(dir, offline)`: create (init) or reopen a node directory.
//! - `serve()` / `stop()`, `ticket()`, `add_peer()`, `endpoint_id()`.
//! - local (the owner channel, in-process): `local_query()` /
//!   `local_execute()`, `sparql()`.
//! - remote: `query_peer()`.
//! - chat: `chat_init()`, `chat_send()`, `chat_log()`.
//! - vibrations: `entrain(point, listener)` -> `Entrainment`; the
//!   listener's `on_vibration` fires on every signal of the Sympathetic
//!   point until `damp()` or `on_end`.
//!
//! Vibration semantics mirror the owner channel: while not serving, the
//! crate keeps one in-process pipeline per `Node`, so entrainments and
//! local writes share a connection and vibrate each other. While
//! serving, every call routes to the serving pipeline instead, so
//! remote writes vibrate too. Entrainments opened before `serve()` stay
//! bound to the standalone pipeline and do not see remote writes; damp
//! and re-entrain after `serve()` when that matters.
//!
//! Every method is blocking and must not be called from the crate's own
//! callbacks (`on_vibration`/`on_end`); dispatch to another thread
//! first.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use resonator_node::Node as Pipeline;
use resonator_protocol::{Damp, Entrain, EnvelopeObject, RequestKind, Row, Value, mod_matches};
use resonator_transport::{RequestStream, TransportError};
use rsntr::channel::{self, OwnerChannel};
use rsntr::chat;
use rsntr::client::{self, QueryOutcome};
use rsntr::serve::{self, RunningNode};
use rsntr::store;

uniffi::setup_scaffolding!();

/// The one tokio runtime the bindings own; built on first use.
fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("building the resonator tokio runtime")
    })
}

// ---------------------------------------------------------------------
// Errors, values, records
// ---------------------------------------------------------------------

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    /// Connection, engine, protocol, or local failure.
    #[error("{msg}")]
    Failure { msg: String },
    /// The serving side's authenticator or policy said no.
    #[error("denied: {reason}")]
    Denied { reason: String },
}

fn fail(msg: impl Into<String>) -> FfiError {
    FfiError::Failure { msg: msg.into() }
}

fn err(e: anyhow::Error) -> FfiError {
    fail(format!("{e:#}"))
}

/// One envelope value crossing the FFI. `BlobRef` results (blobs beyond
/// the frame cap) arrive as `Text` carrying the `blake3:<hex>` hash.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiValue {
    Null,
    Integer { v: i64 },
    Real { v: f64 },
    Text { v: String },
    Blob { v: Vec<u8> },
}

fn ffi_to_value(v: FfiValue) -> Value {
    match v {
        FfiValue::Null => Value::Null,
        FfiValue::Integer { v } => Value::Integer(v),
        FfiValue::Real { v } => Value::Real(v),
        FfiValue::Text { v } => Value::Text(v),
        FfiValue::Blob { v } => Value::Blob(v),
    }
}

fn value_to_ffi(v: &Value) -> FfiValue {
    match v {
        Value::Null => FfiValue::Null,
        Value::Integer(i) => FfiValue::Integer { v: *i },
        Value::Real(f) => FfiValue::Real { v: *f },
        Value::Text(s) => FfiValue::Text { v: s.clone() },
        Value::Blob(b) => FfiValue::Blob { v: b.clone() },
        Value::BlobRef { hash, .. } => FfiValue::Text { v: hash.clone() },
    }
}

/// One result row, values aligned to the result's `columns` (NULLs
/// materialized; the wire omits them).
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiRow {
    pub values: Vec<FfiValue>,
}

fn row_to_ffi(columns: &[String], row: &Row) -> FfiRow {
    let values = columns
        .iter()
        .map(|col| {
            row.cells
                .iter()
                .find(|(name, _)| name == col)
                .map(|(_, v)| value_to_ffi(v))
                .unwrap_or(FfiValue::Null)
        })
        .collect();
    FfiRow { values }
}

/// A successful query/execute: rows plus the `rsntr:Done` trailer facts.
/// A sparql CONSTRUCT/DESCRIBE answers with `turtle` set and no rows.
#[derive(Debug, Clone, uniffi::Record)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<FfiRow>,
    pub row_count: Option<i64>,
    pub affected_rows: Option<i64>,
    pub last_insert_rowid: Option<i64>,
    pub truncated: bool,
    pub turtle: Option<String>,
}

fn outcome_to_result(outcome: QueryOutcome) -> Result<QueryResult, FfiError> {
    match outcome {
        QueryOutcome::Rows {
            columns,
            rows,
            done,
        } => {
            let rows = rows.iter().map(|r| row_to_ffi(&columns, r)).collect();
            Ok(QueryResult {
                columns,
                rows,
                row_count: done.row_count,
                affected_rows: done.affected_rows,
                last_insert_rowid: done.last_insert_rowid,
                truncated: done.truncated,
                turtle: None,
            })
        }
        QueryOutcome::Graph { triples, done } => {
            let mut text = String::new();
            for t in &triples {
                text.push_str(&format!("{t} .\n"));
            }
            Ok(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                row_count: done.row_count,
                affected_rows: done.affected_rows,
                last_insert_rowid: done.last_insert_rowid,
                truncated: done.truncated,
                turtle: Some(text),
            })
        }
        QueryOutcome::Denied(d) => Err(FfiError::Denied {
            reason: d.reason.unwrap_or_else(|| "(no reason given)".into()),
        }),
        QueryOutcome::Failed(e) => Err(fail(format!(
            "[{}] {}",
            e.code,
            e.reason.as_deref().unwrap_or("(no reason given)")
        ))),
        QueryOutcome::Help { text, .. } => Ok(QueryResult {
            columns: Vec::new(),
            rows: Vec::new(),
            row_count: None,
            affected_rows: None,
            last_insert_rowid: None,
            truncated: false,
            turtle: Some(text),
        }),
    }
}

/// One chat history entry (newest first from `chat_log`).
#[derive(Debug, Clone, uniffi::Record)]
pub struct ChatMessage {
    pub id: String,
    pub scope: String,
    pub sender: String,
    pub at: String,
    pub received_at: String,
    pub body: String,
    pub blob_hash: Option<String>,
    pub blob_name: Option<String>,
    pub outgoing: bool,
    /// `_outbox` delivery status of an outgoing message, when known.
    pub status: Option<String>,
}

/// The receipt of a `chat_send`: the send is an `_outbox` enqueue;
/// delivery happens while the node serves.
#[derive(Debug, Clone, uniffi::Record)]
pub struct SendReceipt {
    pub message_id: String,
    pub scope: String,
    pub queued_to: Vec<String>,
    pub blob_hash: Option<String>,
}

/// The request kind implied by a statement text under a modulation,
/// mirroring the CLI heuristic; the serving side derives the real kind
/// from its own footprint.
fn classify_kind(modulation: &str, signal: &str) -> RequestKind {
    if mod_matches("sparql", modulation) {
        client::classify_sparql(signal)
    } else {
        client::classify_sql(signal)
    }
}

// ---------------------------------------------------------------------
// Vibrations
// ---------------------------------------------------------------------

/// Foreign callback receiving one entrainment's signals.
#[uniffi::export(with_foreign)]
pub trait VibrationListener: Send + Sync {
    /// One vibration of the entrained point. `at` is the node's
    /// `xsd:dateTime` timestamp when given.
    fn on_vibration(&self, point: String, seq: i64, at: Option<String>);
    /// The entrainment ended (damp confirmed, node shut down, or the
    /// consumer was too slow); no further callbacks follow. `reason` is
    /// `None` for a clean end.
    fn on_end(&self, reason: Option<String>);
}

/// A live entrainment; dropping it does NOT damp (the session is held by
/// the runtime until `damp()` or node shutdown).
#[derive(uniffi::Object)]
pub struct Entrainment {
    point: String,
    damp_tx: Mutex<Option<tokio::sync::mpsc::Sender<EnvelopeObject>>>,
}

#[uniffi::export]
impl Entrainment {
    /// The entrained point IRI.
    pub fn point(&self) -> String {
        self.point.clone()
    }

    /// Ends the entrainment in-band; `on_end` fires once the node
    /// confirms. Idempotent.
    pub fn damp(&self) {
        let tx = self.damp_tx.lock().expect("damp lock").take();
        if let Some(tx) = tx {
            let _ = tx.try_send(EnvelopeObject::Damp(Damp {
                id: None,
                point: self.point.clone(),
            }));
        }
    }
}

/// The in-process server stream of one entrainment: response frames
/// become listener callbacks, the client->server side carries the damp.
struct VibrationStream {
    listener: Arc<dyn VibrationListener>,
    /// Resolves once the node acknowledges (first bare Done) or refuses.
    ready: Option<tokio::sync::oneshot::Sender<Result<(), FfiError>>>,
    inbound: tokio::sync::mpsc::Receiver<EnvelopeObject>,
    acked: bool,
    end_reason: Option<String>,
}

impl RequestStream for VibrationStream {
    async fn send(&mut self, obj: &EnvelopeObject) -> Result<(), TransportError> {
        match obj {
            EnvelopeObject::Done(_) => {
                // First Done acknowledges the entrainment; a second one
                // confirms a damp (the handler ends right after).
                if let Some(tx) = self.ready.take() {
                    self.acked = true;
                    let _ = tx.send(Ok(()));
                }
            }
            EnvelopeObject::Vibration(v) => {
                self.listener
                    .on_vibration(v.point.clone(), v.seq, v.at.clone());
            }
            EnvelopeObject::Denied(d) => {
                let reason = d
                    .reason
                    .clone()
                    .unwrap_or_else(|| "entrainment denied".into());
                match self.ready.take() {
                    Some(tx) => {
                        let _ = tx.send(Err(FfiError::Denied { reason }));
                    }
                    None => self.end_reason = Some(reason),
                }
            }
            EnvelopeObject::Error(e) => {
                let reason = format!(
                    "[{}] {}",
                    e.code,
                    e.reason.as_deref().unwrap_or("(no reason given)")
                );
                match self.ready.take() {
                    Some(tx) => {
                        let _ = tx.send(Err(fail(reason)));
                    }
                    None => self.end_reason = Some(reason),
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<EnvelopeObject>, TransportError> {
        Ok(self.inbound.recv().await)
    }

    async fn finish(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------
// Node
// ---------------------------------------------------------------------

/// A resonator node: one directory holding the sqlite database and the
/// ed25519 key. Constructing initializes the directory when needed.
#[derive(uniffi::Object)]
pub struct Node {
    dir: PathBuf,
    offline: bool,
    running: Mutex<Option<RunningNode>>,
    /// The standalone in-process pipeline used while not serving; built
    /// lazily, kept so entrainments and writes share its connection.
    local: Mutex<Option<Arc<Pipeline>>>,
}

impl Node {
    /// The pipeline every call routes through: the serving one while
    /// serving, the kept standalone one otherwise.
    fn pipeline(&self) -> Result<Arc<Pipeline>, FfiError> {
        if let Some(r) = self.running.lock().expect("running lock").as_ref() {
            return Ok(r.node().clone());
        }
        let mut guard = self.local.lock().expect("local lock");
        if let Some(node) = guard.as_ref() {
            return Ok(node.clone());
        }
        let ch = OwnerChannel::local(&self.dir).map_err(err)?;
        let node = ch
            .node()
            .expect("a local owner channel has a pipeline")
            .clone();
        // The in-process pipeline signals entrainments only once the
        // hook is on its connection (Node::run does this when serving).
        rt().block_on(node.install_vibration_hook())
            .map_err(|e| fail(format!("installing the vibration hook: {e}")))?;
        *guard = Some(node.clone());
        Ok(node)
    }

    /// Runs one owner-channel statement on the routed pipeline.
    fn owner_run(
        &self,
        kind: RequestKind,
        modulation: &str,
        signal: &str,
        params: Vec<FfiValue>,
    ) -> Result<QueryResult, FfiError> {
        let node = self.pipeline()?;
        let values: Vec<Value> = params.into_iter().map(ffi_to_value).collect();
        let outcome = rt()
            .block_on(async move {
                let ch = OwnerChannel::from_node(node);
                channel::run(&ch, kind, modulation, signal, values).await
            })
            .map_err(err)?;
        outcome_to_result(outcome)
    }
}

#[uniffi::export]
impl Node {
    /// Opens (initializing on first use) the node directory. `offline`
    /// binds serving to localhost with no relays (tests, LAN demos).
    #[uniffi::constructor]
    pub fn new(dir: String, offline: bool) -> Result<Arc<Self>, FfiError> {
        let dir = PathBuf::from(dir);
        if store::node_id(&dir).is_err() {
            store::init_dir(&dir).map_err(err)?;
        }
        Ok(Arc::new(Node {
            dir,
            offline,
            running: Mutex::new(None),
            local: Mutex::new(None),
        }))
    }

    /// This node's 64-hex endpoint id.
    pub fn endpoint_id(&self) -> Result<String, FfiError> {
        Ok(store::node_id(&self.dir).map_err(err)?.to_string())
    }

    /// Path to the node's sqlite database (WAL; readable while serving).
    pub fn db_path(&self) -> String {
        store::db_path(&self.dir).to_string_lossy().into_owned()
    }

    /// Whether this node is currently serving.
    pub fn is_serving(&self) -> bool {
        self.running.lock().expect("running lock").is_some()
    }

    /// Starts serving on the crate's background runtime; returns the
    /// direct socket addresses. Subsequent calls route to the serving
    /// pipeline, so remote writes vibrate local entrainments.
    pub fn serve(&self) -> Result<Vec<String>, FfiError> {
        if self.is_serving() {
            return Err(fail("node is already serving"));
        }
        let running = rt()
            .block_on(serve::start_node(&self.dir, self.offline))
            .map_err(err)?;
        let addrs = running
            .direct_addrs()
            .iter()
            .map(|a| a.to_string())
            .collect();
        *self.running.lock().expect("running lock") = Some(running);
        Ok(addrs)
    }

    /// Stops serving (a no-op when not serving).
    pub fn stop(&self) {
        let running = self.running.lock().expect("running lock").take();
        if let Some(r) = running {
            rt().block_on(r.shutdown());
        }
    }

    /// A shareable dialing ticket for the live serving endpoint, waiting
    /// up to `wait_ms` for a direct address so the ticket is dialable
    /// immediately. Requires `serve()` first.
    pub fn ticket(&self, wait_ms: u64) -> Result<String, FfiError> {
        let guard = self.running.lock().expect("running lock");
        let Some(r) = guard.as_ref() else {
            return Err(fail(
                "node is not serving; call serve() first (a ticket names the live endpoint)",
            ));
        };
        Ok(rt().block_on(r.ready_ticket(Duration::from_millis(wait_ms))))
    }

    /// Registers a peer under `name`. The target may be a dialing ticket
    /// or a 64-hex endpoint id. Returns the peer's endpoint id.
    pub fn add_peer(&self, name: String, ticket_or_id: String) -> Result<String, FfiError> {
        let (id, _addrs) = store::peer_add(&self.dir, &name, &ticket_or_id, &[]).map_err(err)?;
        Ok(id.to_string())
    }

    /// Runs `signal` on this node over the owner channel: no peer gate,
    /// no authenticator chain, footprint-collected and audited, DDL
    /// permitted. Classified Query/Execute from the text.
    pub fn local_query(
        &self,
        signal: String,
        params: Vec<FfiValue>,
    ) -> Result<QueryResult, FfiError> {
        let kind = classify_kind("sql-sqlite", &signal);
        self.owner_run(kind, "sql-sqlite", &signal, params)
    }

    /// [`local_query`] forced to a write (`rsntr:Execute`).
    pub fn local_execute(
        &self,
        signal: String,
        params: Vec<FfiValue>,
    ) -> Result<QueryResult, FfiError> {
        self.owner_run(RequestKind::Execute, "sql-sqlite", &signal, params)
    }

    /// Runs a SPARQL text against this node's own store (owner channel).
    /// SELECT/ASK answer rows; CONSTRUCT/DESCRIBE answer `turtle`;
    /// updates answer `affected_rows`.
    pub fn sparql(&self, query: String) -> Result<QueryResult, FfiError> {
        let kind = classify_kind("sparql", &query);
        self.owner_run(kind, "sparql", &query, Vec::new())
    }

    /// Sends `signal` to `peer` (petname or 64-hex id) under
    /// `modulation` (usually "sql-sqlite" or "sparql"), classified
    /// Query/Execute from the text.
    pub fn query_peer(
        &self,
        peer: String,
        signal: String,
        modulation: String,
        params: Vec<FfiValue>,
        timeout_ms: Option<i64>,
    ) -> Result<QueryResult, FfiError> {
        let kind = classify_kind(&modulation, &signal);
        let values: Vec<Value> = params.into_iter().map(ffi_to_value).collect();
        let report = rt()
            .block_on(client::run_statement(
                &self.dir,
                &peer,
                kind,
                &modulation,
                &signal,
                values,
                self.offline,
                timeout_ms,
            ))
            .map_err(err)?;
        outcome_to_result(report.outcome)
    }

    // --- chat ---

    /// Scaffolds chat on this node (tables, projection points, policy);
    /// idempotent.
    pub fn chat_init(&self) -> Result<(), FfiError> {
        chat::chat_init(&self.dir).map_err(err)?;
        Ok(())
    }

    /// Sends a chat message to `target` (peer petname, 64-hex id, room
    /// name, or room IRI). The send is an `_outbox` enqueue; delivery
    /// happens while this node serves.
    pub fn chat_send(&self, target: String, text: String) -> Result<SendReceipt, FfiError> {
        let report = rt()
            .block_on(chat::chat_send(&self.dir, &target, &text, None))
            .map_err(err)?;
        Ok(SendReceipt {
            message_id: report.message_id,
            scope: report.scope,
            queued_to: report.queued_to,
            blob_hash: report.blob.map(|(hash, _)| hash),
        })
    }

    /// Reads chat history, newest first. `scope` filters to one
    /// conversation (peer or room); `None` reads all.
    pub fn chat_log(
        &self,
        scope: Option<String>,
        limit: i64,
    ) -> Result<Vec<ChatMessage>, FfiError> {
        read_chat_log(&self.dir, scope.as_deref(), limit).map_err(err)
    }

    /// Nudges the outbox worker to scan `_outbox` now. No-op when not
    /// serving.
    pub fn wake_outbox(&self) {
        if let Some(r) = self.running.lock().expect("running lock").as_ref() {
            r.wake_outbox();
        }
    }

    // --- vibrations ---

    /// Entrains a Sympathetic `point` (its IRI, from `_projection`):
    /// `listener.on_vibration` fires on every signal until `damp()` or
    /// `on_end`. Owner lane: any registered Sympathetic point works, no
    /// policy filter. Blocks until the node acknowledges.
    pub fn entrain(
        &self,
        point: String,
        listener: Arc<dyn VibrationListener>,
    ) -> Result<Arc<Entrainment>, FfiError> {
        let node = self.pipeline()?;
        let id = ulid::Ulid::new().to_string();
        let envelope = EnvelopeObject::Entrain(Entrain {
            id,
            point: point.clone(),
        });
        let (damp_tx, inbound) = tokio::sync::mpsc::channel(4);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let mut stream = VibrationStream {
            listener: listener.clone(),
            ready: Some(ready_tx),
            inbound,
            acked: false,
            end_reason: None,
        };
        rt().spawn(async move {
            let result = node.handle_owner(envelope, &mut stream).await;
            if stream.acked {
                let reason = match result {
                    Ok(()) => stream.end_reason.take(),
                    Err(e) => Some(e.to_string()),
                };
                stream.listener.on_end(reason);
            }
        });
        match rt().block_on(async { tokio::time::timeout(Duration::from_secs(10), ready_rx).await })
        {
            Ok(Ok(Ok(()))) => Ok(Arc::new(Entrainment {
                point,
                damp_tx: Mutex::new(Some(damp_tx)),
            })),
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_)) => Err(fail("entrainment ended before it was acknowledged")),
            Err(_) => Err(fail("timed out waiting for the entrain acknowledgement")),
        }
    }
}

/// Reads chat history newest first straight off the database (read-only;
/// outgoing rows join their `_outbox` delivery status).
fn read_chat_log(dir: &Path, scope: Option<&str>, limit: i64) -> anyhow::Result<Vec<ChatMessage>> {
    let conn = store::open_db(dir)?;
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'chat_messages'",
        [],
        |r| r.get(0),
    )?;
    if n == 0 {
        anyhow::bail!("chat is not scaffolded here; call chat_init() first");
    }
    // A given scope may be a petname, a room name/IRI, or a 64-hex id.
    let resolved = match scope {
        None => None,
        Some(s) => Some(chat::resolve_target(&conn, s)?.scope()),
    };
    let mut stmt = conn.prepare(
        "SELECT m.id, m.scope, m.sender, m.at, m.received_at, m.body, \
                m.blob_hash, m.blob_name, m.outgoing, o.status \
         FROM chat_messages m LEFT JOIN _outbox o ON o.request_id = m.id \
         WHERE (?1 IS NULL OR m.scope = ?1) \
         ORDER BY m.received_at DESC, m.id DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map((resolved, limit), |r| {
            Ok(ChatMessage {
                id: r.get(0)?,
                scope: r.get(1)?,
                sender: r.get(2)?,
                at: r.get(3)?,
                received_at: r.get(4)?,
                body: r.get(5)?,
                blob_hash: r.get(6)?,
                blob_name: r.get(7)?,
                outgoing: r.get::<_, i64>(8)? != 0,
                status: r.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
