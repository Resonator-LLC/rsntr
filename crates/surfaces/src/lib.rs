//! resonator-surfaces: async surfaces over the wire protocol.
//!
//! - [`outbox`]: the send side of the network as plain tables. Applications
//!   INSERT into `_outbox` and read `_results`; the [`outbox::OutboxWorker`]
//!   drives queued rows over a [`resonator_transport::Transport`], with
//!   restart-safe idempotency via stable wire request ids.
//! - [`presence`]: iroh-gossip liveness beacons on a topic derived from the
//!   admitted peer set; received beacons refresh `_peers.last_seen`, and
//!   [`presence::is_stale`] feeds outbox scheduling.
//! - [`remote`]: the SQL vtab surfaces `remote_query(...)` and
//!   `CREATE VIRTUAL TABLE ... USING iroh_remote(...)`: interactive
//!   remote reads and writes as plain SQL over a [`resonator_transport::Transport`].

pub mod outbox;
pub mod presence;
pub mod remote;

pub use outbox::{
    OUTBOX_DDL, OUTBOX_ID_TRIGGER_DDL, OutboxConfig, OutboxError, OutboxHandle, OutboxWaker,
    OutboxWorker, RESULTS_DDL, enqueue, ensure_outbox_tables,
};
pub use presence::{
    DEFAULT_CADENCE, Presence, PresenceConfig, PresenceError, is_stale, topic_id_for,
};
pub use remote::{
    DEFAULT_REMOTE_TIMEOUT, PeerResolver, RemoteContext, RemoteError, RemoteReply,
    register_remote_vtabs, run_remote,
};
