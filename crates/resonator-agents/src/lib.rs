//! Robots, tools and AI agents for the resonator wave server.
//!
//! See `HANDOFF.md` in this crate for the state of the work.
//!
//! - [`runtime::Integrations::mount`]: the one call that sets everything up
//!   on a prepared server and returns the extra HTTP routes.
//! - [`adapters`]: the server's search, attachments and profiles for the
//!   Robots API layer.
//! - [`http`]: the Data API, `/robot/rpc`, robot registration and the robot
//!   event stream.
//! - [`mcp`]: the Model Context Protocol endpoint.
//! - [`agent`]: the built-in AI agents.
//! - [`tools`]: wave tools shared by the agents and MCP.
//! - [`robots`]: address policy, signatures, capabilities, the connector.
//! - [`llm`]: the OpenAI-compatible model client.

pub mod adapters;
pub mod agent;
pub mod http;
pub mod llm;
pub mod mcp;
pub mod outline;
pub mod robots;
pub mod runtime;
pub mod tools;

pub use runtime::{Integrations, MountError};
