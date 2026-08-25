//! The node-facing side: [`ModsHost`] implements
//! [`ModHandler`](resonator_node::ModHandler).
//!
//! Per request: peer gate, then the chain decides `_policy` action
//! `mod:<name>` (one `_audit` row per invocation), then a fresh plugin
//! instance runs `handle()` inside `spawn_blocking` while its emitted
//! frames flow through the pipeline's mpsc forwarder. Statements the
//! plugin runs via db_query/db_execute take the normal footprint ->
//! chain -> enforce path and write their own per-statement audit rows.

use std::rc::Rc;
use std::time::Instant;

use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use resonator_authenticator::{Chain, Decision, Footprint};
use resonator_mod_pdk::{HandleResult, Kind, StatementIn};
use resonator_node::audit::{audit_direct, audit_full, audit_outcome};
use resonator_node::{DbHandle, ModHandler, ModHandlerFuture, Node, NodeConfig, peer_known};
use resonator_protocol::{Denied, EnvelopeObject, ErrorCode, ErrorEnvelope, Request, RequestKind};

use crate::error::ModError;
use crate::host::{RequestState, StateGuard, proto_to_pdk};
use crate::registry::{ModEntry, ModRegistry, read_enabled_rows};

/// The extism mods host: a loaded [`ModRegistry`] plus the node handles
/// (db thread, chain, config) every invocation runs against.
pub struct ModsHost {
    db: DbHandle,
    chain: Arc<Chain>,
    config: Arc<NodeConfig>,
    registry: ModRegistry,
}

impl ModsHost {
    /// Reads the enabled `_modulations` rows through the db handle and
    /// compiles them off the db thread. Returns the host plus the rows
    /// that were refused, as (name, reason).
    pub async fn load(
        db: DbHandle,
        chain: Arc<Chain>,
        config: Arc<NodeConfig>,
    ) -> Result<(Self, Vec<(String, String)>), ModError> {
        let rows = db.call(|conn| read_enabled_rows(conn)).await??;
        let default_timeout = config.max_duration_ms;
        let (registry, refused) =
            tokio::task::spawn_blocking(move || ModRegistry::from_rows(rows, default_timeout))
                .await
                .expect("registry load task panicked");
        Ok((
            Self {
                db,
                chain,
                config,
                registry,
            },
            refused,
        ))
    }

    /// Builds a host from an already-loaded registry (tests).
    pub fn with_registry(
        db: DbHandle,
        chain: Arc<Chain>,
        config: Arc<NodeConfig>,
        registry: ModRegistry,
    ) -> Self {
        Self {
            db,
            chain,
            config,
            registry,
        }
    }

    /// Loads from the node's own db/chain/config and, when at least one
    /// mod loaded, registers the host on the node. Returns the refused
    /// rows either way.
    pub async fn install(node: &Node) -> Result<Vec<(String, String)>, ModError> {
        let (host, refused) = Self::load(
            node.db().clone(),
            node.chain().clone(),
            node.config().clone(),
        )
        .await?;
        if !host.registry.is_empty() {
            node.set_mod_handler(Box::new(host));
        }
        Ok(refused)
    }

    pub fn registry(&self) -> &ModRegistry {
        &self.registry
    }

    async fn serve(&self, peer: String, request: Request, frames: mpsc::Sender<EnvelopeObject>) {
        let id = request.id_string();
        let Some(entry) = self.registry.find(&request.modulation) else {
            let _ = frames
                .send(error_frame(
                    &id,
                    ErrorCode::ModUnsupported,
                    &format!(
                        "modulation {:?} is not served by this node",
                        request.modulation
                    ),
                ))
                .await;
            return;
        };

        // Invocation gate: peer gate + `_policy` action mod:<name>, one
        // audit row.
        let gate = {
            let peer = peer.clone();
            let id = id.clone();
            let name = entry.name.clone();
            let chain = self.chain.clone();
            let signal = request.signal.clone();
            self.db
                .call(move |conn| gate_mod(conn, &peer, &id, &name, &chain, &signal))
                .await
        };
        let audit_id = match gate {
            Err(e) => {
                let _ = frames
                    .send(error_frame(&id, ErrorCode::EngineError, &e.to_string()))
                    .await;
                return;
            }
            Ok(ModGate::UnknownPeer) => {
                let _ = frames
                    .send(denied_frame(
                        &id,
                        "unknown peer: only rsntr:Knock is accepted",
                    ))
                    .await;
                return;
            }
            Ok(ModGate::Denied { reason }) => {
                let _ = frames.send(denied_frame(&id, &reason)).await;
                return;
            }
            Ok(ModGate::Allowed { audit_id }) => audit_id,
        };

        // Budgets: the request's options clamped by the node ceilings.
        let row_cap = request
            .options
            .row_limit
            .filter(|v| *v >= 0)
            .map_or(self.config.max_rows, |v| v.min(self.config.max_rows));
        let byte_cap = request
            .options
            .byte_limit
            .filter(|v| *v > 0)
            .map_or(self.config.max_response_bytes, |v| {
                v.min(self.config.max_response_bytes)
            });
        let timeout_ms = request
            .options
            .timeout_ms
            .filter(|v| *v > 0)
            .map_or(self.config.max_duration_ms, |v| {
                (v as u64).min(self.config.max_duration_ms)
            });

        let input = StatementIn {
            request_id: id.clone(),
            peer: peer.clone(),
            kind: match request.kind {
                RequestKind::Query => Kind::Query,
                RequestKind::Execute => Kind::Execute,
            },
            text: request.signal.clone(),
            params: request.params.iter().map(proto_to_pdk).collect(),
            row_limit: row_cap.max(0) as u64,
            byte_limit: byte_cap.max(0) as u64,
            timeout_ms,
        };
        let seed = StateSeed {
            mod_name: entry.name.clone(),
            request_id: id.clone(),
            peer,
            entry: entry.clone(),
            db: self.db.clone(),
            chain: self.chain.clone(),
            node_config: self.config.clone(),
            rt: tokio::runtime::Handle::current(),
            frames: frames.clone(),
            row_cap,
            byte_cap,
        };

        let run = tokio::task::spawn_blocking(move || run_plugin(seed, input)).await;
        let (frames_out, bytes_out, result) = match run {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "mod task panicked or was cancelled");
                let _ = frames
                    .send(error_frame(
                        &id,
                        ErrorCode::EngineError,
                        "mod execution failed",
                    ))
                    .await;
                return;
            }
        };

        match result {
            Ok(HandleResult::Done) => {}
            Ok(HandleResult::Error { code, message }) => {
                // The plugin's own error report; its code passes through.
                let _ = frames
                    .send(EnvelopeObject::Error(ErrorEnvelope {
                        id: Some(id.clone()),
                        code,
                        reason: Some(message),
                    }))
                    .await;
            }
            Err(message) => {
                debug!(mod_name = %entry.name, error = %message, "plugin call failed");
                let code = if message.contains("timeout") {
                    ErrorCode::Timeout
                } else {
                    ErrorCode::EngineError
                };
                let _ = frames.send(error_frame(&id, code, &message)).await;
            }
        }

        let _ = self
            .db
            .call(move |conn| audit_outcome(conn, audit_id, frames_out, bytes_out))
            .await;
    }
}

impl ModHandler for ModsHost {
    fn mods(&self) -> Vec<String> {
        self.registry.names()
    }

    fn handle(
        &self,
        peer: String,
        request: Request,
        frames: mpsc::Sender<EnvelopeObject>,
    ) -> ModHandlerFuture<'_> {
        Box::pin(self.serve(peer, request, frames))
    }
}

/// The Send bundle a request carries onto its blocking thread; becomes
/// the thread-local [`RequestState`] there.
struct StateSeed {
    mod_name: String,
    request_id: String,
    peer: String,
    entry: Arc<ModEntry>,
    db: DbHandle,
    chain: Arc<Chain>,
    node_config: Arc<NodeConfig>,
    rt: tokio::runtime::Handle,
    frames: mpsc::Sender<EnvelopeObject>,
    row_cap: i64,
    byte_cap: i64,
}

/// Instantiates a fresh plugin and calls `handle()` with the request
/// state installed for the host functions. Returns (frames emitted,
/// bytes emitted, plugin outcome).
fn run_plugin(seed: StateSeed, input: StatementIn) -> (i64, i64, Result<HandleResult, String>) {
    let mut plugin = match seed.entry.instantiate() {
        Ok(p) => p,
        Err(e) => return (0, 0, Err(e.to_string())),
    };
    let state = Rc::new(RequestState {
        mod_name: seed.mod_name,
        request_id: seed.request_id,
        peer: seed.peer,
        caps: seed.entry.caps.clone(),
        config: seed.entry.config.clone(),
        db: seed.db,
        chain: seed.chain,
        node_config: seed.node_config,
        rt: seed.rt,
        frames: seed.frames,
        row_cap: seed.row_cap,
        byte_cap: seed.byte_cap,
        rows_out: Default::default(),
        bytes_out: Default::default(),
        frames_out: Default::default(),
    });
    let guard = StateGuard::install(state.clone());
    let result = plugin
        .call::<extism::convert::Json<StatementIn>, extism::convert::Json<HandleResult>>(
            "handle",
            extism::convert::Json(input),
        )
        .map(|j| j.0)
        // Alternate anyhow formatting: the whole cause chain, so a host
        // function's refusal reads through the wasm trap wrapper.
        .map_err(|e| format!("{e:#}"));
    drop(guard);
    (state.frames_out.get(), state.bytes_out.get(), result)
}

/// The invocation gate's outcome, decided on the db thread.
enum ModGate {
    UnknownPeer,
    Denied { reason: String },
    Allowed { audit_id: i64 },
}

/// Peer gate, then the chain on action `mod:<name>` with an empty
/// footprint; every outcome writes exactly one `_audit` row.
fn gate_mod(
    conn: &mut Connection,
    peer: &str,
    id: &str,
    mod_name: &str,
    chain: &Chain,
    signal: &str,
) -> ModGate {
    if !peer_known(conn, peer) {
        audit_direct(conn, peer, id, signal, "deny", "peer-gate", "unknown peer");
        return ModGate::UnknownPeer;
    }
    let action = format!("mod:{mod_name}");
    let footprint = Footprint::default();
    let start = Instant::now();
    let decided = chain.decide(conn, peer, &action, &footprint, signal);
    let duration = start.elapsed().as_millis() as u64;
    match decided.decision {
        Decision::Allow | Decision::AllowNarrowed { .. } => {
            let audit_id = audit_full(
                conn,
                peer,
                id,
                signal,
                "{}",
                "allow",
                &decided.decided_by,
                Some(&format!("mod {mod_name} invoked")),
                duration,
            );
            ModGate::Allowed { audit_id }
        }
        Decision::Deny { reason } => {
            audit_full(
                conn,
                peer,
                id,
                signal,
                "{}",
                "deny",
                &decided.decided_by,
                Some(&reason),
                duration,
            );
            ModGate::Denied { reason }
        }
        // The chain's tail default is Deny; Escalate cannot get past it.
        Decision::Escalate => {
            let reason = "authenticator chain did not decide".to_string();
            audit_full(
                conn,
                peer,
                id,
                signal,
                "{}",
                "deny",
                "node",
                Some(&reason),
                duration,
            );
            ModGate::Denied { reason }
        }
    }
}

fn error_frame(id: &str, code: ErrorCode, reason: &str) -> EnvelopeObject {
    EnvelopeObject::Error(ErrorEnvelope {
        id: Some(id.to_string()),
        code: code.as_str().to_string(),
        reason: Some(reason.to_string()),
    })
}

fn denied_frame(id: &str, reason: &str) -> EnvelopeObject {
    EnvelopeObject::Denied(Denied {
        id: Some(id.to_string()),
        reason: Some(reason.to_string()),
    })
}
