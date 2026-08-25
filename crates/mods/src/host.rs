//! The host functions plugins call, and the per-request state they act
//! on.
//!
//! Host functions are baked into the cached `CompiledPlugin`, so they
//! cannot capture per-request data. Plugin calls are synchronous and run
//! on one blocking-pool thread per request, so the per-request state
//! lives in a thread local installed around `plugin.call` ([`StateGuard`])
//! and every host function reads it from there. A host function invoked
//! outside a request (e.g. from `describe()`), or one whose capability
//! was not granted, returns an error, which traps the plugin with that
//! message.
//!
//! Capability gates: `db_query` needs `db_read`, `db_execute` needs
//! `db_write`, `now_ns` needs `clock`. `emit_frame`, `log`, and
//! `config_get` are always available.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use extism::convert::Json;
use extism::{Error, Manifest, PTR, PluginBuilder, UserData};
use tokio::sync::mpsc;

use resonator_authenticator::Chain;
use resonator_mod_pdk::{DbExec, DbRows, FrameOut, Value as PdkValue};
use resonator_node::{DbHandle, ModDbOutcome, NodeConfig, run_mod_statement};
use resonator_protocol::{EnvelopeObject, MAX_FRAME_LEN, Value as ProtoValue};

/// Everything one in-flight request exposes to the host functions.
pub(crate) struct RequestState {
    pub mod_name: String,
    pub request_id: String,
    pub peer: String,
    pub caps: BTreeSet<String>,
    pub config: serde_json::Map<String, serde_json::Value>,
    pub db: DbHandle,
    pub chain: Arc<Chain>,
    pub node_config: Arc<NodeConfig>,
    /// Runtime handle for bridging the blocking thread back into async
    /// (`db.call`).
    pub rt: tokio::runtime::Handle,
    pub frames: mpsc::Sender<EnvelopeObject>,
    pub row_cap: i64,
    pub byte_cap: i64,
    pub rows_out: Cell<i64>,
    pub bytes_out: Cell<i64>,
    pub frames_out: Cell<i64>,
}

thread_local! {
    static CURRENT: RefCell<Option<Rc<RequestState>>> = const { RefCell::new(None) };
}

/// Installs a request's state in the thread local for the duration of
/// one `plugin.call`; clears it on drop.
pub(crate) struct StateGuard(());

impl StateGuard {
    pub fn install(state: Rc<RequestState>) -> Self {
        CURRENT.with(|c| *c.borrow_mut() = Some(state));
        StateGuard(())
    }
}

impl Drop for StateGuard {
    fn drop(&mut self) {
        CURRENT.with(|c| c.borrow_mut().take());
    }
}

fn state() -> Result<Rc<RequestState>, Error> {
    CURRENT
        .with(|c| c.borrow().clone())
        .ok_or_else(|| Error::msg("host function called outside an active mod request"))
}

impl RequestState {
    fn need_cap(&self, cap: &str, what: &str) -> Result<(), Error> {
        if self.caps.contains(cap) {
            Ok(())
        } else {
            Err(Error::msg(format!(
                "mod {:?} was not granted the {cap:?} capability required by {what}",
                self.mod_name
            )))
        }
    }

    /// Renumbers rows onto the wire's dense 0-based sequence, serializes
    /// (validating), enforces the frame/row/byte budgets, and pushes one
    /// frame to the response stream.
    fn account_and_send(&self, mut obj: EnvelopeObject) -> Result<(), Error> {
        if let EnvelopeObject::Row(rows) = &mut obj {
            let n = rows.len() as i64;
            if self.rows_out.get() + n > self.row_cap {
                return Err(Error::msg(format!("row limit ({}) exceeded", self.row_cap)));
            }
            // The ABI's seq prop is plugin bookkeeping (1-based by
            // convention); the wire wants rows numbered densely from 0
            // in emission order, so the host assigns them.
            for (i, row) in rows.iter_mut().enumerate() {
                row.seq = self.rows_out.get() + i as i64;
            }
            self.rows_out.set(self.rows_out.get() + n);
        }
        let turtle = obj
            .to_turtle()
            .map_err(|e| Error::msg(format!("emitted frame does not serialize: {e}")))?;
        if turtle.len() > MAX_FRAME_LEN {
            return Err(Error::msg(format!(
                "emitted frame is {} bytes; the frame cap is {MAX_FRAME_LEN}",
                turtle.len()
            )));
        }
        let total = self.bytes_out.get() + turtle.len() as i64;
        if total > self.byte_cap {
            return Err(Error::msg(format!(
                "byte limit ({}) exceeded",
                self.byte_cap
            )));
        }
        self.bytes_out.set(total);
        self.frames_out.set(self.frames_out.get() + 1);
        self.frames
            .blocking_send(obj)
            .map_err(|_| Error::msg("client went away; response stream closed"))
    }

    /// Bridges one plugin statement onto the db thread through the full
    /// gate/decide/enforce path. Deny and engine failures surface as
    /// errors (trapping the plugin unless it handles them).
    fn run_db(
        &self,
        sql: String,
        params: Vec<PdkValue>,
        write_allowed: bool,
    ) -> Result<ModDbOutcome, Error> {
        let params = params
            .iter()
            .map(pdk_to_proto)
            .collect::<Result<Vec<_>, _>>()?;
        let db = self.db.clone();
        let chain = self.chain.clone();
        let config = self.node_config.clone();
        let peer = self.peer.clone();
        let id = self.request_id.clone();
        let outcome = self
            .rt
            .block_on(db.call(move |conn| {
                run_mod_statement(
                    conn,
                    &peer,
                    &id,
                    &chain,
                    &config,
                    &sql,
                    &params,
                    write_allowed,
                )
            }))
            .map_err(|e| Error::msg(format!("db thread failed: {e}")))?;
        match outcome {
            ModDbOutcome::Denied { reason } => Err(Error::msg(format!("denied: {reason}"))),
            ModDbOutcome::Error { message } => Err(Error::msg(message)),
            ok => Ok(ok),
        }
    }
}

// ---------------------------------------------------------------------------
// Value conversions (ABI JSON <-> protocol / sqlite)
// ---------------------------------------------------------------------------

/// ABI value -> protocol value (base64 blobs decoded).
pub(crate) fn pdk_to_proto(v: &PdkValue) -> Result<ProtoValue, Error> {
    Ok(match v {
        PdkValue::Null => ProtoValue::Null,
        PdkValue::Integer(n) => ProtoValue::Integer(*n),
        PdkValue::Real(f) => ProtoValue::Real(*f),
        PdkValue::Text(s) => ProtoValue::Text(s.clone()),
        PdkValue::Blob(_) => ProtoValue::Blob(
            v.blob_bytes()
                .ok_or_else(|| Error::msg("blob value is not valid base64"))?,
        ),
        PdkValue::BlobRef { hash, bytes } => ProtoValue::BlobRef {
            hash: hash.clone(),
            bytes: *bytes,
        },
    })
}

/// Protocol value -> ABI value (blobs to base64).
pub(crate) fn proto_to_pdk(v: &ProtoValue) -> PdkValue {
    match v {
        ProtoValue::Null => PdkValue::Null,
        ProtoValue::Integer(n) => PdkValue::Integer(*n),
        ProtoValue::Real(f) => PdkValue::Real(*f),
        ProtoValue::Text(s) => PdkValue::Text(s.clone()),
        ProtoValue::Blob(b) => PdkValue::blob_from_bytes(b),
        ProtoValue::BlobRef { hash, bytes } => PdkValue::BlobRef {
            hash: hash.clone(),
            bytes: *bytes,
        },
    }
}

/// sqlite value -> ABI value, for db_query result rows.
fn sql_to_pdk(v: rusqlite::types::Value) -> PdkValue {
    use rusqlite::types::Value as Sql;
    match v {
        Sql::Null => PdkValue::Null,
        Sql::Integer(n) => PdkValue::Integer(n),
        Sql::Real(f) => PdkValue::Real(f),
        Sql::Text(s) => PdkValue::Text(s),
        Sql::Blob(b) => PdkValue::blob_from_bytes(&b),
    }
}

// ---------------------------------------------------------------------------
// The host functions
// ---------------------------------------------------------------------------

extism::host_fn!(emit_frame(frame: Json<FrameOut>) {
    let st = state()?;
    let obj = crate::frames::frame_to_envelope(&st.request_id, &frame.0)?;
    st.account_and_send(obj)?;
    Ok(())
});

extism::host_fn!(db_query(sql: String, params: Json<Vec<PdkValue>>) -> Json<DbRows> {
    let st = state()?;
    st.need_cap("db_read", "db_query")?;
    match st.run_db(sql, params.0, false)? {
        ModDbOutcome::Rows { columns, rows } => Ok(Json(DbRows {
            columns,
            rows: rows
                .into_iter()
                .map(|r| r.into_iter().map(sql_to_pdk).collect())
                .collect(),
        })),
        // run_db(write_allowed = false) refuses writes before execution.
        _ => Err(Error::msg("db_query produced a non-row outcome")),
    }
});

extism::host_fn!(db_execute(sql: String, params: Json<Vec<PdkValue>>) -> Json<DbExec> {
    let st = state()?;
    st.need_cap("db_write", "db_execute")?;
    match st.run_db(sql, params.0, true)? {
        ModDbOutcome::Executed { rows_affected } => Ok(Json(DbExec {
            rows_affected: rows_affected.max(0) as u64,
        })),
        // A read statement through db_execute affects nothing.
        _ => Ok(Json(DbExec { rows_affected: 0 })),
    }
});

extism::host_fn!(log(level: String, message: String) {
    let st = state()?;
    match level.as_str() {
        "error" => tracing::error!(mod_name = %st.mod_name, request_id = %st.request_id, "{message}"),
        "warn" => tracing::warn!(mod_name = %st.mod_name, request_id = %st.request_id, "{message}"),
        "debug" => tracing::debug!(mod_name = %st.mod_name, request_id = %st.request_id, "{message}"),
        "trace" => tracing::trace!(mod_name = %st.mod_name, request_id = %st.request_id, "{message}"),
        _ => tracing::info!(mod_name = %st.mod_name, request_id = %st.request_id, "{message}"),
    }
    Ok(())
});

extism::host_fn!(now_ns() -> Json<i64> {
    let st = state()?;
    st.need_cap("clock", "now_ns")?;
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Error::msg(format!("system clock is before the epoch: {e}")))?
        .as_nanos();
    Ok(Json(i64::try_from(ns).map_err(|_| Error::msg("clock overflow"))?))
});

extism::host_fn!(config_get(key: String) -> Json<Option<String>> {
    let st = state()?;
    Ok(Json(st.config.get(&key).map(|v| match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    })))
});

/// The builder every mod is compiled with: sandboxed (no WASI) plus the
/// six ABI v1 host functions.
pub(crate) fn plugin_builder(manifest: Manifest) -> PluginBuilder<'static> {
    PluginBuilder::new(manifest)
        .with_wasi(false)
        .with_function("emit_frame", [PTR], [], UserData::new(()), emit_frame)
        .with_function("db_query", [PTR, PTR], [PTR], UserData::new(()), db_query)
        .with_function(
            "db_execute",
            [PTR, PTR],
            [PTR],
            UserData::new(()),
            db_execute,
        )
        .with_function("log", [PTR, PTR], [], UserData::new(()), log)
        .with_function("now_ns", [], [PTR], UserData::new(()), now_ns)
        .with_function("config_get", [PTR], [PTR], UserData::new(()), config_get)
}
