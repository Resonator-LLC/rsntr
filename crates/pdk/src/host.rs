//! Host function bindings and safe wrappers (wasm32 only).
//!
//! Every argument and result crosses the boundary as an extism memory
//! block; structured data is JSON in the shapes of [`crate::types`].
//! Each host function is granted per-mod in the `_modulations` row; calling
//! one that was not granted traps with a clear error.

use extism_pdk::{Error, Json, host_fn};

use crate::types::{DbExec, DbRows, FrameOut, Value};

// Raw extern declarations live in a private module so the safe wrappers
// below can reuse the plain names (`log`, `now_ns`, ...).
mod raw {
    use super::*;

    #[host_fn]
    extern "ExtismHost" {
        pub fn emit_frame(frame: Json<FrameOut>);
        pub fn db_query(sql: String, params: Json<Vec<Value>>) -> Json<DbRows>;
        pub fn db_execute(sql: String, params: Json<Vec<Value>>) -> Json<DbExec>;
        pub fn log(level: String, message: String);
        pub fn now_ns() -> Json<i64>;
        pub fn config_get(key: String) -> Json<Option<String>>;
    }
}

/// Emits one frame to the response stream. The host stamps
/// `rsntr:requestId` and enforces row/byte/frame-size budgets.
pub fn emit(frame: &FrameOut) -> Result<(), Error> {
    unsafe { raw::emit_frame(Json(frame.clone())) }
}

/// Emits a `Result` header frame opening a row-streaming response.
pub fn emit_result(columns: &[&str]) -> Result<(), Error> {
    emit(&FrameOut::result(columns))
}

/// Emits one `Row` frame. `seq` is 1-based.
pub fn emit_row(seq: i64, cells: Vec<(String, Value)>) -> Result<(), Error> {
    emit(&FrameOut::row(seq, cells))
}

/// Emits the `Done` trailer closing the response after `rows` rows.
pub fn emit_done(rows: i64) -> Result<(), Error> {
    emit(&FrameOut::done(rows))
}

/// Runs a read query on the node db. Requires the `db_read` capability;
/// the statement passes the node's footprint + authenticator path exactly
/// like a sql-sqlite request from the same peer.
pub fn db_query(sql: &str, params: &[Value]) -> Result<DbRows, Error> {
    Ok(unsafe { raw::db_query(sql.to_string(), Json(params.to_vec())) }?.0)
}

/// Runs a write statement on the node db. Requires the `db_write`
/// capability; gated like [`db_query`]. Returns rows affected.
pub fn db_execute(sql: &str, params: &[Value]) -> Result<u64, Error> {
    Ok(
        unsafe { raw::db_execute(sql.to_string(), Json(params.to_vec())) }?
            .0
            .rows_affected,
    )
}

/// Logs through the host's tracing, tagged with mod name and request id.
/// `level` is one of "trace", "debug", "info", "warn", "error".
pub fn log(level: &str, message: &str) -> Result<(), Error> {
    unsafe { raw::log(level.to_string(), message.to_string()) }
}

/// Wall clock, nanoseconds since the Unix epoch. Requires the `clock`
/// capability (plugins get no WASI clocks).
pub fn now_ns() -> Result<i64, Error> {
    Ok(unsafe { raw::now_ns() }?.0)
}

/// Reads one key from this mod's `_modulations.config` JSON.
pub fn config_get(key: &str) -> Result<Option<String>, Error> {
    Ok(unsafe { raw::config_get(key.to_string()) }?.0)
}
