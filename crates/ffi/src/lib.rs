//! Mobile bindings for the resonator node (uniffi -> Swift/Kotlin): a
//! blocking, callback-driven surface over the `rsntr` CLI library, the
//! same primitives the Python package wraps. One crate-owned
//! multi-thread tokio runtime backs every call; no extism mods host
//! rides along (off for mobile targets), and the web console server is
//! opt-in via the `web` feature (`serve_web()`) for WebView shells.
//!
//! Surface:
//! - `Node(dir, offline)` / `Node.open(dir, offline, app, policy)`:
//!   create (init) or reopen a node directory, registering the app.
//! - `serve()` / `stop()`, `ticket()`, `add_peer()`, `knock()`,
//!   `endpoint_id()`.
//! - lifecycle for a shared directory (docs/shared-node.md): `close()`,
//!   `suspend()`, `resume()`, `serving_info()`, `is_shared_serving()`,
//!   `current_route()`; `apps()`, `declare_tables()`.
//! - local (the owner channel): `local_query()` / `local_execute()`,
//!   `sparql()`.
//! - remote: `query_peer()`.
//! - chat: `chat_init()`, `chat_send()`, `chat_log()`.
//! - vibrations: `entrain(point, listener)` -> `Entrainment`; the
//!   listener's `on_vibration` fires on every signal of the Sympathetic
//!   point until `damp()` or `on_end`.
//!
//! Several apps may share one node directory; at most one process
//! serves it (the lease, `rsntr.serve.lock`). Every call routes by what
//! is true at that moment: to this process's own serving pipeline when
//! it holds the lease; over the owner channel (unix socket or loopback
//! TCP) when another process serves; otherwise to an in-process
//! pipeline of this `Node`. Vibrations are live in every case: the
//! serving pipeline and the in-process pipeline both watch for commits
//! made by other processes and signal their entrainments, and a socket
//! entrainment is pushed by the serving process itself. When the
//! serving process goes away, a socket entrainment ends with
//! `on_end("server gone; re-entrain")`: call `entrain` again (it routes
//! fresh), optionally after `resume()` to take the lease over.
//!
//! Callbacks run on a dispatcher thread owned by the `Node`, never on
//! the runtime, so a listener may call `Node` methods. `suspend()`
//! never waits for a running callback.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use resonator_node::Node as Pipeline;
use resonator_protocol::{
    Damp, Entrain, EntrainmentEvent, EnvelopeObject, RequestKind, Row, Value, mod_matches,
};
use resonator_transport::{RequestStream, TransportError};
use rsntr::channel::{self, OwnerChannel};
use rsntr::chat;
use rsntr::client::{self, QueryOutcome};
use rsntr::serve::{self, RunningNode};
use rsntr::{lease, owner, store};

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
    /// Another process holds the directory's serving lease; `holder`
    /// names it when its record is readable. Not an error for an app
    /// that is content to be a client: use the owner channel.
    #[error("another process is already serving this node")]
    AlreadyServing { holder: Option<ServingInfo> },
}

/// Who an app is (recorded in the serving lease and in `_apps`).
#[derive(Debug, Clone, uniffi::Record)]
pub struct AppInfo {
    /// Reverse-DNS id or a slug (`org.example.notebook`).
    pub id: String,
    pub name: Option<String>,
    pub version: Option<String>,
}

/// A table an app declares as its own, with its description.
#[derive(Debug, Clone, uniffi::Record)]
pub struct TableDecl {
    pub name: String,
    pub comment: String,
}

/// One `_apps` row.
#[derive(Debug, Clone, uniffi::Record)]
pub struct AppRecord {
    pub app_id: String,
    pub name: String,
    pub version: Option<String>,
    pub lib_version: String,
    pub platform: String,
    pub tables: Vec<String>,
    pub first_seen: String,
    pub last_seen: String,
    pub note: Option<String>,
}

/// Who holds the directory's serving lease.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ServingInfo {
    pub pid: u32,
    pub app_id: String,
    pub app_name: Option<String>,
    pub app_version: Option<String>,
    pub started_at: String,
    /// True when this very process holds it.
    pub is_self: bool,
}

/// What `resume()` does about serving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ServePolicy {
    /// Take the lease when nobody holds it (the foreground app serves).
    ServeIfFree,
    /// Never serve; always a client of whoever does (or in-process).
    ClientOnly,
}

/// Where a call would go right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RouteKind {
    /// This process serves the directory.
    Serving,
    /// Another process serves it; the owner channel carries the call.
    Socket,
    /// Nobody serves it; this `Node`'s in-process pipeline.
    Local,
}

fn serving_info_of(h: &lease::LeaseInfo) -> ServingInfo {
    ServingInfo {
        pid: h.pid,
        app_id: h.app.id.clone(),
        app_name: h.app.name.clone(),
        app_version: h.app.version.clone(),
        started_at: h.started_at.clone(),
        is_self: h.pid == std::process::id(),
    }
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

/// The answer to a knock (connection-protocol.md section 4). `decision`
/// is `"allow"` (admitted — talk normally now), `"pending"` (parked for
/// the owner), `"deny"`, or `None` when the peer answered nothing at all
/// (knocks are rate-limited and dropped in silence).
#[derive(Debug, Clone, uniffi::Record)]
pub struct KnockResult {
    pub id: String,
    pub decision: Option<String>,
    pub decided_by: Option<String>,
    pub reason: Option<String>,
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
// Callback dispatch
// ---------------------------------------------------------------------

type Callback = Box<dyn FnOnce() + Send + 'static>;

/// One thread per `Node` that runs every listener callback, so a
/// callback never executes on the tokio runtime (it may block, it may
/// call `Node` methods) and `suspend()` never has to wait for one.
#[derive(Clone)]
struct Dispatcher {
    tx: std::sync::mpsc::Sender<Callback>,
}

impl Dispatcher {
    fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Callback>();
        std::thread::Builder::new()
            .name("rsntr-callbacks".into())
            .spawn(move || {
                while let Ok(cb) = rx.recv() {
                    cb();
                }
            })
            .expect("spawning the callback thread");
        Self { tx }
    }

    fn run(&self, cb: impl FnOnce() + Send + 'static) {
        let _ = self.tx.send(Box::new(cb));
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
    /// The entrainment ended (damp confirmed, node shut down, the
    /// serving process went away, the consumer was too slow, or this
    /// `Node` closed); no further callbacks follow. `reason` is `None`
    /// for a clean end, `"closed"` after `close()`/`suspend()`, and
    /// `"server gone; re-entrain"` when the serving process left.
    fn on_end(&self, reason: Option<String>);
}

/// A live entrainment; dropping it does NOT damp (the session is held by
/// the runtime until `damp()`, node shutdown, or `close()`).
#[derive(uniffi::Object)]
pub struct Entrainment {
    point: String,
    damp_tx: Mutex<Option<tokio::sync::mpsc::Sender<EnvelopeObject>>>,
    /// Set by `close()`/`suspend()` so the end reports "closed".
    closing: Arc<AtomicBool>,
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
    dispatch: Dispatcher,
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
                let listener = self.listener.clone();
                let (point, seq, at) = (v.point.clone(), v.seq, v.at.clone());
                self.dispatch
                    .run(move || listener.on_vibration(point, seq, at));
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
// Host-published media
// ---------------------------------------------------------------------

/// Told when the number of viewers of a published feed changes. An
/// encoder uses it to force a keyframe when someone joins, without which
/// a new viewer waits for the next one.
#[uniffi::export(with_foreign)]
pub trait MediaViewerListener: Send + Sync {
    fn on_viewers(&self, count: u32);
}

/// A media source this app publishes: write encoded frames into it and
/// every viewer of that source receives them.
///
/// Writes never block. A viewer that cannot keep up misses chunks and
/// resumes at the newest ones, because the alternative on a reliable
/// ordered stream is worse: the viewer stalls the encoder and lag grows
/// without a ceiling. Feed it whole encoded units (a fragmented-MP4
/// segment, an encoded frame), not arbitrary slices.
#[derive(uniffi::Object)]
pub struct MediaFeed {
    name: String,
    writer: Mutex<Option<resonator_node::FeedWriter>>,
    feeds: Arc<resonator_node::MediaFeeds>,
}

#[uniffi::export]
impl MediaFeed {
    /// The source name viewers ask for.
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// Hands one chunk to every viewer; returns how many received it.
    /// Zero is not an error: a camera may run before anyone looks.
    pub fn write(&self, chunk: Vec<u8>) -> u32 {
        match self.writer.lock().expect("feed lock").as_ref() {
            Some(w) => w.write(&chunk) as u32,
            None => 0,
        }
    }

    /// How many viewers are watching right now.
    pub fn viewers(&self) -> u32 {
        match self.writer.lock().expect("feed lock").as_ref() {
            Some(w) => w.viewers() as u32,
            None => 0,
        }
    }

    /// Ends the feed: viewers' streams close and the source stops being
    /// servable until it is published again. Idempotent.
    pub fn close(&self) {
        if self.writer.lock().expect("feed lock").take().is_some() {
            self.feeds.withdraw(&self.name);
        }
    }
}

// ---------------------------------------------------------------------
// Node
// ---------------------------------------------------------------------

/// Where a call goes (see the module docs).
enum Route {
    Serving(Arc<Pipeline>),
    Socket,
    Local(Arc<Pipeline>),
}

/// A resonator node: one directory holding the sqlite database and the
/// ed25519 key, possibly shared with other apps. Constructing
/// initializes the directory when needed.
#[derive(uniffi::Object)]
pub struct Node {
    dir: PathBuf,
    offline: bool,
    app: Option<AppInfo>,
    policy: ServePolicy,
    running: Mutex<Option<RunningNode>>,
    /// The in-process pipeline used while nobody serves; built lazily,
    /// kept so entrainments and writes share its connection, with the
    /// foreign-write watcher running. Dropped by `close()`.
    local: Mutex<Option<Arc<Pipeline>>>,
    /// Every entrainment handed out, so `close()` can end them.
    entrainments: Mutex<Vec<Weak<Entrainment>>>,
    dispatch: Dispatcher,
    /// The loopback web console while it serves. Lock order: `web`
    /// before `running` before `local`, always.
    #[cfg(feature = "web")]
    web: Mutex<Option<resonator_web::WebServer>>,
}

impl Node {
    fn app_node(&self) -> Option<resonator_node::AppInfo> {
        self.app.as_ref().map(|a| resonator_node::AppInfo {
            id: a.id.clone(),
            name: a.name.clone(),
            version: a.version.clone(),
        })
    }

    fn app_lease(&self) -> lease::AppInfo {
        match &self.app {
            Some(a) => lease::AppInfo {
                id: a.id.clone(),
                name: a.name.clone(),
                version: a.version.clone(),
            },
            None => lease::AppInfo::new("network.resonator.app"),
        }
    }

    /// The in-process pipeline, built on first use: the vibration hook
    /// on its connection and the foreign-write watcher running.
    async fn local_pipeline(&self) -> Result<Arc<Pipeline>, FfiError> {
        if let Some(node) = self.local.lock().expect("local lock").as_ref() {
            return Ok(node.clone());
        }
        let ch = OwnerChannel::local(&self.dir).map_err(err)?;
        let node = ch
            .node()
            .expect("a local owner channel has a pipeline")
            .clone();
        node.install_vibration_hook()
            .await
            .map_err(|e| fail(format!("installing the vibration hook: {e}")))?;
        node.start_change_watcher().await;
        let existing = {
            let mut guard = self.local.lock().expect("local lock");
            match guard.as_ref() {
                Some(existing) => Some(existing.clone()),
                None => {
                    *guard = Some(node.clone());
                    None
                }
            }
        };
        if let Some(existing) = existing {
            // Raced with another caller; theirs wins, ours is closed.
            let _ = node.db().close().await;
            return Ok(existing);
        }
        Ok(node)
    }

    async fn route(&self) -> Result<Route, FfiError> {
        if let Some(r) = self.running.lock().expect("running lock").as_ref() {
            return Ok(Route::Serving(r.node().clone()));
        }
        if owner::probe(&self.dir).await {
            return Ok(Route::Socket);
        }
        Ok(Route::Local(self.local_pipeline().await?))
    }

    async fn channel(&self) -> Result<OwnerChannel, FfiError> {
        Ok(match self.route().await? {
            Route::Serving(node) | Route::Local(node) => OwnerChannel::from_node(node),
            Route::Socket => OwnerChannel::socket(&self.dir),
        })
    }

    /// Runs one owner-channel statement wherever the route leads.
    fn owner_run(
        &self,
        kind: RequestKind,
        modulation: &str,
        signal: &str,
        params: Vec<FfiValue>,
    ) -> Result<QueryResult, FfiError> {
        let values: Vec<Value> = params.into_iter().map(ffi_to_value).collect();
        let outcome = rt().block_on(async {
            let ch = self.channel().await?;
            channel::run(&ch, kind, modulation, signal, values)
                .await
                .map_err(err)
        })?;
        outcome_to_result(outcome)
    }

    /// Registers the app in `_apps` over the current route.
    async fn register_app(&self) -> Result<(), FfiError> {
        let Some(app) = self.app_node() else {
            return Ok(());
        };
        let ch = self.channel().await?;
        rsntr::appscmd::register_over(&ch, &app).await.map_err(err)
    }

    /// Ends the in-process pipeline: every entrainment on it, its
    /// watcher, and its connection (the sqlite locks with it).
    fn close_local(&self) {
        let local = self.local.lock().expect("local lock").take();
        if let Some(node) = local {
            node.entrainments().close_all();
            node.stop_change_watcher();
            let _ = rt().block_on(node.db().close());
        }
    }

    fn remember(&self, e: &Arc<Entrainment>) {
        let mut all = self.entrainments.lock().expect("entrainments lock");
        all.retain(|w| w.strong_count() > 0);
        all.push(Arc::downgrade(e));
    }
}

#[uniffi::export]
impl Node {
    /// Opens (initializing on first use) the node directory. `offline`
    /// binds serving to localhost with no relays (tests, LAN demos). The
    /// app stays unregistered; see [`open`](Self::open).
    #[uniffi::constructor]
    pub fn new(dir: String, offline: bool) -> Result<Arc<Self>, FfiError> {
        Self::open(dir, offline, None, ServePolicy::ServeIfFree)
    }

    /// Opens (initializing on first use) a possibly shared node
    /// directory as `app`, registering it in `_apps`, with the serving
    /// policy `resume()` follows. Safe for several apps starting on one
    /// empty directory at once (one identity results).
    #[uniffi::constructor]
    pub fn open(
        dir: String,
        offline: bool,
        app: Option<AppInfo>,
        policy: ServePolicy,
    ) -> Result<Arc<Self>, FfiError> {
        let dir = PathBuf::from(dir);
        store::ensure_dir_initialized(&dir).map_err(err)?;
        let node = Arc::new(Node {
            dir,
            offline,
            app,
            policy,
            running: Mutex::new(None),
            local: Mutex::new(None),
            entrainments: Mutex::new(Vec::new()),
            dispatch: Dispatcher::new(),
            #[cfg(feature = "web")]
            web: Mutex::new(None),
        });
        rt().block_on(node.register_app())?;
        Ok(node)
    }

    /// This node's 64-hex endpoint id.
    pub fn endpoint_id(&self) -> Result<String, FfiError> {
        Ok(store::node_id(&self.dir).map_err(err)?.to_string())
    }

    /// Path to the node's sqlite database (WAL; readable while serving).
    pub fn db_path(&self) -> String {
        store::db_path(&self.dir).to_string_lossy().into_owned()
    }

    /// Whether this process is currently serving the directory.
    pub fn is_serving(&self) -> bool {
        self.running.lock().expect("running lock").is_some()
    }

    /// Starts serving on the crate's background runtime (takes the
    /// directory's serving lease); returns the direct socket addresses.
    /// `AlreadyServing` names the process that holds the lease.
    pub fn serve(&self) -> Result<Vec<String>, FfiError> {
        if self.is_serving() {
            return Err(fail("node is already serving"));
        }
        let running = rt()
            .block_on(serve::start_node_as(
                &self.dir,
                self.offline,
                self.app_lease(),
            ))
            .map_err(|e| match e.downcast_ref::<lease::LeaseError>() {
                Some(lease::LeaseError::AlreadyServing { holder, .. }) => {
                    FfiError::AlreadyServing {
                        holder: holder.as_deref().map(serving_info_of),
                    }
                }
                _ => err(e),
            })?;
        let addrs = running
            .direct_addrs()
            .iter()
            .map(|a| a.to_string())
            .collect();
        *self.running.lock().expect("running lock") = Some(running);
        Ok(addrs)
    }

    /// Stops serving (a no-op when not serving): releases the lease, the
    /// endpoint, the blob store, and the serving connection. The web
    /// console, which holds clones of the serving pipeline, goes down
    /// first.
    pub fn stop(&self) {
        #[cfg(feature = "web")]
        self.stop_web();
        self.withdraw_published();
        let running = self.running.lock().expect("running lock").take();
        if let Some(r) = running {
            rt().block_on(r.shutdown());
        }
    }

    /// Ends every entrainment (`on_end("closed")`) and drops the
    /// in-process pipeline, so this `Node` holds no database handle.
    /// Serving is untouched; see [`suspend`](Self::suspend). Idempotent.
    pub fn close(&self) {
        let all: Vec<Arc<Entrainment>> = self
            .entrainments
            .lock()
            .expect("entrainments lock")
            .drain(..)
            .filter_map(|w| w.upgrade())
            .collect();
        for e in &all {
            e.closing.store(true, Ordering::SeqCst);
            e.damp();
        }
        self.close_local();
    }

    /// Lets go of the directory entirely: the web console, serving (the
    /// lease, endpoint, blob store, serving connection), every
    /// entrainment, and the in-process pipeline. After this the process
    /// holds no handle on the node directory, which is what an app must
    /// be able to say before it is suspended (iOS terminates a suspended
    /// app holding locks on files outside its own container). Never
    /// waits for a running callback.
    pub fn suspend(&self) {
        // Mark first: stopping the serving pipeline ends its
        // entrainments, and they must report "closed", not a clean end.
        for e in self
            .entrainments
            .lock()
            .expect("entrainments lock")
            .iter()
            .filter_map(|w| w.upgrade())
        {
            e.closing.store(true, Ordering::SeqCst);
        }
        self.stop();
        self.close();
    }

    /// Ends every feed this process publishes (serving stops with
    /// `stop()`, which ends their viewers anyway; this also frees the
    /// declarations so a later publish starts clean).
    fn withdraw_published(&self) {
        if let Some(r) = self.running.lock().expect("running lock").as_ref() {
            let feeds = r.node().media_feeds();
            for name in feeds.published() {
                feeds.withdraw(&name);
            }
        }
    }

    /// Comes back after [`suspend`](Self::suspend): takes the serving
    /// lease when the policy is `ServeIfFree` and nobody holds it (a
    /// held lease is not an error), refreshes the app's `last_seen`, and
    /// reports who serves now (`None` when nobody does). Pipelines
    /// rebuild lazily on the next call; re-entrain what matters.
    pub fn resume(&self) -> Result<Option<ServingInfo>, FfiError> {
        if self.policy == ServePolicy::ServeIfFree && !self.is_serving() {
            match self.serve() {
                Ok(_) | Err(FfiError::AlreadyServing { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        if let Some(app) = &self.app
            && let Ok(conn) = store::open_db(&self.dir)
        {
            let _ = resonator_node::touch_app(&conn, &app.id, 300);
        }
        Ok(self.serving_info())
    }

    /// Who holds the directory's serving lease right now, if anyone.
    pub fn serving_info(&self) -> Option<ServingInfo> {
        lease::holder(&self.dir).map(|h| serving_info_of(&h))
    }

    /// True when another process serves the directory.
    pub fn is_shared_serving(&self) -> bool {
        self.serving_info().is_some_and(|i| !i.is_self)
    }

    /// Where a call would go right now.
    pub fn current_route(&self) -> Result<RouteKind, FfiError> {
        Ok(match rt().block_on(self.route())? {
            Route::Serving(_) => RouteKind::Serving,
            Route::Socket => RouteKind::Socket,
            Route::Local(_) => RouteKind::Local,
        })
    }

    /// Every app registered in the node.
    pub fn apps(&self) -> Result<Vec<AppRecord>, FfiError> {
        let conn = store::open_db(&self.dir).map_err(err)?;
        let rows = resonator_node::list_apps(&conn).map_err(|e| fail(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| AppRecord {
                app_id: r.app_id,
                name: r.name,
                version: r.version,
                lib_version: r.lib_version,
                platform: r.platform,
                tables: r.tables,
                first_seen: r.first_seen,
                last_seen: r.last_seen,
                note: r.note,
            })
            .collect())
    }

    /// Declares tables this app owns (they must exist): a `_comments`
    /// description each plus the ownership record, so other apps and
    /// peers can discover them. Needs a node opened with `AppInfo`.
    pub fn declare_tables(&self, tables: Vec<TableDecl>) -> Result<(), FfiError> {
        let Some(app) = self.app_node() else {
            return Err(fail("open the node with AppInfo before declaring tables"));
        };
        let decls: Vec<resonator_node::TableDecl> = tables
            .into_iter()
            .map(|t| resonator_node::TableDecl {
                name: t.name,
                comment: t.comment,
            })
            .collect();
        let statements = {
            let conn = store::open_db(&self.dir).map_err(err)?;
            resonator_node::declare_statements(&conn, &app.id, &decls)
                .map_err(|e| fail(e.to_string()))?
        };
        rt().block_on(async {
            let ch = self.channel().await?;
            rsntr::appscmd::register_over(&ch, &app)
                .await
                .map_err(err)?;
            for (sql, params) in statements {
                channel::execute(&ch, &sql, params.into_iter().map(Value::Text).collect())
                    .await
                    .map_err(err)?;
            }
            Ok(())
        })
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
        // `timeout_ms` rides to the peer as a request option the engine
        // honours; nothing bounds the call on this side, and a peer that
        // stalls mid-handler (a mod waiting on a slow upstream) would pin
        // the caller's thread forever. Give the wire itself a deadline
        // with headroom for the dial. Built entirely inside the runtime:
        // the client binds its endpoint as the future is created.
        let deadline = match timeout_ms {
            Some(ms) if ms > 0 => Some(Duration::from_millis(ms as u64 + 5_000)),
            _ => None,
        };
        let dir = self.dir.clone();
        let offline = self.offline;
        let report = rt().block_on(async move {
            let statement = client::run_statement(
                &dir,
                &peer,
                kind,
                &modulation,
                &signal,
                values,
                offline,
                timeout_ms,
            );
            match deadline {
                Some(deadline) => match tokio::time::timeout(deadline, statement).await {
                    Ok(r) => r.map_err(err),
                    Err(_) => Err(fail(format!(
                        "no answer from {peer} within {} ms",
                        deadline.as_millis()
                    ))),
                },
                None => statement.await.map_err(err),
            }
        })?;
        outcome_to_result(report.outcome)
    }

    /// Knocks on `peer` (petname, ticket, or 64-hex id) with `message`:
    /// the one frame an unadmitted key may send. `allow` means this node
    /// is now in the peer's `_peers` table and may talk normally. A
    /// stranger the peer's policy refuses outright answers `Denied`.
    pub fn knock(&self, peer: String, message: String) -> Result<KnockResult, FfiError> {
        let report = rt()
            .block_on(client::knock(&self.dir, &peer, &message, self.offline))
            .map_err(|e| {
                let text = format!("{e:#}");
                match text.strip_prefix("denied: ") {
                    Some(reason) => FfiError::Denied {
                        reason: reason.to_string(),
                    },
                    None => fail(text),
                }
            })?;
        Ok(KnockResult {
            id: report.id,
            decision: report.decision.as_ref().map(|d| d.decision.clone()),
            decided_by: report.decision.as_ref().map(|d| d.decided_by.clone()),
            reason: report.decision.and_then(|d| d.reason),
        })
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
    /// happens while some process serves the directory.
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

    // --- media ---

    /// Publishes a media source this app produces: declares it in
    /// `_media` (empty command, meaning host-provided) and returns the
    /// feed to write encoded frames into. This is how a node inside a
    /// phone serves its camera, where no shell command could.
    ///
    /// Requires this process to be serving the node: a media request
    /// lands on whichever process holds the serving lease, so only that
    /// one can answer it. Publishing grants nothing by itself — a peer
    /// still needs a `_policy` row allowing it the `media` action on
    /// this source name.
    pub fn publish_media(
        &self,
        name: String,
        content_type: String,
        listener: Option<Arc<dyn MediaViewerListener>>,
    ) -> Result<Arc<MediaFeed>, FfiError> {
        if name.trim().is_empty() {
            return Err(fail("a media source needs a name"));
        }
        let running = self.running.lock().expect("running lock");
        let Some(r) = running.as_ref() else {
            return Err(fail(match self.serving_info() {
                Some(info) => format!(
                    "this process does not serve the node ({} does, pid {}); a media \
                     source can only be published by the process that serves it",
                    info.app_id, info.pid
                ),
                None => "the node is not serving; call serve() before publishing a media source"
                    .to_string(),
            }));
        };
        let node = r.node().clone();
        drop(running);

        // The row is the declaration and the policy anchor; an empty
        // command is what marks it host-provided.
        let (n, ct) = (name.clone(), content_type.clone());
        rt().block_on(async {
            let ch = OwnerChannel::from_node(node.clone());
            channel::execute(
                &ch,
                "INSERT INTO _media (name, command, content_type) VALUES (?1, '', ?2) \
                 ON CONFLICT(name) DO UPDATE SET command = '', content_type = ?2",
                vec![Value::Text(n), Value::Text(ct)],
            )
            .await
            .map_err(err)
        })?;

        let writer = node.media_feeds().publish(&name, &content_type);
        if let Some(listener) = listener {
            let dispatch = self.dispatch.clone();
            writer.set_viewer_observer(Some(Arc::new(move |count: usize| {
                let listener = listener.clone();
                dispatch.run(move || listener.on_viewers(count as u32));
            })));
        }
        Ok(Arc::new(MediaFeed {
            name,
            writer: Mutex::new(Some(writer)),
            feeds: node.media_feeds().clone(),
        }))
    }

    /// The media sources this process is publishing right now.
    pub fn published_media(&self) -> Vec<String> {
        match self.running.lock().expect("running lock").as_ref() {
            Some(r) => r.node().media_feeds().published(),
            None => Vec::new(),
        }
    }

    /// Nudges the outbox worker to scan `_outbox` now. No-op when this
    /// process is not serving (another server notices the write itself).
    pub fn wake_outbox(&self) {
        if let Some(r) = self.running.lock().expect("running lock").as_ref() {
            r.wake_outbox();
        }
    }

    // --- vibrations ---

    /// Entrains a Sympathetic `point` (its IRI, from `_projection`):
    /// `listener.on_vibration` fires on every signal until `damp()` or
    /// `on_end`. Owner lane: any registered Sympathetic point works, no
    /// policy filter. Routed like every call: pushed by this process's
    /// serving pipeline, by the other process serving the directory
    /// (over the owner channel), or by this `Node`'s in-process pipeline.
    /// Blocks until the node acknowledges.
    pub fn entrain(
        &self,
        point: String,
        listener: Arc<dyn VibrationListener>,
    ) -> Result<Arc<Entrainment>, FfiError> {
        let (damp_tx, damp_rx) = tokio::sync::mpsc::channel(4);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let closing = Arc::new(AtomicBool::new(false));
        let dispatch = self.dispatch.clone();
        match rt().block_on(self.route())? {
            Route::Socket => {
                let dir = self.dir.clone();
                let point = point.clone();
                let closing = closing.clone();
                rt().spawn(socket_entrainment(
                    dir, point, listener, dispatch, ready_tx, damp_rx, closing,
                ));
            }
            Route::Serving(node) | Route::Local(node) => {
                let id = ulid::Ulid::new().to_string();
                let envelope = EnvelopeObject::Entrain(Entrain {
                    id,
                    point: point.clone(),
                });
                let mut stream = VibrationStream {
                    listener: listener.clone(),
                    dispatch: dispatch.clone(),
                    ready: Some(ready_tx),
                    inbound: damp_rx,
                    acked: false,
                    end_reason: None,
                };
                let closing = closing.clone();
                rt().spawn(async move {
                    let result = node.handle_owner(envelope, &mut stream).await;
                    if stream.acked {
                        let reason = if closing.load(Ordering::SeqCst) {
                            Some("closed".to_string())
                        } else {
                            match result {
                                Ok(()) => stream.end_reason.take(),
                                Err(e) => Some(e.to_string()),
                            }
                        };
                        let listener = stream.listener.clone();
                        dispatch.run(move || listener.on_end(reason));
                    }
                });
            }
        }
        match rt().block_on(async { tokio::time::timeout(Duration::from_secs(10), ready_rx).await })
        {
            Ok(Ok(Ok(()))) => {
                let e = Arc::new(Entrainment {
                    point,
                    damp_tx: Mutex::new(Some(damp_tx)),
                    closing,
                });
                self.remember(&e);
                Ok(e)
            }
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_)) => Err(fail("entrainment ended before it was acknowledged")),
            Err(_) => Err(fail("timed out waiting for the entrain acknowledgement")),
        }
    }
}

/// An entrainment pushed by another process over the owner channel.
async fn socket_entrainment(
    dir: PathBuf,
    point: String,
    listener: Arc<dyn VibrationListener>,
    dispatch: Dispatcher,
    ready_tx: tokio::sync::oneshot::Sender<Result<(), FfiError>>,
    mut damp_rx: tokio::sync::mpsc::Receiver<EnvelopeObject>,
    closing: Arc<AtomicBool>,
) {
    let (mut stream, mut reader, id) = match owner::entrain(&dir, &point).await {
        Ok(x) => x,
        Err(e) => {
            let _ = ready_tx.send(Err(err(e)));
            return;
        }
    };
    let mut ready = Some(ready_tx);
    let mut acked = false;
    let end_reason: Option<String>;
    let mut damping = false;
    let mut damp_deadline = tokio::time::Instant::now() + Duration::from_secs(3600 * 24 * 365);
    loop {
        tokio::select! {
            frame = stream.recv() => {
                let event = match frame {
                    Ok(Some(f)) => match reader.accept(f) {
                        Ok(ev) => ev,
                        Err(e) => {
                            end_reason = Some(format!("entrainment choreography: {e}"));
                            break;
                        }
                    },
                    Ok(None) | Err(_) => {
                        end_reason = Some("server gone; re-entrain".to_string());
                        break;
                    }
                };
                match event {
                    EntrainmentEvent::Entrained => {
                        acked = true;
                        if let Some(tx) = ready.take() {
                            let _ = tx.send(Ok(()));
                        }
                    }
                    EntrainmentEvent::Vibration(v) => {
                        let listener = listener.clone();
                        let (p, seq, at) = (v.point.clone(), v.seq, v.at.clone());
                        dispatch.run(move || listener.on_vibration(p, seq, at));
                    }
                    EntrainmentEvent::Damped => {
                        end_reason = None;
                        break;
                    }
                    EntrainmentEvent::Denied(d) => {
                        let reason = d.reason.unwrap_or_else(|| "entrainment denied".into());
                        match ready.take() {
                            Some(tx) => {
                                let _ = tx.send(Err(FfiError::Denied { reason }));
                                return;
                            }
                            None => {
                                end_reason = Some(reason);
                                break;
                            }
                        }
                    }
                    EntrainmentEvent::Error(e) => {
                        let reason = format!(
                            "[{}] {}",
                            e.code,
                            e.reason.as_deref().unwrap_or("(no reason given)")
                        );
                        match ready.take() {
                            Some(tx) => {
                                let _ = tx.send(Err(fail(reason)));
                                return;
                            }
                            None => {
                                end_reason = Some(reason);
                                break;
                            }
                        }
                    }
                }
            }
            damp = damp_rx.recv(), if !damping => {
                // A Damp request (or every handle dropped): ask the server
                // to confirm, and give up on it after two seconds (closing
                // the connection is damping too).
                let _ = damp;
                damping = true;
                damp_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
                let _ = stream
                    .send(&EnvelopeObject::Damp(Damp {
                        id: Some(id.clone()),
                        point: point.clone(),
                    }))
                    .await;
            }
            _ = tokio::time::sleep_until(damp_deadline), if damping => {
                end_reason = None;
                break;
            }
        }
    }
    if acked {
        let reason = if closing.load(Ordering::SeqCst) {
            Some("closed".to_string())
        } else {
            end_reason
        };
        dispatch.run(move || listener.on_end(reason));
    }
}

/// The loopback web console (docs/web-api.md) for WebView shells: the
/// serving pipeline plus the axum server on 127.0.0.1, capability token
/// persisted in the node directory (`rsntr.web-token`).
#[cfg(feature = "web")]
#[uniffi::export]
impl Node {
    /// Starts the web console on an ephemeral loopback port. Requires
    /// `serve()` first. Returns the entry URL with the capability token
    /// in the fragment, ready for a WebView to load. `rotate_token`
    /// mints a replacement `rsntr.web-token`.
    pub fn serve_web(&self, rotate_token: bool) -> Result<String, FfiError> {
        let mut web = self.web.lock().expect("web lock");
        if web.is_some() {
            return Err(fail("web console is already serving"));
        }
        let running = self.running.lock().expect("running lock");
        let Some(r) = running.as_ref() else {
            return Err(fail("node is not serving; call serve() first"));
        };
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
        let server = rt()
            .block_on(serve::start_web(r, &self.dir, addr, rotate_token))
            .map_err(err)?;
        let url = server.url();
        *web = Some(server);
        Ok(url)
    }

    /// The console entry URL (fragment token included) while the web
    /// console serves, `None` otherwise.
    pub fn web_url(&self) -> Option<String> {
        self.web.lock().expect("web lock").as_ref().map(|s| s.url())
    }

    /// Stops the web console only (a no-op when it is not serving).
    pub fn stop_web(&self) {
        let server = self.web.lock().expect("web lock").take();
        if let Some(s) = server {
            rt().block_on(s.shutdown());
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
