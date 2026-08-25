//! resonator-mods: the extism host for wasm modulation plugins.
//!
//! A mod is a `.wasm` plugin registered in the `_modulations` table
//! (name, blob, sha256, enabled bit, granted caps, config, limits) and
//! advertised in the node's hello next to the builtins. Requests whose
//! modulation matches a loaded mod run its `handle()` export in a fresh
//! plugin instance per request, on a blocking thread, streaming frames
//! back through the pipeline's forwarder; anything else still answers
//! `mod-unsupported`.
//!
//! Security model (extism plan sec 6): no WASI, no filesystem, no
//! network; manifest wall-clock and memory limits; invocation gated by
//! `_policy` action `mod:<name>` through the authenticator chain; every
//! db_query/db_execute statement takes the same footprint -> chain ->
//! enforce path as a sql-sqlite request from the same peer, so the unit
//! of authorization is each statement the plugin runs. One `_audit` row
//! per invocation plus the normal per-statement rows.
//!
//! ABI v1 (`resonator-mod-pdk`): exports `describe()` and
//! `handle(StatementIn) -> HandleResult`; host functions `emit_frame`,
//! `db_query` (cap `db_read`), `db_execute` (cap `db_write`), `log`,
//! `now_ns` (cap `clock`), `config_get`. JSON crosses the boundary in
//! the pdk crate's shapes.

mod error;
mod frames;
mod handler;
mod host;
mod registry;
mod store;

pub use error::ModError;
pub use handler::ModsHost;
pub use registry::{
    DEFAULT_MEMORY_MB, EnabledRow, ModEntry, ModLimits, ModRegistry, describe_stored,
    describe_wasm, read_enabled_rows,
};
pub use resonator_mod_pdk::Descriptor;
pub use store::{ModRow, mod_add, mod_list, mod_rm, mod_set_enabled, sha256_hex};
