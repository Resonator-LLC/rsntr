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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, InterruptHandle, TransactionBehavior};
use tokio::sync::oneshot;
use tracing::debug;

use crate::ddl::{
    check_schema_compat, ensure_node_tables, schema_is_current, stamp_schema_version,
};
use crate::error::NodeError;

type Job = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

/// Sentinel for "no job with a deadline is executing".
const IDLE: u64 = u64::MAX;

/// How long a connection waits for the write lock before a statement
/// fails busy. Set before anything else on every connection this crate
/// opens: several processes share one database, and the migrations at
/// open are writes like any other.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Opens (or creates) a node database file: busy timeout, WAL mode,
/// `synchronous = NORMAL` (crash-consistent in WAL; the last commits may
/// be lost on power loss, never the database), a bounded WAL, the
/// reserved tables ensured (once, see [`migrate`]), and the `rdf_*` SQL
/// functions plus the `sparql()` vtab registered. This is the connection
/// a [`DbHandle`] should own.
pub fn open_node_db(path: &Path) -> Result<Connection, NodeError> {
    // The journal-mode switch on a brand-new file and the schema
    // pre-checks before WAL is on are the two places sqlite may answer
    // busy without consulting the busy handler (several apps creating
    // one node at once); a bounded retry of the whole open covers both.
    let mut attempt: u32 = 0;
    loop {
        match open_node_db_once(path) {
            Ok(conn) => return Ok(conn),
            Err(NodeError::Db(e)) if is_busy(&e) && attempt < 40 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(25 * u64::from(attempt.min(8))));
            }
            Err(NodeError::Busy(_)) if attempt < 2 => attempt += 1,
            Err(e) => return Err(e),
        }
    }
}

fn open_node_db_once(path: &Path) -> Result<Connection, NodeError> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "journal_size_limit", 64 * 1024 * 1024)?;
    prepare_connection(conn)
}

/// In-memory variant of [`open_node_db`], for tests and ephemeral nodes.
pub fn open_node_db_in_memory() -> Result<Connection, NodeError> {
    prepare_connection(Connection::open_in_memory()?)
}

fn prepare_connection(mut conn: Connection) -> Result<Connection, NodeError> {
    migrate(&mut conn)?;
    resonator_sparql::register(&conn)?;
    Ok(conn)
}

/// True for the busy/locked family of sqlite failures.
pub fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::DatabaseBusy
                || f.code == rusqlite::ErrorCode::DatabaseLocked
    )
}

/// Brings the schema up to this build's, atomically and at most one
/// process at a time: refuses a database too new to read, opens with no
/// write at all when the schema is already current, and otherwise runs
/// [`ensure_node_tables`] inside one `BEGIN IMMEDIATE` (the write lock
/// is taken up front, so two apps upgrading together serialize instead
/// of failing on a snapshot conflict). A lock held past
/// [`BUSY_TIMEOUT`] surfaces as [`NodeError::Busy`].
pub fn migrate(conn: &mut Connection) -> Result<(), NodeError> {
    check_schema_compat(conn)?;
    if schema_is_current(conn) {
        return Ok(());
    }
    let tx = match conn.transaction_with_behavior(TransactionBehavior::Immediate) {
        Ok(tx) => tx,
        Err(e) if is_busy(&e) => return Err(NodeError::Busy(BUSY_TIMEOUT)),
        Err(e) => return Err(e.into()),
    };
    // Re-check under the lock: another process may have migrated while
    // this one waited.
    check_schema_compat(&tx)?;
    if !schema_is_current(&tx) {
        ensure_node_tables(&tx)?;
        stamp_schema_version(&tx)?;
    }
    tx.commit()?;
    Ok(())
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
    /// Set by [`close`](Self::close); the db thread drops the connection
    /// after the job that set it.
    closing: Arc<AtomicBool>,
    thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
}

impl DbHandle {
    /// Moves `conn` onto a fresh blocking thread and returns the handle.
    /// The thread exits when the last handle clone is dropped or when
    /// [`close`](Self::close) runs.
    pub fn spawn(conn: Connection) -> Self {
        let interrupt = conn.get_interrupt_handle();
        let (tx, rx) = std_mpsc::channel::<Job>();
        let closing = Arc::new(AtomicBool::new(false));
        let closing_flag = closing.clone();
        let thread = std::thread::Builder::new()
            .name("rsntr-db".into())
            .spawn(move || {
                let mut conn = conn;
                while let Ok(job) = rx.recv() {
                    job(&mut conn);
                    if closing_flag.load(Ordering::SeqCst) {
                        break;
                    }
                }
                // Dropping the receiver first makes every later `call`
                // fail fast with DbClosed instead of queueing forever.
                drop(rx);
                drop(conn);
                debug!("db thread exiting");
            })
            .expect("spawning the db thread cannot fail");
        Self {
            tx,
            interrupt: Arc::new(interrupt),
            busy: Arc::new(AtomicU64::new(IDLE)),
            next_gen: Arc::new(AtomicU64::new(0)),
            closing,
            thread: Arc::new(Mutex::new(Some(thread))),
        }
    }

    /// Closes the connection: runs after every job queued so far, takes
    /// a passive WAL checkpoint, and waits for the db thread to drop the
    /// connection (and with it every sqlite lock it held). Every handle
    /// clone answers [`NodeError::DbClosed`] afterwards. Idempotent.
    pub async fn close(&self) -> Result<(), NodeError> {
        if !self.closing.load(Ordering::SeqCst) {
            let closing = self.closing.clone();
            let (done_tx, done_rx) = oneshot::channel();
            let sent = self.tx.send(Box::new(move |conn| {
                let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE)");
                closing.store(true, Ordering::SeqCst);
                let _ = done_tx.send(());
            }));
            if sent.is_ok() {
                let _ = done_rx.await;
            }
        }
        let handle = self.thread.lock().expect("db thread lock").take();
        if let Some(handle) = handle {
            let _ = tokio::task::spawn_blocking(move || handle.join()).await;
        }
        Ok(())
    }

    /// True once [`close`](Self::close) ran (or the thread is gone).
    pub fn is_closed(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
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

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rsntr-node-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn schema_too_new_is_refused_and_current_open_writes_nothing() {
        let dir = scratch("schema");
        let path = dir.join("n.db");
        let first = open_node_db(&path).expect("fresh open migrates");
        assert!(crate::ddl::schema_is_current(&first));
        // A second connection's data_version only moves when OTHER
        // connections commit: a current-schema open must not move it.
        let probe = Connection::open(&path).unwrap();
        let before: i64 = probe
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        let again = open_node_db(&path).expect("reopen");
        let after: i64 = probe
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, after, "a current schema is opened without writes");
        drop(again);
        // A newer build's database refuses this one.
        crate::ddl::set_rsntr(&first, crate::ddl::MIN_READER_VERSION_KEY, "999").unwrap();
        match open_node_db(&path) {
            Err(NodeError::SchemaTooNew {
                required,
                supported,
            }) => {
                assert_eq!((required, supported), (999, crate::ddl::SCHEMA_VERSION));
            }
            other => panic!("expected SchemaTooNew, got {:?}", other.map(|_| ())),
        }
        drop(first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_waits_for_a_busy_migrator_and_concurrent_fresh_opens_succeed() {
        let dir = scratch("busy");
        let path = dir.join("n.db");
        // Four threads race to create and migrate one fresh database.
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let p = path.clone();
                std::thread::spawn(move || open_node_db(&p).map(|_| ()))
            })
            .collect();
        for h in handles {
            h.join().unwrap().expect("concurrent fresh open");
        }
        // Force a migration, then hold the write lock for 300 ms on a
        // side connection while another open waits for it.
        {
            let side = Connection::open(&path).unwrap();
            side.execute("DELETE FROM _rsntr WHERE key = 'schema_version'", [])
                .unwrap();
        }
        let holder = {
            let p = path.clone();
            std::thread::spawn(move || {
                let mut c = Connection::open(&p).unwrap();
                let tx = c
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                std::thread::sleep(Duration::from_millis(300));
                tx.commit().unwrap();
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        let started = std::time::Instant::now();
        let conn = open_node_db(&path).expect("open waits for the lock");
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert!(crate::ddl::schema_is_current(&conn));
        holder.join().unwrap();
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn close_drops_the_connection_and_later_calls_fail_fast() {
        let db = DbHandle::spawn(open_node_db_in_memory().unwrap());
        let n: i64 = db
            .call(|c| c.query_row("SELECT 1", [], |r| r.get(0)).unwrap())
            .await
            .unwrap();
        assert_eq!(n, 1);
        db.close().await.unwrap();
        assert!(db.is_closed());
        assert!(matches!(db.call(|_c| ()).await, Err(NodeError::DbClosed)));
        db.close().await.unwrap();
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
