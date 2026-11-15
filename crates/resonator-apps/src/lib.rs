//! Wave apps: the runtime that runs one wasm component per gadget on every
//! node of a wave.
//!
//! An app is a component of the world `resonator:app/wave-app` (see
//! `wit/app.wit`), shipped as a zip bundle with a manifest. The node hands
//! it events (wave messages, peers, UI events) one at a time; the guest is
//! called synchronously under a deadline and a memory limit. It reads from
//! a [`Snapshot`] the node keeps up to date, and everything it wants to
//! change comes back as a list of [`Effect`]s that the node carries out
//! after the call. So this crate knows nothing of the wave server, and a
//! guest can never block the node.
//!
//! WASI is linked with an empty context: no files, no sockets, no
//! environment, no stdio. Only clocks and random do anything.

mod bundle;
mod engine;
mod error;
mod host;
mod instance;
mod manifest;
mod worker;

pub use bundle::{Bundle, MAX_BUNDLE_BYTES, MAX_MANIFEST_BYTES, MAX_WASM_BYTES, app_id};
pub use engine::{AppEngine, Component, EngineOptions};
pub use error::AppError;
pub use host::{Effect, LogLevel, Snapshot};
pub use instance::{Deadlines, Instance, InstanceConfig};
pub use manifest::{DEFAULT_MEMORY_MB, MAX_MEMORY_MB, Manifest, Permission};
pub use worker::{Event, InstanceHandle, Report, spawn};

/// The generated bindings of `wit/app.wit`.
mod bindings {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "wave-app",
        additional_derives: [PartialEq, Eq],
    });
}

pub use bindings::resonator::app::types::{Peer, StreamMode, UiEvent, UiNode, WaveMessage};
