//! resonator-node: the serving pipeline and builtin modulation handlers.
//!
//! Pipeline per request: peer gate against `_peers`, footprint collection
//! with the sqlite authorizer in collect mode, authenticator chain decide,
//! execution under the authorizer re-armed in enforce mode inside a
//! per-request transaction, resource limits (sqlite3_limit, VDBE-step
//! progress meter, wall-clock interrupt, row/byte caps), Result/Row/Done
//! response streaming, `_applied` idempotency for writes, and an
//! unconditional `_audit` record for every request.
//!
//! Builtin modulations: `sql-sqlite`, `sparql` (SELECT/ASK as rows,
//! CONSTRUCT as `rsntr:Graph` frames), `help`, `projection` (plus
//! entrain/Vibration/Damp), `media`, and knock admission.
//!
//! The sqlite connection lives on a dedicated blocking thread
//! ([`DbHandle`]); the async pipeline talks to it over channels. The
//! pipeline is wired to the transport as the server-side handler: feed
//! [`Node::run`] the `IncomingRequest` receiver that
//! [`IrohTransport::bind`](resonator_transport::IrohTransport::bind) (or
//! any other [`Transport`](resonator_transport::Transport) impl) hands
//! out.

pub mod audit;
pub mod chat;
pub mod clock;
pub mod db;
pub mod ddl;
pub mod entrain;
pub mod error;
pub mod footprint;
pub mod help;
pub mod mod_handler;
pub mod pipeline;
pub mod projection;
pub mod sparql_mod;

pub use chat::{
    BODY_MAX_BYTES, BlobAttachment, CHAT_HISTORY_POINT, CHAT_INBOX_POINT, CHAT_SEND_POINT,
    CHAT_TABLES_DDL, ChatMessage, DIRECT_RESOURCE, message_to_turtle, parse_message,
};
pub use db::{DbHandle, open_node_db, open_node_db_in_memory};
pub use ddl::{
    APPLIED_DDL, AUDIT_DDL, DEFAULT_MODS, INBOX_DDL, KNOCK_BUDGET_DDL, MEDIA_DDL, MODULATIONS_DDL,
    PEERS_DDL, PROJECTION_TABLE_DDL, RSNTR_DDL, enabled_mod_names, ensure_modulations_table,
    ensure_node_tables, get_rsntr, node_hello, seed_rsntr_defaults, served_modulations, set_rsntr,
};
pub use entrain::Entrainments;
pub use error::NodeError;
pub use footprint::{Approved, CollectState, Lane};
pub use mod_handler::{ModHandler, ModHandlerFuture};
pub use pipeline::{
    BlobImporter, KnockLimits, ModDbOutcome, Node, NodeConfig, PostDecideHook, peer_known,
    run_mod_statement,
};
