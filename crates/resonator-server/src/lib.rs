//! The resonator wave server: SQLite storage, the per-wavelet
//! submit/transform/apply pipeline, accounts, authentication, and the HTTP
//! and WebSocket front end.
//!
//! Derived in large part from `org.waveprotocol.box.server` of Apache Wave
//! (Apache License 2.0); the modules say what they were ported from.
//!
//! - [`storage`]: the database, the delta store and the account store.
//! - [`waveserver`]: wavelet containers, the wave server and the wave bus.
//! - [`account`]: account data.
//! - [`auth`]: registration, sign-in, sessions and bearer tokens; resolves
//!   every caller to an [`auth::Identity`].
//! - [`frontend`]: wave view subscriptions and submission on behalf of a
//!   participant; the layer for anything that acts as one, in process or
//!   over the network.
//! - [`rpc`]: the client wave protocol over a WebSocket.
//! - [`search`]: the search index, the query language and the inbox.
//! - [`attachment`]: attachment files, their metadata and thumbnails.
//! - [`welcome`]: the welcome wave for new accounts.
//! - [`http`]: the HTTP server and its endpoints.
//! - [`metrics`]: counters for `/metrics`.
//! - [`config`]: configuration from file, environment and command line.
//! - [`app`]: start-up and shutdown of the server process.
//! - [`clock`]: the injected time source.

pub mod account;
pub mod app;
pub mod attachment;
pub mod auth;
pub mod clock;
pub mod config;
pub mod frontend;
pub mod http;
pub mod metrics;
pub mod rpc;
pub mod search;
pub mod storage;
pub mod waveserver;
pub mod welcome;
