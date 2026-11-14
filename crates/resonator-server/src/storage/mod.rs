//! SQLite storage: one database file in WAL mode.
//!
//! All writes go through one writer thread fed by a channel. The writer
//! batches whatever is queued into a single transaction (group commit) and
//! reports each job's result only after that transaction has committed.
//! Reads run on a small pool of threads with read-only connections. The
//! async functions here hand work to those threads and never block the
//! runtime.

mod account_store;
mod auth_store;
mod contact_store;
mod delta_store;
mod epoch_store;
mod federation_store;
mod migrations;
mod pending_store;
mod profile_store;
mod robot_store;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};

pub use account_store::AccountStore;
pub use auth_store::{AccessTokenRecord, AuthStore, SessionRecord};
pub use contact_store::{Contact, ContactStore};
pub use delta_store::{AppliedDelta, DeltaStore, DeltasAccess, StoredSnapshot, WaveletDeltaRecord};
pub use epoch_store::{OrphanDelta, Rewind, StoredEpoch};
pub use federation_store::{
    AppliedRequest, CoHostRecord, DeviceCertRecord, FederationStore, OutboxEntry, OutboxStatus,
    PairInvitationRecord, PeerRecord, PeerStatus, PushTargetRecord, RevocationRecord,
    SignerInfoRecord,
};
pub use migrations::SCHEMA_VERSION;
pub use pending_store::{PendingStore, PendingSubmit};
pub use profile_store::{ProfileRecord, ProfileStore, StoredProfile};
pub use robot_store::{AgentTrigger, AgentUsage, RobotRegistration, RobotStore, TriggerState};

/// Failure of the storage layer (`PersistenceException` in the original).
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("storage i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// The database was closed, or its worker threads are gone.
    #[error("database is closed")]
    Closed,
    /// The group commit a write was part of failed; the write is not stored.
    #[error("commit failed: {0}")]
    Commit(String),
    #[error("database schema version {found} is newer than the supported version {supported}")]
    SchemaTooNew { found: u32, supported: u32 },
    /// A stored row cannot be decoded or contradicts its neighbours.
    #[error("corrupt stored data for wavelet {wavelet}: {message}")]
    Corrupt { wavelet: String, message: String },
    /// A stored account row cannot be decoded.
    #[error("corrupt stored account {address}: {message}")]
    CorruptAccount { address: String, message: String },
    /// `FileNotFoundPersistenceException` in the original.
    #[error("wavelet {0} does not exist")]
    WaveletNotFound(String),
    /// The request does not fit the stored state, e.g. appended deltas that
    /// do not continue the history.
    #[error("{0}")]
    Invalid(String),
}

/// SQLite's `synchronous` setting. In WAL mode `Normal` syncs only at
/// checkpoints (a power failure can lose the last commits but not corrupt
/// the database); `Full` syncs the WAL on every commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Synchronous {
    Off,
    Normal,
    #[default]
    Full,
    Extra,
}

impl Synchronous {
    pub fn as_str(self) -> &'static str {
        match self {
            Synchronous::Off => "OFF",
            Synchronous::Normal => "NORMAL",
            Synchronous::Full => "FULL",
            Synchronous::Extra => "EXTRA",
        }
    }
}

impl std::str::FromStr for Synchronous {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "off" => Ok(Synchronous::Off),
            "normal" => Ok(Synchronous::Normal),
            "full" => Ok(Synchronous::Full),
            "extra" => Ok(Synchronous::Extra),
            _ => Err(format!(
                "invalid synchronous mode '{s}' (expected off, normal, full or extra)"
            )),
        }
    }
}

impl std::fmt::Display for Synchronous {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How to open a [`Database`].
#[derive(Debug, Clone)]
pub struct DatabaseOptions {
    pub synchronous: Synchronous,
    /// Number of read-only connections, each on its own thread.
    pub read_connections: usize,
    /// Writes that may be queued or running before further writers wait.
    pub write_queue_depth: usize,
    /// Most write jobs committed in one transaction.
    pub max_batch: usize,
}

impl Default for DatabaseOptions {
    fn default() -> Self {
        DatabaseOptions {
            synchronous: Synchronous::Full,
            read_connections: 4,
            write_queue_depth: 8192,
            max_batch: 1024,
        }
    }
}

const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

/// A job for the writer thread. `run` executes inside the batch
/// transaction; `finish` is called once the batch has committed or failed.
trait WriteJob: Send {
    fn run(&mut self, conn: &Connection) -> bool;
    fn finish(self: Box<Self>, commit: Result<(), String>);
}

struct TypedWriteJob<T, F> {
    work: Option<F>,
    result: Option<Result<T, StorageError>>,
    reply: oneshot::Sender<Result<T, StorageError>>,
    /// Held until the job is finished, for back-pressure.
    _permit: OwnedSemaphorePermit,
}

impl<T, F> WriteJob for TypedWriteJob<T, F>
where
    T: Send,
    F: FnOnce(&Connection) -> Result<T, StorageError> + Send,
{
    fn run(&mut self, conn: &Connection) -> bool {
        let Some(work) = self.work.take() else {
            return false;
        };
        let result = work(conn);
        let ok = result.is_ok();
        self.result = Some(result);
        ok
    }

    fn finish(self: Box<Self>, commit: Result<(), String>) {
        let result = match (self.result, commit) {
            (Some(Err(error)), _) => Err(error),
            (_, Err(message)) => Err(StorageError::Commit(message)),
            (Some(Ok(value)), Ok(())) => Ok(value),
            (None, Ok(())) => Err(StorageError::Closed),
        };
        // The requester may have stopped waiting.
        let _ = self.reply.send(result);
    }
}

type ReadJob = Box<dyn FnOnce(&Connection) + Send>;

/// The result of a queued write, available once its transaction commits.
pub(crate) struct WriteHandle<T> {
    reply: oneshot::Receiver<Result<T, StorageError>>,
}

impl<T> WriteHandle<T> {
    /// Waits until the write is durably committed (as durable as the
    /// `synchronous` setting makes a commit) or has failed.
    pub(crate) async fn wait(self) -> Result<T, StorageError> {
        self.reply.await.unwrap_or(Err(StorageError::Closed))
    }
}

struct Inner {
    path: PathBuf,
    options: DatabaseOptions,
    write_tx: Mutex<Option<Sender<Box<dyn WriteJob>>>>,
    read_tx: Mutex<Option<Sender<ReadJob>>>,
    write_permits: Arc<Semaphore>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

/// Handle to the open database; cheap to clone.
#[derive(Clone)]
pub struct Database {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database")
            .field("path", &self.inner.path)
            .finish()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn open_reader(path: &Path) -> Result<Connection, StorageError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "query_only", true)?;
    conn.set_prepared_statement_cache_capacity(64);
    Ok(conn)
}

impl Database {
    /// Opens (creating if absent) and migrates the database, then starts
    /// the writer and reader threads. Blocks for the duration of the
    /// migration; call it at startup.
    pub fn open(path: impl AsRef<Path>, options: DatabaseOptions) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();
        let mut writer = Connection::open(&path)?;
        writer.busy_timeout(BUSY_TIMEOUT)?;
        let mode: String = writer.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StorageError::Invalid(format!(
                "could not switch {} to WAL mode (journal mode is {mode})",
                path.display()
            )));
        }
        writer.pragma_update(None, "synchronous", options.synchronous.as_str())?;
        writer.pragma_update(None, "foreign_keys", true)?;
        writer.pragma_update(None, "temp_store", "MEMORY")?;
        // Bound the WAL file left behind after a checkpoint.
        let _: i64 = writer.query_row("PRAGMA journal_size_limit=67108864", [], |r| r.get(0))?;
        writer.set_prepared_statement_cache_capacity(64);
        migrations::migrate(&mut writer)?;

        let mut threads = Vec::new();
        let (write_tx, write_rx) = channel::<Box<dyn WriteJob>>();
        let max_batch = options.max_batch.max(1);
        threads.push(
            std::thread::Builder::new()
                .name("resonator-db-writer".to_owned())
                .spawn(move || writer_loop(writer, write_rx, max_batch))?,
        );

        let (read_tx, read_rx) = channel::<ReadJob>();
        let read_rx = Arc::new(Mutex::new(read_rx));
        for index in 0..options.read_connections.max(1) {
            let conn = open_reader(&path)?;
            let read_rx = Arc::clone(&read_rx);
            threads.push(
                std::thread::Builder::new()
                    .name(format!("resonator-db-reader-{index}"))
                    .spawn(move || reader_loop(conn, read_rx))?,
            );
        }

        Ok(Database {
            inner: Arc::new(Inner {
                write_permits: Arc::new(Semaphore::new(options.write_queue_depth.max(1))),
                path,
                options,
                write_tx: Mutex::new(Some(write_tx)),
                read_tx: Mutex::new(Some(read_tx)),
                threads: Mutex::new(threads),
            }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn options(&self) -> &DatabaseOptions {
        &self.inner.options
    }

    pub fn delta_store(&self) -> DeltaStore {
        DeltaStore::new(self.clone())
    }

    pub fn account_store(&self) -> AccountStore {
        AccountStore::new(self.clone())
    }

    pub fn auth_store(&self) -> AuthStore {
        AuthStore::new(self.clone())
    }

    /// The registrations of robots and the bookkeeping of agents.
    pub fn robot_store(&self) -> RobotStore {
        RobotStore::new(self.clone())
    }

    /// The federation tables: peer keys and signer info.
    pub fn federation_store(&self) -> FederationStore {
        FederationStore::new(self.clone())
    }

    pub fn pending_store(&self) -> PendingStore {
        PendingStore::new(self.clone())
    }

    /// The contacts added by hand (device mode).
    pub fn contact_store(&self) -> ContactStore {
        ContactStore::new(self.clone())
    }

    /// Names and avatars: of this domain's accounts, and fetched copies.
    pub fn profile_store(&self) -> ProfileStore {
        ProfileStore::new(self.clone())
    }

    /// Number of writes queued or running; at most
    /// [`DatabaseOptions::write_queue_depth`].
    pub fn write_queue_depth(&self) -> usize {
        self.inner
            .options
            .write_queue_depth
            .max(1)
            .saturating_sub(self.inner.write_permits.available_permits())
    }

    /// Runs `work` on a read-only connection.
    pub(crate) async fn read<T, F>(&self, work: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, StorageError> + Send + 'static,
    {
        let (reply, result) = oneshot::channel();
        let job: ReadJob = Box::new(move |conn| {
            let _ = reply.send(work(conn));
        });
        {
            let guard = lock(&self.inner.read_tx);
            let sender = guard.as_ref().ok_or(StorageError::Closed)?;
            sender.send(job).map_err(|_| StorageError::Closed)?;
        }
        result.await.unwrap_or(Err(StorageError::Closed))
    }

    /// Queues `work` for the writer. Waits only while the write queue is
    /// full; the returned handle resolves once the write has committed.
    /// Jobs run in the order they were queued. A job that fails is rolled
    /// back on its own and does not affect the rest of its batch.
    pub(crate) async fn enqueue_write<T, F>(&self, work: F) -> Result<WriteHandle<T>, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, StorageError> + Send + 'static,
    {
        let permit = Arc::clone(&self.inner.write_permits)
            .acquire_owned()
            .await
            .map_err(|_| StorageError::Closed)?;
        let (reply, result) = oneshot::channel();
        let job = Box::new(TypedWriteJob {
            work: Some(work),
            result: None,
            reply,
            _permit: permit,
        });
        let guard = lock(&self.inner.write_tx);
        let sender = guard.as_ref().ok_or(StorageError::Closed)?;
        sender.send(job).map_err(|_| StorageError::Closed)?;
        Ok(WriteHandle { reply: result })
    }

    /// Runs `work` on the writer and waits for its commit.
    pub(crate) async fn write<T, F>(&self, work: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, StorageError> + Send + 'static,
    {
        self.enqueue_write(work).await?.wait().await
    }

    /// Waits until every write queued before this call has committed.
    pub async fn flush(&self) -> Result<(), StorageError> {
        self.write(|_| Ok(())).await
    }

    /// Reads a value of the key/value meta table.
    pub async fn meta(&self, key: &str) -> Result<Option<String>, StorageError> {
        let key = key.to_owned();
        self.read(move |conn| {
            Ok(conn
                .prepare_cached("SELECT value FROM meta WHERE key = ?1")?
                .query_row([&key], |row| row.get(0))
                .optional()?)
        })
        .await
    }

    /// Sets a value of the key/value meta table.
    pub async fn set_meta(&self, key: &str, value: &str) -> Result<(), StorageError> {
        let (key, value) = (key.to_owned(), value.to_owned());
        self.write(move |conn| {
            conn.prepare_cached(
                "INSERT INTO meta (key, value) VALUES (?1, ?2) \
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            )?
            .execute([&key, &value])?;
            Ok(())
        })
        .await
    }

    /// The schema version of the open database.
    pub async fn schema_version(&self) -> Result<u32, StorageError> {
        self.read(migrations::schema_version).await
    }

    /// Writes a consistent copy of the database to `destination` while the
    /// database stays in use. `destination` must not exist.
    pub async fn backup_to(&self, destination: impl AsRef<Path>) -> Result<(), StorageError> {
        let source = self.inner.path.clone();
        let destination = destination.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || backup(&source, &destination))
            .await
            .map_err(|_| StorageError::Closed)?
    }

    /// Moves what it can of the write-ahead log into the database file
    /// without waiting for readers or writers (a passive checkpoint), on a
    /// connection of its own. SQLite does this by itself as the log grows;
    /// calling it now and then keeps the log short when long reads get in
    /// the way. Returns the log's size in frames and how many of them are
    /// now in the database file.
    pub async fn checkpoint(&self) -> Result<(u64, u64), StorageError> {
        let path = self.inner.path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = Connection::open_with_flags(
                &path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            conn.busy_timeout(BUSY_TIMEOUT)?;
            let (log, checkpointed): (i64, i64) =
                conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                    Ok((row.get(1)?, row.get(2)?))
                })?;
            Ok((log.max(0) as u64, checkpointed.max(0) as u64))
        })
        .await
        .map_err(|_| StorageError::Closed)?
    }

    /// Stops accepting work, lets the writer finish everything already
    /// queued, checkpoints the WAL and joins the worker threads. Work
    /// queued by other handles after this fails with
    /// [`StorageError::Closed`].
    pub async fn close(&self) {
        lock(&self.inner.write_tx).take();
        lock(&self.inner.read_tx).take();
        self.inner.write_permits.close();
        let threads = std::mem::take(&mut *lock(&self.inner.threads));
        let joined = tokio::task::spawn_blocking(move || {
            for thread in threads {
                let _ = thread.join();
            }
        });
        let _ = joined.await;
    }
}

/// Writes a consistent copy of the database at `source` to `destination`
/// with `VACUUM INTO`, which reads from a single snapshot and so is safe
/// while another process or thread is writing. Blocking.
pub fn backup(source: &Path, destination: &Path) -> Result<(), StorageError> {
    if destination.exists() {
        return Err(StorageError::Invalid(format!(
            "backup destination {} already exists",
            destination.display()
        )));
    }
    let destination = destination
        .to_str()
        .ok_or_else(|| StorageError::Invalid("backup destination is not valid UTF-8".to_owned()))?;
    let conn = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.execute("VACUUM INTO ?1", [destination])?;
    Ok(())
}

fn writer_loop(conn: Connection, jobs: Receiver<Box<dyn WriteJob>>, max_batch: usize) {
    while let Ok(first) = jobs.recv() {
        let mut batch = vec![first];
        while batch.len() < max_batch {
            match jobs.try_recv() {
                Ok(job) => batch.push(job),
                Err(_) => break,
            }
        }
        let commit = run_batch(&conn, &mut batch).map_err(|error| {
            tracing::error!(%error, jobs = batch.len(), "group commit failed");
            error.to_string()
        });
        for job in batch {
            job.finish(commit.clone());
        }
    }
    // All senders are gone: final checkpoint so the WAL is folded in.
    if let Err(error) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA optimize;") {
        tracing::warn!(%error, "final checkpoint failed");
    }
}

fn run_batch(conn: &Connection, batch: &mut [Box<dyn WriteJob>]) -> Result<(), rusqlite::Error> {
    conn.prepare_cached("BEGIN IMMEDIATE")?.execute([])?;
    let mut run = || -> Result<(), rusqlite::Error> {
        for job in batch.iter_mut() {
            conn.prepare_cached("SAVEPOINT job")?.execute([])?;
            if !job.run(conn) {
                conn.prepare_cached("ROLLBACK TO job")?.execute([])?;
            }
            conn.prepare_cached("RELEASE job")?.execute([])?;
        }
        conn.prepare_cached("COMMIT")?.execute([])?;
        Ok(())
    };
    let result = run();
    if result.is_err() && !conn.is_autocommit() {
        let _ = conn.execute_batch("ROLLBACK");
    }
    result
}

fn reader_loop(conn: Connection, jobs: Arc<Mutex<Receiver<ReadJob>>>) {
    loop {
        // The lock is held while waiting, so idle readers queue up on it
        // and each job goes to exactly one of them.
        let job = lock(&jobs).recv();
        match job {
            Ok(job) => job(&conn),
            Err(_) => break,
        }
    }
}
