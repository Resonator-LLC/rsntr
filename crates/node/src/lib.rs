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

pub mod apps;
pub mod audit;
pub mod chat;
pub mod clock;
pub mod db;
pub mod ddl;
pub mod entrain;
pub mod error;
pub mod footprint;
pub mod help;
pub mod media;
pub mod mod_handler;
pub mod mod_install;
pub mod pipeline;
pub mod projection;
pub mod sparql_mod;
pub mod system;

pub use apps::{
    AppInfo, AppRow, TableDecl, claimed_by, declare_statements, declare_tables, list_apps,
    register_app, touch_app,
};
pub use chat::{
    BODY_MAX_BYTES, BlobAttachment, CHAT_HISTORY_POINT, CHAT_INBOX_POINT, CHAT_LOG_VIEW_DDL,
    CHAT_SEND_POINT, CHAT_TABLES_DDL, ChatMessage, ChatParseError, DIRECT_RESOURCE, RE_MAX_CHARS,
    TAG_MAX_BYTES, TAGS_MAX, ensure_chat_columns, message_to_turtle, parse_message, tags_from_json,
    tags_to_json, validate_thread_fields,
};
pub use db::{BUSY_TIMEOUT, DbHandle, is_busy, migrate, open_node_db, open_node_db_in_memory};
pub use ddl::{
    APPLIED_DDL, AUDIT_DDL, DEFAULT_MODS, INBOX_DDL, INBOX_POINT, KNOCK_BUDGET_DDL, MEDIA_DDL,
    MODULATIONS_DDL, PEERS_DDL, PROJECTION_TABLE_DDL, RSNTR_DDL, enabled_mod_names,
    ensure_modulations_table, ensure_node_tables, get_rsntr, node_hello, seed_rsntr_defaults,
    served_modulations, set_rsntr,
};
pub use ddl::{
    MIN_READER_VERSION, SCHEMA_VERSION, check_schema_compat, schema_is_current,
    stamp_schema_version,
};
pub use entrain::Entrainments;
pub use error::NodeError;
pub use footprint::{Approved, CollectState, Lane};
pub use media::{FeedChunk, FeedReader, FeedWriter, MediaFeeds};
pub use mod_handler::{ModHandler, ModHandlerFuture};
pub use pipeline::ChangeWatcher;
pub use pipeline::{
    BlobImporter, KnockLimits, ModDbOutcome, Node, NodeConfig, PostDecideHook, peer_known,
    run_mod_statement,
};
pub use system::{GRAPH_SIGNAL, GraphSections, SYSTEM_MOD, build_system_graph};
