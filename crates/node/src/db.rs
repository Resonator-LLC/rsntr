//! The dedicated sqlite thread.
//!
//! A rusqlite `Connection` is `Send` but not `Sync`, and the pipeline is
//! async; the connection lives on one dedicated blocking thread per
//! database and the pipeline talks to it over channels. A job is a boxed
//! closure over `&mut Connection`; results travel back on a oneshot. The
//! handle also owns the connection's `InterruptHandle` and a
//! busy-generation flag so the wall-clock deadline timer can interrupt
//! exactly the statement it was armed for and nothing else.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;

use rusqlite::{Connection, InterruptHandle};
use tokio::sync::oneshot;
use tracing::debug;

use crate::ddl::ensure_node_tables;
use crate::error::NodeError;

type Job = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

/// Sentinel for "no job with a deadline is executing".
const IDLE: u64 = u64::MAX;

/// Opens (or creates) a node database file: WAL mode, reserved tables
/// ensured, and the `rdf_*` SQL functions plus the `sparql()` vtab
/// registered. This is the connection a [`DbHandle`] should own.
pub fn open_node_db(path: &Path) -> Result<Connection, NodeError> {
    let conn = Connection::open(path)?;
    let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    prepare_connection(conn)
}

/// In-memory variant of [`open_node_db`], for tests and ephemeral nodes.
pub fn open_node_db_in_memory() -> Result<Connection, NodeError> {
    prepare_connection(Connection::open_in_memory()?)
}

fn prepare_connection(conn: Connection) -> Result<Connection, NodeError> {
    ensure_node_tables(&conn)?;
    resonator_sparql::register(&conn)?;
    Ok(conn)
}

/// Handle to the dedicated sqlite thread. Cheap to clone.
#[derive(Clone)]
pub struct DbHandle {
    tx: std_mpsc::Sender<Job>,
    interrupt: Arc<InterruptHandle>,
    /// Generation of the deadline-guarded job currently executing, or
    /// [`IDLE`].
    busy: Arc<AtomicU64>,
    next_gen: Arc<AtomicU64>,
}

impl DbHandle {
    /// Moves `conn` onto a fresh blocking thread and returns the handle.
    /// The thread exits when the last handle clone is dropped.
    pub fn spawn(conn: Connection) -> Self {
        let interrupt = conn.get_interrupt_handle();
        let (tx, rx) = std_mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("rsntr-db".into())
            .spawn(move || {
                let mut conn = conn;
                while let Ok(job) = rx.recv() {
                    job(&mut conn);
                }
                debug!("db thread exiting");
            })
            .expect("spawning the db thread cannot fail");
        Self {
            tx,
            interrupt: Arc::new(interrupt),
            busy: Arc::new(AtomicU64::new(IDLE)),
            next_gen: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Runs `f` on the db thread and awaits its result.
    pub async fn call<R, F>(&self, f: F) -> Result<R, NodeError>
    where
        R: Send + 'static,
        F: FnOnce(&mut Connection) -> R + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Box::new(move |conn| {
                let _ = tx.send(f(conn));
            }))
            .map_err(|_| NodeError::DbClosed)?;
        rx.await.map_err(|_| NodeError::DbClosed)
    }

    /// Allocates a fresh generation for a deadline-guarded job.
    pub fn new_generation(&self) -> u64 {
        // Skip the IDLE sentinel on wraparound (never in practice).
        let g = self.next_gen.fetch_add(1, Ordering::Relaxed);
        if g == IDLE {
            self.next_gen.fetch_add(1, Ordering::Relaxed)
        } else {
            g
        }
    }

    /// Like [`call`](Self::call), but marks the db thread busy with
    /// `generation` for the duration of the job, so
    /// [`interrupt_if_running`](Self::interrupt_if_running) can target it.
    pub async fn call_guarded<R, F>(&self, generation: u64, f: F) -> Result<R, NodeError>
    where
        R: Send + 'static,
        F: FnOnce(&mut Connection) -> R + Send + 'static,
    {
        let busy = self.busy.clone();
        self.call(move |conn| {
            busy.store(generation, Ordering::SeqCst);
            let out = f(conn);
            busy.store(IDLE, Ordering::SeqCst);
            out
        })
        .await
    }

    /// Calls `sqlite3_interrupt` iff the guarded job `generation` is still
    /// executing. Used by the wall-clock deadline timer.
    pub fn interrupt_if_running(&self, generation: u64) {
        if self.busy.load(Ordering::SeqCst) == generation {
            debug!(generation, "deadline elapsed; interrupting statement");
            self.interrupt.interrupt();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn call_round_trip_and_sparql_registered() {
        let conn = open_node_db_in_memory().unwrap();
        let db = DbHandle::spawn(conn);
        let n: i64 = db
            .call(|conn| conn.query_row("SELECT 41 + 1", [], |r| r.get(0)).unwrap())
            .await
            .unwrap();
        assert_eq!(n, 42);
        // rdf_* functions are installed at open.
        let ok: String = db
            .call(|conn| {
                conn.query_row("SELECT rdf_init()", [], |r| r.get(0))
                    .unwrap()
            })
            .await
            .unwrap();
        assert_eq!(ok, "ok");
    }

    #[tokio::test]
    async fn wal_mode_on_file_db() {
        let dir = std::env::temp_dir().join(format!("rsntr-node-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wal-test.db");
        let conn = open_node_db(&path).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
