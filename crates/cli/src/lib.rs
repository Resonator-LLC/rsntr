//! rsntr: the resonator console tool, as a library.
//!
//! `main.rs` is argument parsing and printing only; every command is a
//! library function here, so tests can run two nodes in-process and the
//! python crate can reuse the same primitives.
//!
//! - [`store`]: node directory layout (`rsntr.db` + `rsntr.key`),
//!   `rsntr init`, `rsntr id`, the `_peers` and `_media` registries.
//! - [`serve`]: `rsntr serve` as a startable/stoppable [`RunningNode`].
//! - [`client`]: query/help/projection/entrain/media round trips.
//! - [`teletype`]: the numbered-menu projection renderer and the
//!   invocation builder.
//! - [`output`]: value rendering for terminals and the stable `--json`
//!   machine output.
//!
//! # Exit codes (stable, for agents and shell pipelines)
//!
//! - `0`: success
//! - `1`: error (connection, engine, protocol, or local failure)
//! - `2`: denied (the serving side's authenticator or policy said no)
//! - `3`: timeout (the request's wall-clock deadline elapsed)

pub mod channel;
pub mod chat;
pub mod client;
#[cfg(feature = "web")]
pub mod csvcmd;
pub mod inboxcmd;
#[cfg(feature = "mods")]
pub mod modcmd;
pub mod output;
#[cfg(unix)]
pub mod owner_socket;
pub mod serve;
pub mod sqlcmd;
pub mod store;
pub mod teletype;

pub use channel::{OwnerChannel, Prefer};
pub use chat::{
    LogEntry, SendReport, Target, WatchEvent, chat_init, chat_log, chat_send, chat_watch,
    resolve_target, room_add, room_create, room_join,
};
pub use client::{
    EntrainItem, FetchOutcome, MediaChunk, ProjectionOutcome, QueryOutcome, QueryReport,
    classify_sparql, classify_sql, run_entrain, run_fetch, run_help, run_media_channel,
    run_projection, run_query, run_statement,
};
pub use output::{render_row, render_value};
pub use serve::{RunningNode, start_node};
pub use store::{
    DB_FILE, KEY_FILE, db_path, init_dir, key_path, load_secret, media_add, media_allow,
    media_list, node_id, open_db, peer_add, resolve_peer,
};
pub use teletype::{Invocation, build_invocation, render_projection};

/// Success.
pub const EXIT_OK: i32 = 0;
/// Error: connection, engine, protocol, or local failure.
pub const EXIT_ERROR: i32 = 1;
/// Denied: the serving side's authenticator or policy said no.
pub const EXIT_DENIED: i32 = 2;
/// Timeout: the request's wall-clock deadline elapsed.
pub const EXIT_TIMEOUT: i32 = 3;

/// One line documenting the exit codes, shown in `--help`.
pub const EXIT_CODES_HELP: &str = "Exit codes: 0 ok, 1 error, 2 denied, 3 timeout. \
     --json emits stable machine-readable JSON on stdout.";

/// Test-only scratch directory helper shared by unit and integration
/// tests (no tempfile dependency).
#[doc(hidden)]
pub mod testutil {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A unique directory under the system temp dir, removed on drop.
    pub struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        pub fn new(tag: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "rsntr-{tag}-{}-{}-{n}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
            ));
            std::fs::create_dir_all(&path).expect("creating temp dir");
            Self { path }
        }

        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
