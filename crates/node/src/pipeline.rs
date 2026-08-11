//! The serving pipeline.
//!
//! Order of battle for one SQL request:
//!
//! peer gate -> collect-mode prepare (the footprint) -> authenticator
//! chain -> enforce-mode re-prepare -> execution inside a per-request
//! transaction with limits armed -> Result/Row/Done streamed out ->
//! an `_audit` row no matter what.
//!
//! The sqlite connection lives on its dedicated thread behind
//! [`DbHandle`]; each SQL request costs two jobs there: one for
//! gate/prepare/decide, one for execute. The execute job hands frames to
//! the request stream through a tokio channel.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::hooks::{AuthContext, Authorization};
use rusqlite::types::ToSqlOutput;
use rusqlite::{Connection, ErrorCode as SqliteErrorCode, OptionalExtension, ToSql};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use resonator_authenticator::{ActionKind, Chain, Decision, Footprint};
use resonator_protocol::{
    Decision as DecisionEnvelope, Denied, Done, EnvelopeObject, ErrorCode, ErrorEnvelope, Graph,
    Help, Request, ResultHeader, Row, Value, mod_matches,
};
use resonator_transport::{IncomingRequest, RequestStream};

use crate::audit::{audit_direct, audit_direct_dir, audit_full, audit_full_dir, audit_outcome};
use crate::clock::{now_rfc3339, unix_now_f64};
use crate::db::DbHandle;
use crate::entrain::{EntrainGate, Entrainments, gate_entrain, gate_entrain_owner};
use crate::error::NodeError;
use crate::footprint::{Approved, CollectState, Lane, collect_authorizer, enforce_authorizer};
use crate::help::build_help;
use crate::mod_handler::ModHandler;
use crate::projection::{ProjectionOutcome, build_projection};
use crate::sparql_mod::{
    SparqlOutcome, gate_and_run_sparql, run_sparql_owner, term_value, triple_estimate,
};

/// VDBE instructions per progress-handler callback.
const PROGRESS_GRANULARITY: i32 = 500;

/// How long an audio-duplex source may stay silent after the caller has
/// finished sending before the session ends (docs/rdf-envelope-protocol.md
/// sec 4.3). Bounds the half-open phase so a hung-up call always releases
/// its source, whatever the transport reports.
const HALF_OPEN_IDLE: Duration = Duration::from_secs(5);

/// A hook that runs on the db thread in the gap between decide and
/// execute. This is a deliberate seam: tests (and future
/// instrumentation) get exactly one place to watch or disturb the
/// database inside the window the enforce-mode authorizer protects.
pub type PostDecideHook = Arc<dyn Fn(&mut Connection) + Send + Sync>;

/// Server-side pipeline settings: the ceilings that clamp every
/// request's `RequestOptions`, plus batching knobs.
#[derive(Clone)]
pub struct NodeConfig {
    /// Row ceiling; `rsntr:rowLimit` clamps to this.
    pub max_rows: i64,
    /// Response byte ceiling; `rsntr:byteLimit` clamps to this.
    pub max_response_bytes: i64,
    /// Wall-clock ceiling; `rsntr:timeoutMs` clamps to this.
    pub max_duration_ms: u64,
    /// Total VDBE steps one statement may spend (the cpu meter).
    pub vdbe_step_budget: u64,
    /// Rows per Row-batch frame, at most.
    pub rows_per_frame: usize,
    /// Soft per-frame byte budget (comfortably inside the 256 KiB frame
    /// cap).
    pub frame_byte_budget: usize,
    /// PRAGMAs requests may run; all others are denied. Default empty.
    pub pragma_allowlist: Arc<Vec<String>>,
    /// Knock rate limits: per-key and global token buckets.
    pub knock_limits: KnockLimits,
    /// How long one vibration send may stall before the node damps the
    /// entrainment with `limit-exceeded` (the slow-consumer rule).
    pub vibration_send_timeout_ms: u64,
    /// Test seam; see [`PostDecideHook`].
    pub post_decide_hook: Option<PostDecideHook>,
}

/// Persisted token-bucket settings for knock rate limiting.
///
/// Every stranger knock passes two buckets: one keyed by the knocking
/// endpoint id and one global. Processing happens only when both hold a
/// whole token; if either is empty the knock is dropped silently and not
/// audited, so a burst inside one window costs one audit row rather than
/// one per attempt. Refill is by elapsed wall-clock time.
#[derive(Clone, Copy, Debug)]
pub struct KnockLimits {
    /// Per-key bucket capacity (largest burst from one key).
    pub per_key_burst: f64,
    /// Per-key refill, tokens per second.
    pub per_key_refill_per_sec: f64,
    /// Global bucket capacity (largest burst across all keys).
    pub global_burst: f64,
    /// Global refill, tokens per second.
    pub global_refill_per_sec: f64,
}

impl Default for KnockLimits {
    fn default() -> Self {
        // Deliberately harsh (knocks are the spam surface): one knock
        // per key per minute; at most 20 outstanding globally, refilling
        // ten per minute.
        Self {
            per_key_burst: 1.0,
            per_key_refill_per_sec: 1.0 / 60.0,
            global_burst: 20.0,
            global_refill_per_sec: 10.0 / 60.0,
        }
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            max_rows: 100_000,
            max_response_bytes: 8 * 1024 * 1024,
            max_duration_ms: 30_000,
            vdbe_step_budget: 50_000_000,
            rows_per_frame: 128,
            frame_byte_budget: 96 * 1024,
            pragma_allowlist: Arc::new(Vec::new()),
            knock_limits: KnockLimits::default(),
            vibration_send_timeout_ms: 10_000,
            post_decide_hook: None,
        }
    }
}

/// A callback registered via [`Node::set_table_observer`].
type TableObserver = Arc<dyn Fn(&str) + Send + Sync>;

/// A callback registered via [`Node::set_blob_importer`]: imports one
/// local file into the serving process's blob store and resolves to
/// `(64-hex blake3 hash, byte size)` or an error message. Wired by the
/// serving wrapper (the CLI) from its transport's live store; used by the
/// owner channel's chat attachment import (docs/owner-channel.md sec 5.3).
pub type BlobImporter = Arc<
    dyn Fn(
            std::path::PathBuf,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(String, u64), String>> + Send>,
        > + Send
        + Sync,
>;

/// The serving node: one database (one [`DbHandle`]), one authenticator
/// chain, and the pipeline between them. Wire it to the accept side of a
/// [`Transport`](resonator_transport::Transport) by feeding it the
/// transport's `IncomingRequest`s.
pub struct Node {
    db: DbHandle,
    chain: Arc<Chain>,
    config: Arc<NodeConfig>,
    entrainments: Arc<Entrainments>,
    /// Extra per-commit table observer folded into the one sqlite
    /// `update_hook` (owned by the vibration hook). The outbox worker
    /// parks its wake here so both surfaces can share the single slot.
    table_observer: Arc<Mutex<Option<TableObserver>>>,
    /// Extra-modulation provider (the extism mods host); requests whose
    /// modulation none of its mods match still answer mod-unsupported.
    mod_handler: Mutex<Option<Arc<dyn ModHandler>>>,
    /// Blob-store import hook for the owner channel's chat attachment
    /// path parameter; `None` when no store is wired (not serving).
    blob_importer: Mutex<Option<BlobImporter>>,
}

impl Node {
    pub fn new(db: DbHandle, chain: Chain, config: NodeConfig) -> Self {
        Self {
            db,
            chain: Arc::new(chain),
            config: Arc::new(config),
            entrainments: Arc::new(Entrainments::new()),
            table_observer: Arc::new(Mutex::new(None)),
            mod_handler: Mutex::new(None),
            blob_importer: Mutex::new(None),
        }
    }

    /// Registers the blob-store importer the owner channel's chat
    /// attachment path uses. Replaces any previous importer.
    pub fn set_blob_importer(&self, f: BlobImporter) {
        *self
            .blob_importer
            .lock()
            .expect("blob importer lock poisoned") = Some(f);
    }

    /// Registers the handler serving modulations beyond the builtins
    /// (see [`ModHandler`]). Replaces any previous handler.
    pub fn set_mod_handler(&self, handler: Box<dyn ModHandler>) {
        *self.mod_handler.lock().expect("mod handler lock poisoned") = Some(Arc::from(handler));
    }

    /// The authenticator chain (shared with co-hosted surfaces like the
    /// mods host, so every path decides identically).
    pub fn chain(&self) -> &Arc<Chain> {
        &self.chain
    }

    /// The pipeline settings (shared ceilings for co-hosted surfaces).
    pub fn config(&self) -> &Arc<NodeConfig> {
        &self.config
    }

    /// Registers an extra observer, called on the db thread with the
    /// name of each changed table, next to the vibration signalling.
    /// sqlite gives a connection one `update_hook`, so co-hosted
    /// surfaces (the outbox worker's wake, say) plug in here rather than
    /// installing their own. Works before or after the hook exists.
    pub fn set_table_observer(&self, f: impl Fn(&str) + Send + Sync + 'static) {
        *self
            .table_observer
            .lock()
            .expect("table observer lock poisoned") = Some(Arc::new(f));
    }

    /// The database handle (also how tests reach the db thread).
    pub fn db(&self) -> &DbHandle {
        &self.db
    }

    /// The entrainment registry (test observability).
    pub fn entrainments(&self) -> &Arc<Entrainments> {
        &self.entrainments
    }

    /// This node's hello, assembled from `_rsntr`.
    pub async fn hello(&self) -> Result<resonator_protocol::Hello, NodeError> {
        self.db.call(|conn| crate::ddl::node_hello(conn)).await
    }

    /// Installs the sqlite `update_hook` that maps commits to vibration
    /// signals: any table change signals entrainments on points whose
    /// `resource` is that table, and a `_projection` or `_policy` change
    /// additionally signals `urn:rsntr:projection-changed`. [`run`]
    /// (Self::run) calls this; tests driving [`handle`](Self::handle)
    /// directly must call it themselves.
    pub async fn install_vibration_hook(&self) -> Result<(), NodeError> {
        let ent = self.entrainments.clone();
        let observer = self.table_observer.clone();
        self.db
            .call(move |conn| {
                let _ = conn.update_hook(Some(
                    move |_a: rusqlite::hooks::Action, _db: &str, table: &str, _rowid: i64| {
                        if table == "_projection" || table == "_policy" {
                            ent.signal_point(resonator_protocol::vocab::PROJECTION_CHANGED);
                        }
                        ent.signal_table(table);
                        let extra = observer
                            .lock()
                            .expect("table observer lock poisoned")
                            .clone();
                        if let Some(f) = extra {
                            f(table);
                        }
                    },
                ));
            })
            .await
    }

    /// Serves until the transport drops the channel; every request runs
    /// concurrently on its own task.
    pub async fn run<S>(self: Arc<Self>, mut incoming: mpsc::Receiver<IncomingRequest<S>>)
    where
        S: RequestStream + 'static,
    {
        if let Err(e) = self.install_vibration_hook().await {
            warn!(error = %e, "failed to install the vibration update hook");
        }
        while let Some(inc) = incoming.recv().await {
            let node = self.clone();
            tokio::spawn(async move {
                if let Err(e) = node.handle(inc).await {
                    debug!(error = %e, "request handling failed");
                }
            });
        }
    }

    /// One request end to end: respond on the stream, audit, half-close
    /// the send side.
    pub async fn handle<S: RequestStream>(&self, inc: IncomingRequest<S>) -> Result<(), NodeError> {
        let peer = inc.peer.to_string();
        let mut stream = inc.stream;
        let result = self.dispatch(&peer, inc.first, &mut stream).await;
        let _ = stream.finish().await;
        result
    }

    async fn dispatch<S: RequestStream>(
        &self,
        peer: &str,
        first: EnvelopeObject,
        stream: &mut S,
    ) -> Result<(), NodeError> {
        match first {
            // Help is a pre-admission affordance for everyone. It is
            // caught before Request::from_envelope (its text is not SQL)
            // and before the peer gate (strangers may ask).
            EnvelopeObject::Query(st) if mod_matches("help", &st.modulation) => {
                self.serve_help(peer, st.id, st.signal, stream).await
            }
            // Projection mirrors help: pre-peer-gate, so a stranger sees
            // a policy-filtered (possibly empty) public projection.
            EnvelopeObject::Query(st) if mod_matches("projection", &st.modulation) => {
                self.serve_projection_lane(peer, st.id, st.signal, stream, Lane::Remote)
                    .await
            }
            EnvelopeObject::Entrain(e) => {
                self.serve_entrain_lane(peer, e, stream, Lane::Remote).await
            }
            // Media stays behind the peer gate (unlike help): a feed
            // opens only for admitted peers with a `_policy` media
            // allow.
            EnvelopeObject::Query(st) if mod_matches("media", &st.modulation) => {
                self.serve_media(peer, st, stream).await
            }
            // Audio-duplex is media's two-way sibling, same gate shape,
            // its own policy action.
            EnvelopeObject::Query(st) if mod_matches("audio-duplex", &st.modulation) => {
                self.serve_audio_duplex(peer, st, stream).await
            }
            EnvelopeObject::Query(_) | EnvelopeObject::Execute(_) => {
                let request = match Request::from_envelope(&first) {
                    Ok(r) => r,
                    Err(e) => {
                        return send_error(stream, None, ErrorCode::ProtocolError, e.to_string())
                            .await;
                    }
                };
                if mod_matches("chat", &request.modulation) {
                    self.serve_chat(peer, request, stream).await
                } else if mod_matches("sparql", &request.modulation) {
                    self.serve_sparql(peer, request, stream).await
                } else if mod_matches("sql-sqlite", &request.modulation) {
                    self.serve_sql(peer, request, stream).await
                } else if let Some(handler) = self.matching_mod_handler(&request.modulation) {
                    self.serve_mod(peer, request, handler, stream).await
                } else {
                    // The transport's mod gate normally fast-fails
                    // first; this covers direct drivers and stale
                    // hellos.
                    send_error(
                        stream,
                        Some(request.id_string()),
                        ErrorCode::ModUnsupported,
                        format!(
                            "modulation {:?} is not served by this node",
                            request.modulation
                        ),
                    )
                    .await
                }
            }
            EnvelopeObject::Knock(k) => self.serve_knock(peer, k.id, k.message, stream).await,
            other => {
                send_error(
                    stream,
                    None,
                    ErrorCode::ProtocolError,
                    format!("frame is not a request: {other:?}"),
                )
                .await
            }
        }
    }

    /// The owner channel's dispatch entry (docs/owner-channel.md): one
    /// ordinary request envelope from the node owner, responses streamed
    /// on `stream`. No peer gate and no authenticator chain; the
    /// footprint is still collected (for the ledger), every outcome is
    /// audited with `direction = 'local'` and `decided_by = 'owner'`,
    /// DDL/PRAGMA/transaction control are permitted (ATTACH/DETACH and
    /// load_extension stay banned), and the `NodeConfig` resource limits
    /// clamp exactly as on the remote path. The peer identity is the
    /// node's own endpoint id from `_rsntr`, never the caller's claim.
    pub async fn handle_owner<S: RequestStream>(
        &self,
        first: EnvelopeObject,
        stream: &mut S,
    ) -> Result<(), NodeError> {
        let own = self
            .db
            .call(|conn| crate::ddl::get_rsntr(conn, "endpoint_id").unwrap_or_default())
            .await?;
        let result = self.dispatch_owner(&own, first, stream).await;
        let _ = stream.finish().await;
        result
    }

    async fn dispatch_owner<S: RequestStream>(
        &self,
        peer: &str,
        first: EnvelopeObject,
        stream: &mut S,
    ) -> Result<(), NodeError> {
        match first {
            EnvelopeObject::Query(st) if mod_matches("help", &st.modulation) => {
                self.serve_help(peer, st.id, st.signal, stream).await
            }
            EnvelopeObject::Query(st) if mod_matches("projection", &st.modulation) => {
                self.serve_projection_lane(peer, st.id, st.signal, stream, Lane::Owner)
                    .await
            }
            EnvelopeObject::Entrain(e) => {
                self.serve_entrain_lane(peer, e, stream, Lane::Owner).await
            }
            // The media raw byte feed is not served on the owner channel
            // in v1 (docs/owner-channel.md sec 3.2); no CLI command needs
            // it locally.
            EnvelopeObject::Query(st) if mod_matches("media", &st.modulation) => {
                send_error(
                    stream,
                    Some(st.id),
                    ErrorCode::ModUnsupported,
                    "the media raw feed is not served on the owner channel in v1".to_string(),
                )
                .await
            }
            EnvelopeObject::Query(st) if mod_matches("audio-duplex", &st.modulation) => {
                send_error(
                    stream,
                    Some(st.id),
                    ErrorCode::ModUnsupported,
                    "audio-duplex is not served on the owner channel; use the web surface"
                        .to_string(),
                )
                .await
            }
            EnvelopeObject::Query(_) | EnvelopeObject::Execute(_) => {
                let request = match Request::from_envelope(&first) {
                    Ok(r) => r,
                    Err(e) => {
                        return send_error(stream, None, ErrorCode::ProtocolError, e.to_string())
                            .await;
                    }
                };
                if mod_matches("chat", &request.modulation) {
                    self.serve_chat_owner(peer, request, stream).await
                } else if mod_matches("sparql", &request.modulation) {
                    self.serve_sparql_owner(peer, request, stream).await
                } else if mod_matches("sql-sqlite", &request.modulation) {
                    self.serve_sql_owner(peer, request, stream).await
                } else if let Some(handler) = self.matching_mod_handler(&request.modulation) {
                    // A mod does not inherit owner powers: the owner
                    // invoked it, but its internal db_query/db_execute
                    // statements remain chain-decided as on the remote
                    // path (docs/owner-channel.md sec 4).
                    self.serve_mod(peer, request, handler, stream).await
                } else {
                    send_error(
                        stream,
                        Some(request.id_string()),
                        ErrorCode::ModUnsupported,
                        format!(
                            "modulation {:?} is not served by this node",
                            request.modulation
                        ),
                    )
                    .await
                }
            }
            other => {
                send_error(
                    stream,
                    None,
                    ErrorCode::ProtocolError,
                    format!(
                        "the owner channel accepts rsntr:Query, rsntr:Execute, or \
                         rsntr:Entrain; got {other:?}"
                    ),
                )
                .await
            }
        }
    }

    /// sql-sqlite on the owner channel: footprint collected under the
    /// owner ban set, no chain, `allow`/`owner`/`local` audit, execution
    /// under the same limits as the remote path.
    async fn serve_sql_owner<S: RequestStream>(
        &self,
        peer: &str,
        request: Request,
        stream: &mut S,
    ) -> Result<(), NodeError> {
        let id = request.id_string();
        let verdict = {
            let peer = peer.to_string();
            let req = request.clone();
            let allowlist = self.config.pragma_allowlist.clone();
            self.db
                .call(move |conn| screen_owner_request(conn, &peer, &req, &allowlist))
                .await?
        };
        match verdict {
            Verdict::UnknownPeer => unreachable!("the owner channel has no peer gate"),
            Verdict::Banned { reason } | Verdict::Denied { reason } => {
                send_denied(stream, Some(id), reason).await
            }
            Verdict::PrepareFailed { message } => {
                send_error(stream, Some(id), ErrorCode::EngineError, message).await
            }
            Verdict::Cleared {
                approved,
                exec_sql,
                audit_id,
            } => {
                self.execute_and_stream(request, approved, exec_sql, audit_id, stream)
                    .await
            }
        }
    }

    /// sparql on the owner channel: no chain, `allow`/`owner`/`local`
    /// audit, same row caps.
    async fn serve_sparql_owner<S: RequestStream>(
        &self,
        peer: &str,
        request: Request,
        stream: &mut S,
    ) -> Result<(), NodeError> {
        let id = request.id_string();
        let limits = Limits::effective(&request.options, &self.config);
        let outcome = {
            let peer = peer.to_string();
            let req = request.clone();
            let row_cap = limits.row_cap;
            self.db
                .call(move |conn| run_sparql_owner(conn, &peer, &req, row_cap))
                .await?
        };
        self.stream_sparql_outcome(id, outcome, stream).await
    }

    /// chat on the owner channel: the local-append leg. A single text
    /// parameter is the owner-channel-only attachment source path
    /// (docs/owner-channel.md sec 5.3): the serving process, which holds
    /// the blob store lock, imports the file before the append.
    async fn serve_chat_owner<S: RequestStream>(
        &self,
        peer: &str,
        request: Request,
        stream: &mut S,
    ) -> Result<(), NodeError> {
        let id = request.id_string();
        match request.params.as_slice() {
            [] => {}
            [Value::Text(path)] => {
                let importer = self
                    .blob_importer
                    .lock()
                    .expect("blob importer lock poisoned")
                    .clone();
                let Some(importer) = importer else {
                    return send_error(
                        stream,
                        Some(id),
                        ErrorCode::EngineError,
                        "no blob store is wired on this node; import the attachment \
                         in-process instead"
                            .to_string(),
                    )
                    .await;
                };
                if let Err(e) = importer(std::path::PathBuf::from(path)).await {
                    return send_error(
                        stream,
                        Some(id),
                        ErrorCode::EngineError,
                        format!("attachment import failed: {e}"),
                    )
                    .await;
                }
            }
            _ => {
                return send_error(
                    stream,
                    Some(id),
                    ErrorCode::ProtocolError,
                    "a chat Execute on the owner channel carries at most one \
                     parameter: the attachment source path as a text literal"
                        .to_string(),
                )
                .await;
            }
        }
        let outcome = {
            let peer = peer.to_string();
            let req = request.clone();
            let chain = self.chain.clone();
            self.db
                .call(move |conn| crate::chat::handle_chat(conn, &peer, &req, &chain, Lane::Owner))
                .await?
        };
        self.answer_chat_outcome(id, outcome, stream).await
    }

    /// Knock admission. An unadmitted key gets exactly one frame,
    /// `rsntr:Knock`; it routes through the authenticator chain as
    /// action `knock` with no SQL footprint. The token buckets run
    /// first: a knock that empties either bucket is dropped in silence,
    /// unaudited. Past the buckets:
    ///
    /// - a tier allows: a `_peers` row is written and the answer is
    ///   `rsntr:Decision "allow"`;
    /// - a tier denies: `rsntr:Decision "deny"`;
    /// - no automated tier decides (the chain falls to its tail
    ///   default): the knock parks in `_inbox` for the owner and the
    ///   answer is `rsntr:Decision "pending"`.
    async fn serve_knock<S: RequestStream>(
        &self,
        peer: &str,
        id: Option<String>,
        message: String,
        stream: &mut S,
    ) -> Result<(), NodeError> {
        let peer_owned = peer.to_string();
        let chain = self.chain.clone();
        let limits = self.config.knock_limits;
        let outcome = self
            .db
            .call(move |conn| {
                weigh_knock(conn, &peer_owned, id.as_deref(), &message, &chain, limits)
            })
            .await?;

        match outcome {
            // Rate limited: no response frame at all.
            KnockReply::Dropped => Ok(()),
            KnockReply::Decision {
                id,
                decision,
                decided_by,
                reason,
            } => {
                stream
                    .send(&EnvelopeObject::Decision(DecisionEnvelope {
                        id: Some(id),
                        decision,
                        decided_by,
                        reason,
                        at: Some(now_rfc3339()),
                    }))
                    .await?;
                Ok(())
            }
        }
    }

    /// Answers a `help`-modulation query with one `rsntr:Help` frame.
    /// Served to everyone including strangers; the text carries only
    /// owner-published guidance and policy-derived facts, never data.
    async fn serve_help<S: RequestStream>(
        &self,
        peer: &str,
        id: String,
        topic: String,
        stream: &mut S,
    ) -> Result<(), NodeError> {
        let peer_owned = peer.to_string();
        let req_id = id.clone();
        let (text, topics) = self
            .db
            .call(move |conn| build_help(conn, &peer_owned, &req_id, &topic))
            .await?;
        let id = if id.is_empty() { None } else { Some(id) };
        stream
            .send(&EnvelopeObject::Help(Help {
                id,
                signal: text,
                topics,
            }))
            .await?;
        Ok(())
    }

    /// Answers a `projection`-modulation query with one
    /// `rsntr:Projection` frame filtered by policy for this caller. A
    /// path that is not offered answers `point-unknown`. On the owner
    /// lane the projection is unfiltered: the owner sees every point.
    async fn serve_projection_lane<S: RequestStream>(
        &self,
        peer: &str,
        id: String,
        path: String,
        stream: &mut S,
        lane: Lane,
    ) -> Result<(), NodeError> {
        let peer_owned = peer.to_string();
        let req_id = id.clone();
        let path_owned = path.clone();
        let outcome = self
            .db
            .call(move |conn| {
                let outcome =
                    build_projection(conn, &peer_owned, &req_id, &path_owned, lane == Lane::Owner);
                let (decision, reason) = match &outcome {
                    Ok(ProjectionOutcome::Built(_)) => {
                        ("allow", format!("projection at path {path_owned:?}"))
                    }
                    Ok(ProjectionOutcome::UnknownPath) => {
                        ("deny", format!("unknown projection path {path_owned:?}"))
                    }
                    Err(e) => ("deny", format!("projection build failed: {e}")),
                };
                let (direction, decided_by) = match lane {
                    Lane::Owner => ("local", "owner"),
                    Lane::Remote => ("in", "node"),
                };
                audit_direct_dir(
                    conn,
                    direction,
                    &peer_owned,
                    &req_id,
                    "projection",
                    decision,
                    decided_by,
                    &reason,
                );
                outcome
            })
            .await?;
        match outcome {
            Ok(ProjectionOutcome::Built(p)) => {
                stream.send(&EnvelopeObject::Projection(p)).await?;
                Ok(())
            }
            Ok(ProjectionOutcome::UnknownPath) => {
                send_error(
                    stream,
                    Some(id),
                    ErrorCode::PointUnknown,
                    format!("no projection at path {path:?}"),
                )
                .await
            }
            Err(e) => send_error(stream, Some(id), ErrorCode::EngineError, e.to_string()).await,
        }
    }

    /// Entrainment (projection protocol sec 5). Gate, acknowledge with a
    /// bare Done, then forward signals as Vibrations until the client
    /// damps (in-band `rsntr:Damp` or stream close) or a stalled send
    /// forces a `limit-exceeded` damp. The registry entry lives exactly
    /// as long as this handler, which is what makes entrainment
    /// connection-scoped. On the owner lane there is no peer gate and no
    /// policy filter: any registered Sympathetic point may be entrained.
    async fn serve_entrain_lane<S: RequestStream>(
        &self,
        peer: &str,
        e: resonator_protocol::Entrain,
        stream: &mut S,
        lane: Lane,
    ) -> Result<(), NodeError> {
        let id = e.id.clone();
        let point = e.point.clone();
        let gate = {
            let peer = peer.to_string();
            let id = id.clone();
            let point = point.clone();
            self.db
                .call(move |conn| {
                    let gate = match lane {
                        Lane::Owner => gate_entrain_owner(conn, &point),
                        Lane::Remote => gate_entrain(conn, &peer, &point),
                    };
                    let (decision, reason): (&str, String) = match &gate {
                        EntrainGate::UnknownPeer => {
                            ("deny", "unknown peer: only rsntr:Knock is accepted".into())
                        }
                        EntrainGate::UnknownPoint { reason } => ("deny", reason.clone()),
                        EntrainGate::Denied { reason } => ("deny", reason.clone()),
                        EntrainGate::Allowed { .. } => ("allow", format!("entrained <{point}>")),
                    };
                    let (direction, decided_by) = match lane {
                        Lane::Owner => ("local", "owner"),
                        Lane::Remote => ("in", "node"),
                    };
                    audit_direct_dir(
                        conn, direction, &peer, &id, "entrain", decision, decided_by, &reason,
                    );
                    gate
                })
                .await?
        };

        let table = match gate {
            EntrainGate::UnknownPeer => {
                return send_denied(
                    stream,
                    Some(id),
                    "unknown peer: only rsntr:Knock is accepted".to_string(),
                )
                .await;
            }
            EntrainGate::UnknownPoint { reason } => {
                return send_error(stream, Some(id), ErrorCode::PointUnknown, reason).await;
            }
            EntrainGate::Denied { reason } => {
                return send_denied(stream, Some(id), reason).await;
            }
            EntrainGate::Allowed { table } => table,
        };

        // Subscribe before the ack so no signal can slip through a gap
        // between them.
        let (sub_id, mut signals) = self.entrainments.subscribe(point.clone(), table);

        // Acknowledge: entrained.
        if let Err(e) = stream.send(&bare_done(&id)).await {
            self.entrainments.unsubscribe(sub_id);
            return Err(e.into());
        }
        let send_timeout = Duration::from_millis(self.config.vibration_send_timeout_ms);
        let mut seq: i64 = 0;

        loop {
            tokio::select! {
                sig = signals.recv() => match sig {
                    None => break,
                    Some(()) => {
                        let vib = EnvelopeObject::Vibration(resonator_protocol::Vibration {
                            id: id.clone(),
                            point: point.clone(),
                            seq,
                            at: Some(now_rfc3339()),
                            payload: Vec::new(),
                        });
                        seq += 1;
                        match tokio::time::timeout(send_timeout, stream.send(&vib)).await {
                            Ok(Ok(())) => {}
                            // Client gone: the connection-scoped end.
                            Ok(Err(_)) => break,
                            // Slow consumer: damp with limit-exceeded
                            // (best effort; the stream may be wedged).
                            Err(_) => {
                                let _ = tokio::time::timeout(
                                    send_timeout,
                                    stream.send(&error_frame(
                                        &id,
                                        ErrorCode::LimitExceeded,
                                        "vibration consumer too slow; entrainment damped",
                                    )),
                                )
                                .await;
                                self.audit_entrain_end(peer, &id, "limit-exceeded damp").await;
                                break;
                            }
                        }
                    }
                },
                frame = stream.recv() => match frame {
                    Ok(Some(EnvelopeObject::Damp(d))) if d.point == point => {
                        // Confirm with a second Done, then end.
                        let _ = stream.send(&bare_done(&id)).await;
                        self.audit_entrain_end(peer, &id, "damped by peer").await;
                        break;
                    }
                    Ok(Some(other)) => {
                        let _ = send_error(
                            stream,
                            Some(id.clone()),
                            ErrorCode::ProtocolError,
                            format!(
                                "only rsntr:Damp for the entrained point may ride an \
                                 entrain stream, got {other:?}"
                            ),
                        )
                        .await;
                        break;
                    }
                    // Client closed, cleanly or not: entrainment over.
                    Ok(None) | Err(_) => break,
                },
            }
        }

        self.entrainments.unsubscribe(sub_id);
        Ok(())
    }

    /// Best-effort audit row when an entrainment ends abnormally.
    async fn audit_entrain_end(&self, peer: &str, id: &str, reason: &str) {
        let peer = peer.to_string();
        let id = id.to_string();
        let reason = reason.to_string();
        let _ = self
            .db
            .call(move |conn| {
                audit_direct(conn, &peer, &id, "entrain", "allow", "node", &reason);
            })
            .await;
    }

    /// The media modulation. `rsntr:signal` names a `_media` source;
    /// past the peer gate and the `_policy` media check, the source
    /// command is spawned and its stdout streams raw behind one
    /// `rsntr:Media` header frame. The feed ends when the source exits
    /// or the client hangs up, and the child dies either way.
    async fn serve_media<S: RequestStream>(
        &self,
        peer: &str,
        st: resonator_protocol::Statement,
        stream: &mut S,
    ) -> Result<(), NodeError> {
        let id = st.id;
        let name = st.signal.trim().to_string();

        let gate = {
            let peer = peer.to_string();
            let name = name.clone();
            let id = id.clone();
            self.db
                .call(move |conn| screen_source(conn, &peer, &id, &name, "media"))
                .await?
        };

        let (command, content_type) = match gate {
            MediaVerdict::UnknownPeer => {
                return send_denied(
                    stream,
                    Some(id),
                    "unknown peer: only rsntr:Knock is accepted".to_string(),
                )
                .await;
            }
            MediaVerdict::Denied { reason } => {
                return send_denied(stream, Some(id), reason).await;
            }
            MediaVerdict::UnknownSource => {
                return send_error(
                    stream,
                    Some(id),
                    ErrorCode::EngineError,
                    format!("unknown media source {name:?}"),
                )
                .await;
            }
            MediaVerdict::Allowed {
                command,
                content_type,
                ..
            } => (command, content_type),
        };

        let mut source = tokio::process::Command::new("sh");
        source
            .arg("-c")
            .arg(&command)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true);
        // Give the source its own process group, so ending the feed can
        // take down the whole `sh -c` pipeline, not just the shell.
        #[cfg(unix)]
        source.process_group(0);
        let mut child = match source.spawn() {
            Ok(c) => c,
            Err(e) => {
                warn!(source = %name, error = %e, "media source failed to spawn");
                return send_error(
                    stream,
                    Some(id),
                    ErrorCode::EngineError,
                    format!("media source failed to start: {e}"),
                )
                .await;
            }
        };
        let mut stdout = child.stdout.take().expect("stdout was requested as piped");

        stream
            .send(&EnvelopeObject::Media(resonator_protocol::Media {
                id: id.clone(),
                content_type,
            }))
            .await?;

        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 64 * 1024];
        let outcome = loop {
            // Select on the client's closure too: a silent source (a camera
            // refusing a session) must not keep a feed for a departed
            // client alive, holding the source hostage.
            let read = tokio::select! {
                r = stdout.read(&mut buf) => r,
                _ = stream.closed() => {
                    debug!(source = %name, "media client closed the stream");
                    break Ok(());
                }
            };
            let n = match read {
                Ok(0) => break Ok(()),
                Ok(n) => n,
                Err(e) => {
                    break Err(NodeError::from(
                        resonator_transport::TransportError::Stream(format!(
                            "media source read failed: {e}"
                        )),
                    ));
                }
            };
            // A failed send means the client hung up; stop feeding.
            if let Err(e) = stream.send_raw(&buf[..n]).await {
                debug!(source = %name, error = %e, "media client went away");
                break Ok(());
            }
        };
        // Graceful stop for the whole group, reap the shell, then a hard
        // sweep for stragglers.
        #[cfg(unix)]
        let pgid = child.id().map(|pid| pid as i32);
        #[cfg(unix)]
        if let Some(pgid) = pgid {
            unsafe { libc::kill(-pgid, libc::SIGTERM) };
        }
        let _ = child.start_kill();
        let _ = child.wait().await;
        #[cfg(unix)]
        if let Some(pgid) = pgid {
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
        }
        outcome
    }

    /// The builtin `audio-duplex` modulation (envelope doc sec 4.3): a
    /// `_media` source whose `accepts` column is set, run with stdin
    /// piped. After the `rsntr:AudioDuplex` header the stream is raw
    /// bytes both ways: the caller's bytes feed the source's stdin, the
    /// source's stdout feeds the caller.
    ///
    /// Two phases, because `closed()` and `recv_raw()` cannot share one
    /// select (both borrow the stream) and the transport's `closed()`
    /// does not fire on the caller's write-half Fin (that arrives as
    /// `recv_raw -> Ok(None)`): phase 1 pumps both directions until the
    /// caller finishes its write half; phase 2 is serve_media's exact
    /// downstream loop, watching `closed()` for full departure.
    async fn serve_audio_duplex<S: RequestStream>(
        &self,
        peer: &str,
        st: resonator_protocol::Statement,
        stream: &mut S,
    ) -> Result<(), NodeError> {
        let id = st.id;
        let name = st.signal.trim().to_string();

        let gate = {
            let peer = peer.to_string();
            let name = name.clone();
            let id = id.clone();
            self.db
                .call(move |conn| screen_source(conn, &peer, &id, &name, "audio-duplex"))
                .await?
        };

        let (command, content_type, accepts) = match gate {
            MediaVerdict::UnknownPeer => {
                return send_denied(
                    stream,
                    Some(id),
                    "unknown peer: only rsntr:Knock is accepted".to_string(),
                )
                .await;
            }
            MediaVerdict::Denied { reason } => {
                return send_denied(stream, Some(id), reason).await;
            }
            MediaVerdict::UnknownSource => {
                return send_error(
                    stream,
                    Some(id),
                    ErrorCode::EngineError,
                    format!("unknown media source {name:?}"),
                )
                .await;
            }
            MediaVerdict::Allowed {
                command,
                content_type,
                accepts: Some(accepts),
            } => (command, content_type, accepts),
            MediaVerdict::Allowed { accepts: None, .. } => {
                return send_error(
                    stream,
                    Some(id),
                    ErrorCode::EngineError,
                    format!("source {name:?} is not a duplex source (no accepts)"),
                )
                .await;
            }
        };

        let mut source = tokio::process::Command::new("sh");
        source
            .arg("-c")
            .arg(&command)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        source.process_group(0);
        let mut child = match source.spawn() {
            Ok(c) => c,
            Err(e) => {
                warn!(source = %name, error = %e, "audio-duplex source failed to spawn");
                return send_error(
                    stream,
                    Some(id),
                    ErrorCode::EngineError,
                    format!("audio-duplex source failed to start: {e}"),
                )
                .await;
            }
        };
        // The caller's audio goes to the source through a bounded
        // channel and a writer task, never straight from the select
        // loop: a source that stops draining its stdin (a talk sink
        // blocked on its own output) would otherwise wedge `write_all`
        // and with it the whole loop, so the call could not even be
        // hung up. Dropping the oldest audio is the right answer for a
        // live conversation; stalling is not.
        let mut stdin_tx = child.stdin.take().map(|mut sin| {
            let (tx, mut rx) = mpsc::channel::<Vec<u8>>(8);
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                while let Some(chunk) = rx.recv().await {
                    if sin.write_all(&chunk).await.is_err() {
                        break;
                    }
                }
                // Dropping stdin here is what EOFs the source when the
                // caller finishes or the session ends.
            });
            tx
        });
        let mut stdout = child.stdout.take().expect("stdout was requested as piped");

        stream
            .send(&EnvelopeObject::AudioDuplex(
                resonator_protocol::AudioDuplex {
                    id: id.clone(),
                    content_type: (!content_type.is_empty()).then_some(content_type),
                    accepts,
                },
            ))
            .await?;

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = vec![0u8; 64 * 1024];

        // Phase 1: full duplex while the caller's write half is open.
        // A false result means the session is over; true means the caller
        // sent Fin and the downstream may keep flowing.
        let mut outcome: Result<(), NodeError> = Ok(());
        let half_open = loop {
            tokio::select! {
                read = stdout.read(&mut buf) => match read {
                    Ok(0) => break false, // source ended
                    Ok(n) => {
                        if stream.send_raw(&buf[..n]).await.is_err() {
                            debug!(source = %name, "audio-duplex client went away");
                            break false;
                        }
                    }
                    Err(e) => {
                        outcome = Err(NodeError::from(
                            resonator_transport::TransportError::Stream(format!(
                                "audio-duplex source read failed: {e}"
                            )),
                        ));
                        break false;
                    }
                },
                chunk = stream.recv_raw() => match chunk {
                    Ok(Some(bytes)) => {
                        if let Some(tx) = stdin_tx.as_ref() {
                            match tx.try_send(bytes) {
                                Ok(()) => {}
                                Err(mpsc::error::TrySendError::Full(_)) => {
                                    // The source is not keeping up:
                                    // drop this span rather than stall
                                    // the session.
                                    debug!(source = %name, "audio-duplex source is not draining; dropping audio");
                                }
                                Err(mpsc::error::TrySendError::Closed(_)) => {
                                    stdin_tx = None;
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        // Caller Fin: EOF the source's stdin so it can
                        // wind down, keep serving its remaining output.
                        drop(stdin_tx.take());
                        break true;
                    }
                    Err(_) => {
                        debug!(source = %name, "audio-duplex client reset the stream");
                        break false;
                    }
                },
            }
        };

        // Phase 2: downstream only, serve_media's loop, but bounded. The
        // caller has said it will send no more; a source with nothing
        // left to say must not hold the session (and its process group)
        // open waiting for a transport signal that may never come. A
        // talk sink blocked on its own output is exactly that case, and
        // an intercom that keeps a dead call alive is worse than one
        // that hangs up a moment early.
        if half_open && outcome.is_ok() {
            loop {
                let read = tokio::select! {
                    r = stdout.read(&mut buf) => r,
                    _ = stream.closed() => {
                        debug!(source = %name, "audio-duplex client closed the stream");
                        break;
                    }
                    _ = tokio::time::sleep(HALF_OPEN_IDLE) => {
                        debug!(source = %name, "audio-duplex source idle after the client finished");
                        break;
                    }
                };
                match read {
                    Ok(0) => break,
                    Ok(n) => {
                        if stream.send_raw(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        outcome = Err(NodeError::from(
                            resonator_transport::TransportError::Stream(format!(
                                "audio-duplex source read failed: {e}"
                            )),
                        ));
                        break;
                    }
                }
            }
        }

        // Teardown: identical to serve_media.
        drop(stdin_tx);
        #[cfg(unix)]
        let pgid = child.id().map(|pid| pid as i32);
        #[cfg(unix)]
        if let Some(pgid) = pgid {
            unsafe { libc::kill(-pgid, libc::SIGTERM) };
        }
        let _ = child.start_kill();
        let _ = child.wait().await;
        #[cfg(unix)]
        if let Some(pgid) = pgid {
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
        }
        outcome
    }

    /// The builtin `chat` modulation (docs/chat-protocol.md): the signal
    /// carries one `rsntr:Message`; the handler gates on action `chat`,
    /// appends idempotently to `chat_messages`, fans out when hosting
    /// the room, and answers a Result/Done pair like any other write.
    async fn serve_chat<S: RequestStream>(
        &self,
        peer: &str,
        request: Request,
        stream: &mut S,
    ) -> Result<(), NodeError> {
        let id = request.id_string();
        // The attachment source path parameter is an owner-channel-only
        // affordance; on a gated surface it is a protocol error
        // (docs/owner-channel.md sec 5.3).
        if !request.params.is_empty() {
            return send_error(
                stream,
                Some(id),
                ErrorCode::ProtocolError,
                "a chat Execute carries no parameters on this surface".to_string(),
            )
            .await;
        }
        let outcome = {
            let peer = peer.to_string();
            let req = request.clone();
            let chain = self.chain.clone();
            self.db
                .call(move |conn| crate::chat::handle_chat(conn, &peer, &req, &chain, Lane::Remote))
                .await?
        };
        self.answer_chat_outcome(id, outcome, stream).await
    }

    /// Turns a chat handler outcome into its response frames (shared by
    /// the remote and owner lanes).
    async fn answer_chat_outcome<S: RequestStream>(
        &self,
        id: String,
        outcome: crate::chat::ChatOutcome,
        stream: &mut S,
    ) -> Result<(), NodeError> {
        use crate::chat::ChatOutcome;
        match outcome {
            ChatOutcome::UnknownPeer => {
                send_denied(
                    stream,
                    Some(id),
                    "unknown peer: only rsntr:Knock is accepted".to_string(),
                )
                .await
            }
            ChatOutcome::ModUnsupported => {
                send_error(
                    stream,
                    Some(id),
                    ErrorCode::ModUnsupported,
                    "modulation \"chat\" is not served by this node".to_string(),
                )
                .await
            }
            ChatOutcome::Denied { reason } => send_denied(stream, Some(id), reason).await,
            ChatOutcome::BadRequest { message } => {
                send_error(stream, Some(id), ErrorCode::ProtocolError, message).await
            }
            ChatOutcome::LimitExceeded { message } => {
                send_error(stream, Some(id), ErrorCode::LimitExceeded, message).await
            }
            ChatOutcome::EngineError { message } => {
                send_error(stream, Some(id), ErrorCode::EngineError, message).await
            }
            ChatOutcome::Applied { affected } => {
                stream
                    .send(&EnvelopeObject::Result(ResultHeader {
                        id: id.clone(),
                        columns: Vec::new(),
                        decl_types: Vec::new(),
                    }))
                    .await?;
                stream
                    .send(&EnvelopeObject::Done(Done {
                        id,
                        row_count: Some(0),
                        affected_rows: Some(affected),
                        last_insert_rowid: None,
                        truncated: false,
                    }))
                    .await?;
                Ok(())
            }
        }
    }

    /// The registered mod handler, iff one of its mods matches the
    /// requested modulation tag.
    fn matching_mod_handler(&self, requested: &str) -> Option<Arc<dyn ModHandler>> {
        let handler = self
            .mod_handler
            .lock()
            .expect("mod handler lock poisoned")
            .clone()?;
        handler
            .mods()
            .iter()
            .any(|m| mod_matches(requested, m))
            .then_some(handler)
    }

    /// Delegates one request to the mod handler: its frames arrive on an
    /// mpsc channel and are forwarded to the stream. A failed send drops
    /// the receiver, which fails the handler's next send and stops it
    /// (the same client-gone contract SQL execution has).
    async fn serve_mod<S: RequestStream>(
        &self,
        peer: &str,
        request: Request,
        handler: Arc<dyn ModHandler>,
        stream: &mut S,
    ) -> Result<(), NodeError> {
        let (ftx, mut frx) = mpsc::channel::<EnvelopeObject>(16);
        let run = handler.handle(peer.to_string(), request, ftx);
        let forward = async {
            let mut failed = None;
            while let Some(frame) = frx.recv().await {
                if let Err(e) = stream.send(&frame).await {
                    failed = Some(e);
                    break;
                }
            }
            drop(frx);
            failed
        };
        let ((), failed) = tokio::join!(run, forward);
        match failed {
            Some(e) => Err(NodeError::Transport(e)),
            None => Ok(()),
        }
    }

    /// The `sparql` modulation: parse, decide via the chain against the
    /// store's backing tables, then stream SELECT/ASK as Result/Row/Done
    /// and CONSTRUCT as `rsntr:Graph` frames closed by Done.
    async fn serve_sparql<S: RequestStream>(
        &self,
        peer: &str,
        request: Request,
        stream: &mut S,
    ) -> Result<(), NodeError> {
        let id = request.id_string();
        let known = {
            let peer = peer.to_string();
            let id = id.clone();
            let signal = request.signal.clone();
            self.db
                .call(move |conn| {
                    let known = peer_known(conn, &peer);
                    if !known {
                        audit_direct(
                            conn,
                            &peer,
                            &id,
                            &signal,
                            "deny",
                            "peer-gate",
                            "unknown peer",
                        );
                    }
                    known
                })
                .await?
        };
        if !known {
            return send_denied(
                stream,
                Some(id),
                "unknown peer: only rsntr:Knock is accepted".to_string(),
            )
            .await;
        }

        let limits = Limits::effective(&request.options, &self.config);
        let outcome = {
            let peer = peer.to_string();
            let req = request.clone();
            let chain = self.chain.clone();
            let row_cap = limits.row_cap;
            self.db
                .call(move |conn| gate_and_run_sparql(conn, &peer, &req, &chain, row_cap))
                .await?
        };
        self.stream_sparql_outcome(id, outcome, stream).await
    }

    /// Streams one resolved sparql outcome as frames (shared by the
    /// remote and owner lanes).
    async fn stream_sparql_outcome<S: RequestStream>(
        &self,
        id: String,
        outcome: SparqlOutcome,
        stream: &mut S,
    ) -> Result<(), NodeError> {
        match outcome {
            SparqlOutcome::Denied { reason } => send_denied(stream, Some(id), reason).await,
            SparqlOutcome::BadRequest { message } => {
                send_error(stream, Some(id), ErrorCode::ProtocolError, message).await
            }
            SparqlOutcome::EngineError { message } => {
                send_error(stream, Some(id), ErrorCode::EngineError, message).await
            }
            SparqlOutcome::Select {
                vars,
                rows,
                truncated,
            } => {
                stream
                    .send(&EnvelopeObject::Result(ResultHeader {
                        id: id.clone(),
                        columns: vars.clone(),
                        decl_types: Vec::new(),
                    }))
                    .await?;
                let mut seq: i64 = 0;
                let mut batch: Vec<Row> = Vec::new();
                let mut batch_bytes = 0usize;
                for row in &rows {
                    let mut cells = Vec::new();
                    let mut est = 32usize;
                    for (var, term) in vars.iter().zip(row.iter()) {
                        if let Some(t) = term {
                            let v = term_value(t);
                            if let Value::Text(s) = &v {
                                est += s.len() + var.len() + 16;
                            }
                            cells.push((var.clone(), v));
                        }
                    }
                    batch.push(Row { seq, cells });
                    batch_bytes += est;
                    seq += 1;
                    if batch.len() >= self.config.rows_per_frame
                        || batch_bytes >= self.config.frame_byte_budget
                    {
                        stream
                            .send(&EnvelopeObject::Row(std::mem::take(&mut batch)))
                            .await?;
                        batch_bytes = 0;
                    }
                }
                if !batch.is_empty() {
                    stream.send(&EnvelopeObject::Row(batch)).await?;
                }
                stream
                    .send(&EnvelopeObject::Done(Done {
                        id,
                        row_count: Some(seq),
                        affected_rows: None,
                        last_insert_rowid: None,
                        truncated,
                    }))
                    .await?;
                Ok(())
            }
            SparqlOutcome::Ask(b) => {
                stream
                    .send(&EnvelopeObject::Result(ResultHeader {
                        id: id.clone(),
                        columns: vec!["ask".to_string()],
                        decl_types: Vec::new(),
                    }))
                    .await?;
                stream
                    .send(&EnvelopeObject::Row(vec![Row {
                        seq: 0,
                        cells: vec![("ask".to_string(), Value::Integer(i64::from(b)))],
                    }]))
                    .await?;
                stream
                    .send(&EnvelopeObject::Done(Done {
                        id,
                        row_count: Some(1),
                        affected_rows: None,
                        last_insert_rowid: None,
                        truncated: false,
                    }))
                    .await?;
                Ok(())
            }
            SparqlOutcome::Construct { triples } => {
                let total = triples.len() as i64;
                let mut seq: i64 = 0;
                let mut chunk: Vec<oxrdf::Triple> = Vec::new();
                let mut chunk_bytes = 0usize;
                for t in triples {
                    chunk_bytes += triple_estimate(&t);
                    chunk.push(t);
                    if chunk.len() >= 512 || chunk_bytes >= self.config.frame_byte_budget {
                        stream
                            .send(&EnvelopeObject::Graph(Graph {
                                id: id.clone(),
                                seq,
                                payload: std::mem::take(&mut chunk),
                            }))
                            .await?;
                        seq += 1;
                        chunk_bytes = 0;
                    }
                }
                // At least one Graph frame goes out even when empty: the
                // frame is the typed answer, Done merely closes it.
                if !chunk.is_empty() || seq == 0 {
                    stream
                        .send(&EnvelopeObject::Graph(Graph {
                            id: id.clone(),
                            seq,
                            payload: chunk,
                        }))
                        .await?;
                }
                stream
                    .send(&EnvelopeObject::Done(Done {
                        id,
                        row_count: Some(total),
                        affected_rows: None,
                        last_insert_rowid: None,
                        truncated: false,
                    }))
                    .await?;
                Ok(())
            }
            SparqlOutcome::Updated { affected } => {
                stream
                    .send(&EnvelopeObject::Result(ResultHeader {
                        id: id.clone(),
                        columns: Vec::new(),
                        decl_types: Vec::new(),
                    }))
                    .await?;
                stream
                    .send(&EnvelopeObject::Done(Done {
                        id,
                        row_count: Some(0),
                        affected_rows: Some(affected),
                        last_insert_rowid: None,
                        truncated: false,
                    }))
                    .await?;
                Ok(())
            }
        }
    }

    async fn serve_sql<S: RequestStream>(
        &self,
        peer: &str,
        request: Request,
        stream: &mut S,
    ) -> Result<(), NodeError> {
        let id = request.id_string();

        // Job A: peer gate, collect-mode prepare, chain decide.
        let verdict = {
            let peer = peer.to_string();
            let req = request.clone();
            let chain = self.chain.clone();
            let allowlist = self.config.pragma_allowlist.clone();
            self.db
                .call(move |conn| screen_request(conn, &peer, &req, &chain, &allowlist))
                .await?
        };

        match verdict {
            Verdict::UnknownPeer => {
                send_denied(
                    stream,
                    Some(id),
                    "unknown peer: only rsntr:Knock is accepted".to_string(),
                )
                .await
            }
            Verdict::Banned { reason } | Verdict::Denied { reason } => {
                send_denied(stream, Some(id), reason).await
            }
            Verdict::PrepareFailed { message } => {
                send_error(stream, Some(id), ErrorCode::EngineError, message).await
            }
            Verdict::Cleared {
                approved,
                exec_sql,
                audit_id,
            } => {
                self.execute_and_stream(request, approved, exec_sql, audit_id, stream)
                    .await
            }
        }
    }

    async fn execute_and_stream<S: RequestStream>(
        &self,
        request: Request,
        approved: Approved,
        exec_sql: String,
        audit_id: i64,
        stream: &mut S,
    ) -> Result<(), NodeError> {
        // Test seam: the window between decide and execute.
        if let Some(hook) = &self.config.post_decide_hook {
            let hook = hook.clone();
            self.db.call(move |conn| hook(conn)).await?;
        }

        let limits = Limits::effective(&request.options, &self.config);
        let generation = self.db.new_generation();

        // Wall-clock deadline: a tokio timer interrupts the statement
        // through the InterruptHandle if the guarded job still runs.
        let timer = {
            let db = self.db.clone();
            let timeout = Duration::from_millis(limits.timeout_ms);
            tokio::spawn(async move {
                tokio::time::sleep(timeout).await;
                db.interrupt_if_running(generation);
            })
        };

        let (ftx, mut frx) = mpsc::channel::<EnvelopeObject>(16);
        let job = ExecJob {
            id: request.id_string(),
            sql: exec_sql,
            params: request.params.clone(),
            approved,
            limits,
            rows_per_frame: self.config.rows_per_frame,
            frame_byte_budget: self.config.frame_byte_budget,
            pragma_allowlist: self.config.pragma_allowlist.clone(),
            audit_id,
        };
        let exec = {
            let db = self.db.clone();
            tokio::spawn(async move {
                db.call_guarded(generation, move |conn| run_execute(conn, job, ftx))
                    .await
            })
        };

        // Forward frames as they materialize. If the client is gone the
        // send fails; dropping the receiver then fails the job's next
        // send, which stops execution and rolls back.
        let mut send_failed = None;
        while let Some(frame) = frx.recv().await {
            if send_failed.is_none()
                && let Err(e) = stream.send(&frame).await
            {
                send_failed = Some(e);
                break;
            }
        }
        drop(frx);
        let exec_result = exec.await;
        timer.abort();
        match exec_result {
            Ok(job_result) => job_result?,
            Err(join_err) => {
                warn!(error = %join_err, "execute task panicked or was cancelled");
            }
        }
        match send_failed {
            Some(e) => Err(NodeError::Transport(e)),
            None => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------
// Job A: gate, collect, decide
// ---------------------------------------------------------------------------

enum Verdict {
    UnknownPeer,
    /// Prepare touched a categorically banned action (ATTACH, DDL, ...).
    Banned {
        reason: String,
    },
    /// Prepare failed for engine reasons (syntax, missing table).
    PrepareFailed {
        message: String,
    },
    /// The chain refused.
    Denied {
        reason: String,
    },
    Cleared {
        approved: Approved,
        exec_sql: String,
        /// `_audit` row id of the decide record; execution fills in its
        /// rows_out/bytes_out.
        audit_id: i64,
    },
}

/// True when `peer` has a `_peers` row (the peer gate's question). A
/// node's own proven endpoint id is always admitted (chat protocol sec
/// 4.3: self-dial is acceptance path A), so a node's own surfaces can
/// watch it without a self `_peers` row.
pub fn peer_known(conn: &Connection, peer: &str) -> bool {
    if crate::ddl::get_rsntr(conn, "endpoint_id").is_some_and(|own| own == peer) {
        return true;
    }
    conn.query_row(
        "SELECT 1 FROM _peers WHERE endpoint_id = ?1",
        [peer],
        |_| Ok(()),
    )
    .optional()
    .map(|o| o.is_some())
    .unwrap_or(false)
}

/// Collect-mode prepare: yields the finalized footprint and drops the
/// statement (execution re-prepares under enforce mode). The error side
/// carries `(banned_reason, footprint_json, message)`.
type ScreenErr = (Option<String>, String, String);

fn collect_footprint(
    conn: &Connection,
    sql: &str,
    allowlist: &Arc<Vec<String>>,
    lane: Lane,
) -> Result<Footprint, ScreenErr> {
    let state = Arc::new(Mutex::new(CollectState::default()));
    if let Err(e) = conn.authorizer(Some(collect_authorizer(
        state.clone(),
        allowlist.clone(),
        lane,
    ))) {
        return Err((
            None,
            "{}".into(),
            format!("installing authorizer failed: {e}"),
        ));
    }
    let prep = conn.prepare(sql).map(drop);
    let _ = conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    let st = match Arc::try_unwrap(state) {
        Ok(m) => m.into_inner().unwrap_or_default(),
        Err(_) => CollectState::default(),
    };
    let denied = st.denied.clone();
    let footprint = st.finish();
    match (prep, denied) {
        (Ok(()), None) => Ok(footprint),
        (_, Some(reason)) => Err((Some(reason.clone()), footprint.to_json(), reason)),
        (Err(e), None) => Err((None, footprint.to_json(), format!("prepare failed: {e}"))),
    }
}

fn screen_request(
    conn: &mut Connection,
    peer: &str,
    req: &Request,
    chain: &Chain,
    allowlist: &Arc<Vec<String>>,
) -> Verdict {
    let id = req.id_string();

    // 1. Peer gate: an unknown key is turned away before any SQL runs.
    if !peer_known(conn, peer) {
        audit_direct(
            conn,
            peer,
            &id,
            &req.signal,
            "deny",
            "peer-gate",
            "unknown peer",
        );
        return Verdict::UnknownPeer;
    }

    // 2. Collect-mode prepare: the ground-truth footprint.
    let footprint = match collect_footprint(conn, &req.signal, allowlist, Lane::Remote) {
        Ok(fp) => fp,
        Err((Some(reason), fp_json, _)) => {
            audit_full(
                conn,
                peer,
                &id,
                &req.signal,
                &fp_json,
                "deny",
                "node",
                Some(&reason),
                0,
            );
            return Verdict::Banned { reason };
        }
        Err((None, fp_json, message)) => {
            audit_full(
                conn,
                peer,
                &id,
                &req.signal,
                &fp_json,
                "deny",
                "node",
                Some(&message),
                0,
            );
            return Verdict::PrepareFailed { message };
        }
    };

    // 3. Chain decide; the node itself writes the audit row.
    let action = footprint.kind.default_action();
    let start = Instant::now();
    let decided = chain.decide(conn, peer, action, &footprint, &req.signal);
    let duration = start.elapsed().as_millis() as u64;
    let (decision_str, reason) = match &decided.decision {
        Decision::Allow => ("allow", None),
        Decision::AllowNarrowed { .. } => ("allow_narrowed", None),
        Decision::Deny { reason } => ("deny", Some(reason.clone())),
        Decision::Escalate => (
            "deny",
            Some("authenticator chain did not decide".to_string()),
        ),
    };
    let audit_id = audit_full(
        conn,
        peer,
        &id,
        &req.signal,
        &footprint.to_json(),
        decision_str,
        &decided.decided_by,
        reason.as_deref(),
        duration,
    );

    match decided.decision {
        Decision::Allow => Verdict::Cleared {
            approved: Approved {
                footprint,
                lane: Lane::Remote,
            },
            exec_sql: req.signal.clone(),
            audit_id,
        },
        Decision::AllowNarrowed { rewrite } => {
            // The rewrite came from a trusted decider; its own footprint
            // is the envelope execution must stay inside.
            match collect_footprint(conn, &rewrite, allowlist, Lane::Remote) {
                Ok(new_fp) => Verdict::Cleared {
                    approved: Approved {
                        footprint: new_fp,
                        lane: Lane::Remote,
                    },
                    exec_sql: rewrite,
                    audit_id,
                },
                Err((_, _, message)) => Verdict::PrepareFailed {
                    message: format!("narrowed rewrite failed to prepare: {message}"),
                },
            }
        }
        Decision::Deny { reason } => Verdict::Denied { reason },
        // The chain's tail default is Deny; Escalate cannot get past it.
        Decision::Escalate => Verdict::Denied {
            reason: "authenticator chain did not decide".into(),
        },
    }
}

/// The owner channel's screening (docs/owner-channel.md sec 4): no peer
/// gate and no chain. The footprint is collected under the owner ban set
/// (DDL/PRAGMA/transaction control pass; ATTACH/DETACH/load_extension are
/// the only Denied source) and kept for the ledger, not a decision; every
/// outcome audits with `direction = 'local'`, `decided_by = 'owner'`.
fn screen_owner_request(
    conn: &mut Connection,
    peer: &str,
    req: &Request,
    allowlist: &Arc<Vec<String>>,
) -> Verdict {
    let id = req.id_string();
    let start = Instant::now();
    let footprint = match collect_footprint(conn, &req.signal, allowlist, Lane::Owner) {
        Ok(fp) => fp,
        Err((banned, fp_json, message)) => {
            let duration = start.elapsed().as_millis() as u64;
            audit_full_dir(
                conn,
                "local",
                peer,
                &id,
                &req.signal,
                &fp_json,
                "deny",
                "owner",
                Some(&message),
                duration,
            );
            return match banned {
                Some(reason) => Verdict::Banned { reason },
                None => Verdict::PrepareFailed { message },
            };
        }
    };
    let duration = start.elapsed().as_millis() as u64;
    let audit_id = audit_full_dir(
        conn,
        "local",
        peer,
        &id,
        &req.signal,
        &footprint.to_json(),
        "allow",
        "owner",
        None,
        duration,
    );
    Verdict::Cleared {
        approved: Approved {
            footprint,
            lane: Lane::Owner,
        },
        exec_sql: req.signal.clone(),
        audit_id,
    }
}

// ---------------------------------------------------------------------------
// Plugin-issued statements (the mods host's db_query/db_execute bridge)
// ---------------------------------------------------------------------------

/// Outcome of one statement a mod plugin runs on behalf of a request.
#[derive(Debug)]
pub enum ModDbOutcome {
    /// The chain (or a pipeline ban) refused.
    Denied { reason: String },
    /// Engine failure (syntax, missing table, bad parameter).
    Error { message: String },
    /// A read's collected result set.
    Rows {
        columns: Vec<String>,
        rows: Vec<Vec<rusqlite::types::Value>>,
    },
    /// A write's effect.
    Executed { rows_affected: i64 },
}

/// Runs one plugin-issued statement through the exact sql-sqlite serving
/// path: collect-mode footprint, `Chain::decide` on the footprint's
/// action, an `_audit` row, then enforce-mode execution inside its own
/// transaction with the node's row cap. The unit of authorization is
/// this statement, decided as if `peer` had sent it directly.
///
/// `write_allowed` is false for `db_query` (any non-read footprint is
/// refused before the chain runs). Rows are collected in memory (capped
/// at `config.max_rows`), not streamed. No `_applied` row is written:
/// idempotency of plugin-internal writes is the plugin's concern.
#[allow(clippy::too_many_arguments)]
pub fn run_mod_statement(
    conn: &mut Connection,
    peer: &str,
    request_id: &str,
    chain: &Chain,
    config: &NodeConfig,
    sql: &str,
    params: &[Value],
    write_allowed: bool,
) -> ModDbOutcome {
    let allowlist = config.pragma_allowlist.clone();
    let footprint = match collect_footprint(conn, sql, &allowlist, Lane::Remote) {
        Ok(fp) => fp,
        Err((banned, fp_json, message)) => {
            audit_full(
                conn,
                peer,
                request_id,
                sql,
                &fp_json,
                "deny",
                "node",
                Some(&message),
                0,
            );
            return match banned {
                Some(reason) => ModDbOutcome::Denied { reason },
                None => ModDbOutcome::Error { message },
            };
        }
    };
    if footprint.kind != ActionKind::Read && !write_allowed {
        let reason = "db_query is read-only; use db_execute (cap db_write) for writes".to_string();
        audit_full(
            conn,
            peer,
            request_id,
            sql,
            &footprint.to_json(),
            "deny",
            "node",
            Some(&reason),
            0,
        );
        return ModDbOutcome::Denied { reason };
    }

    let action = footprint.kind.default_action();
    let start = Instant::now();
    let decided = chain.decide(conn, peer, action, &footprint, sql);
    let duration = start.elapsed().as_millis() as u64;
    let (decision_str, reason) = match &decided.decision {
        Decision::Allow => ("allow", None),
        Decision::AllowNarrowed { .. } => ("allow_narrowed", None),
        Decision::Deny { reason } => ("deny", Some(reason.clone())),
        Decision::Escalate => (
            "deny",
            Some("authenticator chain did not decide".to_string()),
        ),
    };
    let audit_id = audit_full(
        conn,
        peer,
        request_id,
        sql,
        &footprint.to_json(),
        decision_str,
        &decided.decided_by,
        reason.as_deref(),
        duration,
    );

    let (approved, exec_sql) = match decided.decision {
        Decision::Allow => (
            Approved {
                footprint,
                lane: Lane::Remote,
            },
            sql.to_string(),
        ),
        Decision::AllowNarrowed { rewrite } => {
            match collect_footprint(conn, &rewrite, &allowlist, Lane::Remote) {
                Ok(fp) => (
                    Approved {
                        footprint: fp,
                        lane: Lane::Remote,
                    },
                    rewrite,
                ),
                Err((_, _, message)) => {
                    return ModDbOutcome::Error {
                        message: format!("narrowed rewrite failed to prepare: {message}"),
                    };
                }
            }
        }
        Decision::Deny { reason } => return ModDbOutcome::Denied { reason },
        Decision::Escalate => {
            return ModDbOutcome::Denied {
                reason: "authenticator chain did not decide".into(),
            };
        }
    };

    let outcome = execute_mod_statement(conn, &approved, &exec_sql, params, config);
    if let ModDbOutcome::Rows { rows, .. } = &outcome {
        audit_outcome(conn, audit_id, rows.len() as i64, 0);
    }
    outcome
}

/// Enforce-mode execution for [`run_mod_statement`], inside its own
/// transaction.
fn execute_mod_statement(
    conn: &mut Connection,
    approved: &Approved,
    sql: &str,
    params: &[Value],
    config: &NodeConfig,
) -> ModDbOutcome {
    match execute_mod_inner(conn, approved, sql, params, config) {
        Ok(outcome) | Err(outcome) => outcome,
    }
}

/// [`execute_mod_statement`] body; the Err side is just the early-exit
/// outcome (`?`-friendly around the statement borrow of the transaction).
fn execute_mod_inner(
    conn: &mut Connection,
    approved: &Approved,
    sql: &str,
    params: &[Value],
    config: &NodeConfig,
) -> Result<ModDbOutcome, ModDbOutcome> {
    let engine_err = |e: rusqlite::Error| ModDbOutcome::Error {
        message: e.to_string(),
    };
    for p in params {
        if let Value::BlobRef { .. } = p {
            return Err(ModDbOutcome::Error {
                message: "rsntr:BlobRef parameters are not supported yet".into(),
            });
        }
    }
    let is_write = approved.footprint.kind == ActionKind::Write;
    let tx = conn.transaction().map_err(engine_err)?;
    let deny_reason: Arc<Mutex<Option<String>>> = Arc::default();
    tx.authorizer(Some(enforce_authorizer(
        approved.clone(),
        config.pragma_allowlist.clone(),
        deny_reason.clone(),
    )))
    .map_err(engine_err)?;
    let prep = tx.prepare(sql);
    let _ = tx.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    let mut stmt = prep.map_err(
        |e| match deny_reason.lock().ok().and_then(|mut g| g.take()) {
            Some(reason) => ModDbOutcome::Denied { reason },
            None => engine_err(e),
        },
    )?;

    let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows_out: Vec<Vec<rusqlite::types::Value>> = Vec::new();
    let mut rows = stmt
        .query(rusqlite::params_from_iter(params.iter().map(BoundParam)))
        .map_err(engine_err)?;
    while let Some(row) = rows.next().map_err(engine_err)? {
        if rows_out.len() as i64 >= config.max_rows {
            break;
        }
        let mut cells = Vec::with_capacity(columns.len());
        for i in 0..columns.len() {
            cells.push(
                row.get::<_, rusqlite::types::Value>(i)
                    .map_err(engine_err)?,
            );
        }
        rows_out.push(cells);
    }
    drop(rows);
    drop(stmt);
    let affected = tx.changes() as i64;
    tx.commit().map_err(engine_err)?;
    Ok(if is_write {
        ModDbOutcome::Executed {
            rows_affected: affected,
        }
    } else {
        ModDbOutcome::Rows {
            columns,
            rows: rows_out,
        }
    })
}

/// The media/audio-duplex gate's outcome, decided on the db thread.
enum MediaVerdict {
    UnknownPeer,
    Denied {
        reason: String,
    },
    UnknownSource,
    Allowed {
        command: String,
        content_type: String,
        /// Upstream media type the source's stdin accepts; NULL = a
        /// one-way media source.
        accepts: Option<String>,
    },
}

/// True when some `_policy` row with this action and effect matches the
/// peer (or `'*'`) and the source name (or `'*'`). The source name
/// travels in `table_name`; `action` is `media` or `audio-duplex`.
fn source_policy_match(
    conn: &Connection,
    peer: &str,
    name: &str,
    action: &str,
    effect: &str,
) -> bool {
    conn.query_row(
        "SELECT 1 FROM _policy \
         WHERE action = ?4 AND effect = ?1 \
           AND (peer_or_group = ?2 OR peer_or_group = '*') \
           AND (table_name = ?3 OR table_name = '*') \
         LIMIT 1",
        (effect, peer, name, action),
        |_| Ok(()),
    )
    .optional()
    .map(|o| o.is_some())
    .unwrap_or(false)
}

/// Peer gate -> `_policy` check on `action` (deny wins) -> `_media`
/// lookup; every outcome writes a direct `_audit` row. Serves both the
/// media and audio-duplex gates; the actions are deliberately separate
/// (talking into a place is more privileged than watching it).
fn screen_source(
    conn: &mut Connection,
    peer: &str,
    id: &str,
    name: &str,
    action: &str,
) -> MediaVerdict {
    let signal = format!("{action} {name}");
    if !peer_known(conn, peer) {
        audit_direct(conn, peer, id, &signal, "deny", "peer-gate", "unknown peer");
        return MediaVerdict::UnknownPeer;
    }
    if source_policy_match(conn, peer, name, action, "deny") {
        let reason = format!("{action} source {name:?} is denied by policy");
        audit_direct(conn, peer, id, &signal, "deny", "policy", &reason);
        return MediaVerdict::Denied { reason };
    }
    if !source_policy_match(conn, peer, name, action, "allow") {
        let reason = format!("no policy allows you the {action} source {name:?}");
        audit_direct(conn, peer, id, &signal, "deny", "policy", &reason);
        return MediaVerdict::Denied { reason };
    }
    let row: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT command, content_type, accepts FROM _media WHERE name = ?1",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .unwrap_or(None);
    match row {
        None => {
            audit_direct(
                conn,
                peer,
                id,
                &signal,
                "deny",
                "node",
                "unknown media source",
            );
            MediaVerdict::UnknownSource
        }
        Some((command, content_type, accepts)) => {
            audit_direct(conn, peer, id, &signal, "allow", "policy", "feed opened");
            MediaVerdict::Allowed {
                command,
                content_type,
                accepts,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Knock admission
// ---------------------------------------------------------------------------

/// What the knock handler answers on the stream.
enum KnockReply {
    /// Rate limited: send nothing, audit nothing.
    Dropped,
    /// Answer an `rsntr:Decision`; `decision` is `allow`, `deny`, or
    /// `pending` (parked for the owner).
    Decision {
        id: String,
        decision: String,
        decided_by: String,
        reason: Option<String>,
    },
}

/// Runs on the db thread: rate-limit, route the knock through the chain,
/// act on the result (admit / deny / park), and say what to send.
fn weigh_knock(
    conn: &mut Connection,
    peer: &str,
    client_request_id: Option<&str>,
    message: &str,
    chain: &Chain,
    limits: KnockLimits,
) -> KnockReply {
    // One id ties together the knock, its `_inbox` row, and the answer.
    // A client-supplied `rsntr:id` is honored only as a well-formed ULID
    // (re-serialized to canonical form); anything else is replaced, so a
    // stranger cannot push garbage or oversized keys into `_inbox`.
    let id = client_request_id
        .and_then(|s| ulid::Ulid::from_string(s).ok())
        .map(|u| u.to_string())
        .unwrap_or_else(|| ulid::Ulid::new().to_string());

    // An admitted key has nothing to knock for: answer allow without
    // spending rate budget or re-inserting the peer. Audited directly
    // because the chain is never consulted.
    if peer_known(conn, peer) {
        audit_direct(
            conn,
            peer,
            &id,
            "",
            "allow",
            "node",
            "knock from an already admitted peer",
        );
        return KnockReply::Decision {
            id,
            decision: "allow".to_string(),
            decided_by: "node".to_string(),
            reason: Some("you are already admitted".to_string()),
        };
    }

    // Rate limit before anything else, so a dropped knock is never
    // audited and never reaches the chain.
    if !spend_knock_token(conn, peer, limits) {
        debug!(peer, "knock dropped by rate limit");
        return KnockReply::Dropped;
    }

    // Through the chain as a knock decision: no SQL, no footprint.
    let footprint = Footprint::default();
    let start = Instant::now();
    let decided = chain.decide(conn, peer, "knock", &footprint, "");
    let duration = start.elapsed().as_millis() as u64;

    match decided.decision {
        Decision::Allow | Decision::AllowNarrowed { .. } => {
            audit_full(
                conn,
                peer,
                &id,
                "",
                "{}",
                "allow",
                &decided.decided_by,
                Some("knock admitted"),
                duration,
            );
            admit_peer(conn, peer, &id);
            KnockReply::Decision {
                id,
                decision: "allow".to_string(),
                decided_by: decided.decided_by,
                reason: Some("admitted".to_string()),
            }
        }
        // Tail default deny (decided_by "default") means no automated
        // tier took a position: park for the owner instead of denying.
        Decision::Deny { .. } if decided.decided_by == "default" => {
            audit_full(
                conn,
                peer,
                &id,
                "",
                "{}",
                "deny",
                "default",
                Some("knock parked for the owner"),
                duration,
            );
            park_knock(conn, peer, &id, message);
            KnockReply::Decision {
                id,
                decision: "pending".to_string(),
                decided_by: "node".to_string(),
                reason: Some("your knock is parked for the owner's decision".to_string()),
            }
        }
        Decision::Deny { reason } => {
            audit_full(
                conn,
                peer,
                &id,
                "",
                "{}",
                "deny",
                &decided.decided_by,
                Some(&reason),
                duration,
            );
            KnockReply::Decision {
                id,
                decision: "deny".to_string(),
                decided_by: decided.decided_by,
                reason: Some(reason),
            }
        }
        // The chain never emits Escalate; park defensively if it does.
        Decision::Escalate => {
            audit_full(
                conn,
                peer,
                &id,
                "",
                "{}",
                "deny",
                "node",
                Some("knock parked (chain escalated)"),
                duration,
            );
            park_knock(conn, peer, &id, message);
            KnockReply::Decision {
                id,
                decision: "pending".to_string(),
                decided_by: "node".to_string(),
                reason: Some("your knock is parked for the owner's decision".to_string()),
            }
        }
    }
}

/// Admission itself: the `_peers` row is what "letting a stranger in"
/// means. Idempotent.
fn admit_peer(conn: &Connection, peer: &str, id: &str) {
    let res = conn.execute(
        "INSERT OR IGNORE INTO _peers (endpoint_id, added_at, notes) VALUES (?1, ?2, ?3)",
        (peer, now_rfc3339(), format!("admitted via knock {id}")),
    );
    if let Err(e) = res {
        warn!(error = %e, "failed to insert admitted peer");
    }
}

/// Parks a knock in `_inbox` for the owner. Knocks carry no SQL, so the
/// message rides in `params`. At most one pending row per peer: knocking
/// again across windows does not stack up.
fn park_knock(conn: &Connection, peer: &str, id: &str, message: &str) {
    let already: bool = conn
        .query_row(
            "SELECT 1 FROM _inbox WHERE peer = ?1 AND decision IS NULL AND sql = ''",
            [peer],
            |_| Ok(()),
        )
        .optional()
        .map(|o| o.is_some())
        .unwrap_or(false);
    if already {
        return;
    }
    let res = conn.execute(
        "INSERT INTO _inbox (request_id, peer, sql, params, received_at) \
         VALUES (?1, ?2, '', ?3, ?4)",
        (id, peer, format!("knock: {message}"), now_rfc3339()),
    );
    if let Err(e) = res {
        warn!(error = %e, "failed to park knock in _inbox");
    }
}

/// The persisted per-key + global token bucket. Grants (and spends one
/// token from each bucket) only when both hold a whole token; otherwise
/// nothing is spent, so draining one bucket cannot drain the other.
/// Refill is by elapsed wall-clock time.
fn spend_knock_token(conn: &Connection, peer: &str, limits: KnockLimits) -> bool {
    let now = unix_now_f64();

    let per_key = bucket_level(
        conn,
        peer,
        limits.per_key_burst,
        limits.per_key_refill_per_sec,
        now,
    );
    let global = bucket_level(
        conn,
        "*",
        limits.global_burst,
        limits.global_refill_per_sec,
        now,
    );

    let grant = per_key >= 1.0 && global >= 1.0;
    // The refilled levels persist either way; a token leaves each bucket
    // only on a grant. Recording the refilled level on a drop is honest
    // token accounting and never extends the budget.
    store_bucket(conn, peer, if grant { per_key - 1.0 } else { per_key }, now);
    store_bucket(conn, "*", if grant { global - 1.0 } else { global }, now);
    grant
}

/// Reads one bucket and returns its level refilled to `now`, capped at
/// `burst`. A bucket without a row starts full.
fn bucket_level(conn: &Connection, bucket: &str, burst: f64, refill_per_sec: f64, now: f64) -> f64 {
    let existing: Option<(f64, f64)> = conn
        .query_row(
            "SELECT tokens, updated_at FROM _knock_budget WHERE bucket = ?1",
            [bucket],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .unwrap_or(None);
    match existing {
        Some((tokens, last)) => {
            let elapsed = (now - last).max(0.0);
            (tokens + elapsed * refill_per_sec).min(burst)
        }
        None => burst,
    }
}

/// Upserts one bucket's level and refill timestamp.
fn store_bucket(conn: &Connection, bucket: &str, tokens: f64, now: f64) {
    let res = conn.execute(
        "INSERT INTO _knock_budget (bucket, tokens, updated_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT(bucket) DO UPDATE SET tokens = ?2, updated_at = ?3",
        (bucket, tokens, now),
    );
    if let Err(e) = res {
        warn!(error = %e, "failed to persist knock budget");
    }
}

// ---------------------------------------------------------------------------
// Job B: execute under enforcement, stream frames
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Limits {
    row_cap: i64,
    byte_cap: i64,
    timeout_ms: u64,
    step_budget: u64,
}

impl Limits {
    /// The request's options clamped by the node's ceilings.
    fn effective(opts: &resonator_protocol::RequestOptions, cfg: &NodeConfig) -> Self {
        let row_cap = opts
            .row_limit
            .filter(|v| *v >= 0)
            .map_or(cfg.max_rows, |v| v.min(cfg.max_rows));
        let byte_cap = opts
            .byte_limit
            .filter(|v| *v > 0)
            .map_or(cfg.max_response_bytes, |v| v.min(cfg.max_response_bytes));
        let timeout_ms = opts
            .timeout_ms
            .filter(|v| *v > 0)
            .map_or(cfg.max_duration_ms, |v| (v as u64).min(cfg.max_duration_ms));
        Self {
            row_cap,
            byte_cap,
            timeout_ms,
            step_budget: cfg.vdbe_step_budget,
        }
    }
}

struct ExecJob {
    id: String,
    sql: String,
    params: Vec<Value>,
    approved: Approved,
    limits: Limits,
    rows_per_frame: usize,
    frame_byte_budget: usize,
    pragma_allowlist: Arc<Vec<String>>,
    audit_id: i64,
}

/// Why execution ended without a normal Done.
enum Halt {
    /// The response channel is gone (client hung up); send nothing.
    ClientGone,
    /// The enforce-mode authorizer refused the execution-time shape.
    Denied(String),
    /// A parameter cannot bind (BlobRef in v1).
    BadParam(String),
    /// Engine error, possibly an interrupt.
    Sqlite(rusqlite::Error),
}

struct Emitted {
    rows_out: i64,
    bytes_out: i64,
}

fn run_execute(conn: &mut Connection, job: ExecJob, ftx: mpsc::Sender<EnvelopeObject>) {
    harden_connection(conn);

    let timed_out = Arc::new(AtomicBool::new(false));
    let steps_exceeded = Arc::new(AtomicBool::new(false));
    let outcome = execute_in_tx(conn, &job, &ftx, &timed_out, &steps_exceeded);

    // Defensive disarm: error paths can leave hooks behind.
    let _ = conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    let _ = conn.progress_handler(0, None::<fn() -> bool>);

    match outcome {
        Ok(emitted) => {
            audit_outcome(conn, job.audit_id, emitted.rows_out, emitted.bytes_out);
        }
        Err(Halt::ClientGone) => {
            debug!(id = %job.id, "client went away mid-response");
        }
        Err(Halt::Denied(reason)) => {
            push_frame_lossy(&ftx, error_frame(&job.id, ErrorCode::AuthDenied, &reason));
        }
        Err(Halt::BadParam(msg)) => {
            push_frame_lossy(&ftx, error_frame(&job.id, ErrorCode::EngineError, &msg));
        }
        Err(Halt::Sqlite(e)) => {
            let interrupted = matches!(
                &e,
                rusqlite::Error::SqliteFailure(f, _)
                    if f.code == SqliteErrorCode::OperationInterrupted
            );
            let (code, reason) = if interrupted && steps_exceeded.load(Ordering::SeqCst) {
                (
                    ErrorCode::LimitExceeded,
                    "VDBE step budget exhausted".to_string(),
                )
            } else if interrupted {
                (
                    ErrorCode::Timeout,
                    "wall-clock deadline elapsed; statement interrupted and rolled back"
                        .to_string(),
                )
            } else {
                (ErrorCode::EngineError, e.to_string())
            };
            let _ = timed_out; // flag kept for symmetry/diagnostics
            push_frame_lossy(&ftx, error_frame(&job.id, code, &reason));
        }
    }
}

fn execute_in_tx(
    conn: &mut Connection,
    job: &ExecJob,
    ftx: &mpsc::Sender<EnvelopeObject>,
    timed_out: &Arc<AtomicBool>,
    steps_exceeded: &Arc<AtomicBool>,
) -> Result<Emitted, Halt> {
    let is_write = job.approved.footprint.kind == ActionKind::Write;
    let tx = conn.transaction().map_err(Halt::Sqlite)?;

    // Idempotency: a retransmitted write is answered out of `_applied`
    // without being applied a second time.
    if is_write {
        let recorded: Option<String> = tx
            .query_row(
                "SELECT outcome FROM _applied WHERE request_id = ?1",
                [&job.id],
                |r| r.get(0),
            )
            .optional()
            .map_err(Halt::Sqlite)?;
        if let Some(json) = recorded {
            debug!(id = %job.id, "retransmit answered from _applied");
            return answer_from_applied(&job.id, &json, ftx);
        }
    }

    // Execution-time prepare under the enforce-mode authorizer, so the
    // decision and the execution cannot drift apart.
    let deny_reason: Arc<Mutex<Option<String>>> = Arc::default();
    tx.authorizer(Some(enforce_authorizer(
        job.approved.clone(),
        job.pragma_allowlist.clone(),
        deny_reason.clone(),
    )))
    .map_err(Halt::Sqlite)?;
    let prep = tx.prepare(&job.sql);
    let _ = tx.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    let mut stmt = prep.map_err(
        |e| match deny_reason.lock().ok().and_then(|mut g| g.take()) {
            Some(r) => Halt::Denied(r),
            None => Halt::Sqlite(e),
        },
    )?;

    // Limits: the progress handler is the cpu meter and the wall-clock
    // backstop.
    let deadline = Instant::now() + Duration::from_millis(job.limits.timeout_ms);
    {
        let steps = steps_exceeded.clone();
        let timed = timed_out.clone();
        let budget_units = job.limits.step_budget / PROGRESS_GRANULARITY as u64;
        let mut units: u64 = 0;
        tx.progress_handler(
            PROGRESS_GRANULARITY,
            Some(move || {
                units += 1;
                if units > budget_units {
                    steps.store(true, Ordering::SeqCst);
                    return true;
                }
                if Instant::now() >= deadline {
                    timed.store(true, Ordering::SeqCst);
                    return true;
                }
                false
            }),
        )
        .map_err(Halt::Sqlite)?;
    }

    // Header.
    let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let decl_types: Vec<String> = stmt
        .columns()
        .iter()
        .map(|c| c.decl_type().unwrap_or("").to_string())
        .collect();
    let header = EnvelopeObject::Result(ResultHeader {
        id: job.id.clone(),
        columns: columns.clone(),
        decl_types,
    });
    let mut bytes_out: i64 = header_estimate(&columns);
    push_frame(ftx, header)?;

    // Bind and step.
    for p in &job.params {
        if let Value::BlobRef { .. } = p {
            return Err(Halt::BadParam(
                "rsntr:BlobRef parameters are not supported yet".into(),
            ));
        }
    }
    let mut rows = stmt
        .query(rusqlite::params_from_iter(
            job.params.iter().map(BoundParam),
        ))
        .map_err(Halt::Sqlite)?;

    let mut seq: i64 = 0;
    let mut truncated = false;
    let mut batch: Vec<Row> = Vec::new();
    let mut batch_bytes: usize = 0;
    loop {
        let row = match rows.next() {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => return Err(Halt::Sqlite(e)),
        };
        if seq >= job.limits.row_cap {
            truncated = true;
            break;
        }
        let (proto_row, est) = row_from_sql(row, &columns, seq).map_err(Halt::Sqlite)?;
        if bytes_out + (batch_bytes + est) as i64 > job.limits.byte_cap {
            truncated = true;
            break;
        }
        batch.push(proto_row);
        batch_bytes += est;
        seq += 1;
        if batch.len() >= job.rows_per_frame || batch_bytes >= job.frame_byte_budget {
            bytes_out += batch_bytes as i64;
            push_frame(ftx, EnvelopeObject::Row(std::mem::take(&mut batch)))?;
            batch_bytes = 0;
        }
    }
    if !batch.is_empty() {
        bytes_out += batch_bytes as i64;
        push_frame(ftx, EnvelopeObject::Row(std::mem::take(&mut batch)))?;
    }
    drop(rows);
    drop(stmt);
    let _ = tx.progress_handler(0, None::<fn() -> bool>);

    // Outcome bookkeeping inside the same transaction, then commit.
    let (affected, last_rowid) = if is_write {
        let affected = tx.changes() as i64;
        let last_rowid = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO _applied (request_id, at, outcome) VALUES (?1, ?2, ?3)",
            (
                &job.id,
                now_rfc3339(),
                serde_json::json!({
                    "affected_rows": affected,
                    "last_insert_rowid": last_rowid,
                })
                .to_string(),
            ),
        )
        .map_err(Halt::Sqlite)?;
        (Some(affected), Some(last_rowid))
    } else {
        (None, None)
    };
    tx.commit().map_err(Halt::Sqlite)?;

    push_frame(
        ftx,
        EnvelopeObject::Done(Done {
            id: job.id.clone(),
            row_count: Some(seq),
            affected_rows: affected,
            last_insert_rowid: last_rowid,
            truncated,
        }),
    )?;
    Ok(Emitted {
        rows_out: seq,
        bytes_out,
    })
}

/// Answers a retransmitted write from its `_applied` record.
fn answer_from_applied(
    id: &str,
    outcome_json: &str,
    ftx: &mpsc::Sender<EnvelopeObject>,
) -> Result<Emitted, Halt> {
    let outcome: serde_json::Value = serde_json::from_str(outcome_json).unwrap_or_default();
    let affected = outcome.get("affected_rows").and_then(|v| v.as_i64());
    let last_rowid = outcome.get("last_insert_rowid").and_then(|v| v.as_i64());
    push_frame(
        ftx,
        EnvelopeObject::Result(ResultHeader {
            id: id.to_string(),
            columns: Vec::new(),
            decl_types: Vec::new(),
        }),
    )?;
    push_frame(
        ftx,
        EnvelopeObject::Done(Done {
            id: id.to_string(),
            row_count: Some(0),
            affected_rows: affected,
            last_insert_rowid: last_rowid,
            truncated: false,
        }),
    )?;
    Ok(Emitted {
        rows_out: 0,
        bytes_out: 0,
    })
}

/// Conservative `sqlite3_limit` values for untrusted statements.
fn harden_connection(conn: &Connection) {
    use rusqlite::limits::Limit;
    let _ = conn.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0);
    let _ = conn.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 1_000_000);
    let _ = conn.set_limit(Limit::SQLITE_LIMIT_COMPOUND_SELECT, 64);
    let _ = conn.set_limit(Limit::SQLITE_LIMIT_EXPR_DEPTH, 256);
    let _ = conn.set_limit(Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 8);
}

/// One sqlite row into a protocol row plus its estimated encoded size.
/// NULL cells are skipped (column-omission encoding).
fn row_from_sql(
    row: &rusqlite::Row<'_>,
    columns: &[String],
    seq: i64,
) -> Result<(Row, usize), rusqlite::Error> {
    use rusqlite::types::ValueRef;
    let mut cells = Vec::with_capacity(columns.len());
    let mut est = 32usize; // per-row envelope overhead
    for (i, name) in columns.iter().enumerate() {
        let value = match row.get_ref(i)? {
            ValueRef::Null => continue,
            ValueRef::Integer(v) => {
                est += 20;
                Value::Integer(v)
            }
            ValueRef::Real(v) => {
                est += 26;
                Value::Real(v)
            }
            ValueRef::Text(t) => {
                est += t.len() + 16;
                Value::Text(String::from_utf8_lossy(t).into_owned())
            }
            ValueRef::Blob(b) => {
                est += b.len() * 4 / 3 + 32;
                Value::Blob(b.to_vec())
            }
        };
        est += name.len() + 12;
        cells.push((name.clone(), value));
    }
    Ok((Row { seq, cells }, est))
}

fn header_estimate(columns: &[String]) -> i64 {
    (64 + columns.iter().map(|c| c.len() + 8).sum::<usize>()) as i64
}

/// ToSql adapter over protocol values (BlobRef is rejected before
/// binding ever happens).
struct BoundParam<'a>(&'a Value);

impl ToSql for BoundParam<'_> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        use rusqlite::types::Value as SqlValue;
        Ok(match self.0 {
            Value::Null => ToSqlOutput::Owned(SqlValue::Null),
            Value::Integer(v) => ToSqlOutput::Owned(SqlValue::Integer(*v)),
            Value::Real(v) => ToSqlOutput::Owned(SqlValue::Real(*v)),
            Value::Text(s) => ToSqlOutput::Owned(SqlValue::Text(s.clone())),
            Value::Blob(b) => ToSqlOutput::Owned(SqlValue::Blob(b.clone())),
            Value::BlobRef { .. } => {
                return Err(rusqlite::Error::ToSqlConversionFailure(
                    "BlobRef parameters are not supported".into(),
                ));
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Frame plumbing
// ---------------------------------------------------------------------------

/// A Done frame carrying only the correlation id (entrain ack/confirm).
fn bare_done(id: &str) -> EnvelopeObject {
    EnvelopeObject::Done(Done {
        id: id.to_string(),
        row_count: None,
        affected_rows: None,
        last_insert_rowid: None,
        truncated: false,
    })
}

fn error_frame(id: &str, code: ErrorCode, reason: &str) -> EnvelopeObject {
    EnvelopeObject::Error(ErrorEnvelope {
        id: Some(id.to_string()),
        code: code.as_str().to_string(),
        reason: Some(reason.to_string()),
    })
}

/// Blocking send from the db thread; a closed channel means the client
/// hung up.
fn push_frame(ftx: &mpsc::Sender<EnvelopeObject>, obj: EnvelopeObject) -> Result<(), Halt> {
    ftx.blocking_send(obj).map_err(|_| Halt::ClientGone)
}

fn push_frame_lossy(ftx: &mpsc::Sender<EnvelopeObject>, obj: EnvelopeObject) {
    if ftx.blocking_send(obj).is_err() {
        debug!("response channel closed while sending final frame");
    }
}

async fn send_denied<S: RequestStream>(
    stream: &mut S,
    id: Option<String>,
    reason: String,
) -> Result<(), NodeError> {
    stream
        .send(&EnvelopeObject::Denied(Denied {
            id,
            reason: Some(reason),
        }))
        .await?;
    Ok(())
}

async fn send_error<S: RequestStream>(
    stream: &mut S,
    id: Option<String>,
    code: ErrorCode,
    reason: String,
) -> Result<(), NodeError> {
    stream
        .send(&EnvelopeObject::Error(ErrorEnvelope {
            id,
            code: code.as_str().to_string(),
            reason: Some(reason),
        }))
        .await?;
    Ok(())
}
