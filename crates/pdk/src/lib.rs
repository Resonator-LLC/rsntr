//! Plugin development kit for Resonator mods.
//!
//! A mod is a wasm plugin that registers one modulation on a node (row in
//! the `_modulations` table). It exports two functions, `describe` and
//! `handle`, and talks back to the node only through granted host
//! functions ([`host`]). All structured data crosses the boundary as JSON
//! in the shapes of [`types`] (ABI v1).
//!
//! Build with `cargo build --target wasm32-unknown-unknown --release` from
//! a `cdylib` crate depending on this crate and `extism-pdk`.
//!
//! A complete minimal mod:
//!
//! ```ignore
//! use extism_pdk::{FnResult, Json, plugin_fn};
//! use resonator_mod_pdk::{Descriptor, HandleResult, StatementIn, Value, host};
//!
//! #[plugin_fn]
//! pub fn describe() -> FnResult<Json<Descriptor>> {
//!     Ok(Json(Descriptor {
//!         abi: 1,
//!         name: "echo".to_string(),
//!         version: "0.1.0".to_string(),
//!         help_text: "echoes the request text back as one row".to_string(),
//!         topics: vec![],
//!         needs: vec![],
//!     }))
//! }
//!
//! #[plugin_fn]
//! pub fn handle(Json(st): Json<StatementIn>) -> FnResult<Json<HandleResult>> {
//!     host::emit_result(&["echo"])?;
//!     host::emit_row(1, vec![("echo".to_string(), Value::Text(st.text))])?;
//!     host::emit_done(1)?;
//!     Ok(Json(HandleResult::done()))
//! }
//! ```

mod types;

pub use types::{
    DbExec, DbRows, Descriptor, FrameOut, HandleResult, Kind, PropValue, StatementIn, Value,
};

#[cfg(target_arch = "wasm32")]
pub mod host;
