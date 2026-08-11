//! The outbox surface: sending over the network expressed as two plain
//! tables and a background worker.
//!
//! Applications never touch the transport. They INSERT a row into
//! `_outbox` and later read `_results`; the [`OutboxWorker`] does the
//! rest.
//!
//! - [`ensure_outbox_tables`] creates `_outbox` and `_results` with
//!   `CREATE TABLE IF NOT EXISTS` (re-running is harmless), installs a
//!   trigger that backfills a `request_id` on rows inserted without one,
//!   and notes the surface schema version in `_rsntr` when that table is
//!   present.
//! - [`enqueue`] is the Rust-side insert: it mints a real ULID
//!   `request_id` and adds the row. Plain SQL INSERTs also work; the
//!   trigger gives them a random id which the worker folds into a stable
//!   wire ULID (below).
//! - [`OutboxWorker`] scans for `status = 'queued'` rows (plus, after a
//!   restart, `sent` rows whose answer never arrived), ships each one
//!   over a [`Transport`] as a `rsntr:Query`/`rsntr:Execute` under the
//!   row's `mod`, writes the streamed response into `_results` as JSON,
//!   and walks the status through
//!   `queued | sent | done | denied | error | expired` with exponential
//!   backoff and an expiry. A rusqlite `update_hook` on `_outbox` wakes
//!   it; a `PRAGMA data_version` poll backstops writes arriving on other
//!   connections.
//!
//! Crash-safety is idempotency: the serving node remembers applied
//! writes by request id in `_applied`, so re-shipping a `sent` row after
//! a restart cannot apply twice. The wire id is derived from the stored
//! `request_id` deterministically and therefore identical on every
//! resend.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rusqlite::hooks::Action;
use rusqlite::{Connection, OptionalExtension};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::{debug, warn};
use ulid::Ulid;

use resonator_node::{DbHandle, NodeError};
use resonator_protocol::{
    Request, RequestKind, RequestOptions, ResponseEvent, ResponseReader, Row, Value,
};
use resonator_transport::{PeerId, RequestStream, Transport};

/// Surface schema version written to `_rsntr` under
/// `outbox_schema_version`; bump on any `_outbox`/`_results` shape
/// change.
pub const OUTBOX_SCHEMA_VERSION: i64 = 1;

/// `_outbox`: one queued/sent/finished request per row.
pub const OUTBOX_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _outbox (
  request_id   TEXT PRIMARY KEY,           -- ULID; trigger backfills if absent
  peer         TEXT NOT NULL,              -- _peers.endpoint_id or petname
  mod          TEXT NOT NULL DEFAULT 'sql-sqlite',
  signal       TEXT NOT NULL,
  params       TEXT NOT NULL DEFAULT '[]', -- JSON array
  status       TEXT NOT NULL DEFAULT 'queued',
               -- queued | sent | done | denied | error | expired
  error        TEXT,
  created_at   TEXT NOT NULL DEFAULT (datetime('now')),
  completed_at TEXT
);
";

/// `_results`: JSON result rows keyed by request.
pub const RESULTS_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _results (
  request_id   TEXT NOT NULL,
  row_no       INTEGER NOT NULL,
  row          TEXT NOT NULL,              -- JSON (array of values, or graph)
  PRIMARY KEY (request_id, row_no)
);
";

/// Backfills a `request_id` on rows inserted without one. sqlite cannot
/// mint ULIDs, so the trigger writes a random 32-hex token instead; the
/// worker folds any non-ULID token into a stable 128-bit wire id (see
/// [`wire_request_id`]). [`enqueue`] rows carry a real ULID from the
/// start.
pub const OUTBOX_ID_TRIGGER_DDL: &str = "\
CREATE TRIGGER IF NOT EXISTS _outbox_mint_request_id
AFTER INSERT ON _outbox
WHEN NEW.request_id IS NULL OR NEW.request_id = ''
BEGIN
  UPDATE _outbox SET request_id = lower(hex(randomblob(16)))
  WHERE rowid = NEW.rowid;
END;
";

/// Setup failures only. A failing request is written to `_outbox.error`
/// and the worker moves on; it never surfaces through this type.
#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    /// The dedicated sqlite thread is gone.
    #[error("database thread is not running")]
    DbClosed,
    /// A database error while creating the surface tables.
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
}

impl From<NodeError> for OutboxError {
    fn from(e: NodeError) -> Self {
        match e {
            NodeError::DbClosed => OutboxError::DbClosed,
            NodeError::Db(db) => OutboxError::Db(db),
            // Setup sends no frames, so any other node error means the
            // thread is effectively lost.
            _ => OutboxError::DbClosed,
        }
    }
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool, rusqlite::Error> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |row| row.get(0),
    )?;
    Ok(n > 0)
}

/// Creates `_outbox`, `_results`, and the id trigger when missing, and
/// records the surface schema version in `_rsntr` if that table exists.
/// Idempotent; called on every worker start.
pub fn ensure_outbox_tables(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(OUTBOX_DDL)?;
    conn.execute_batch(RESULTS_DDL)?;
    conn.execute_batch(OUTBOX_ID_TRIGGER_DDL)?;
    if table_exists(conn, "_rsntr")? {
        conn.execute(
            "INSERT INTO _rsntr (key, value) VALUES ('outbox_schema_version', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [OUTBOX_SCHEMA_VERSION.to_string()],
        )?;
    }
    Ok(())
}

/// Inserts one request with a freshly minted ULID `request_id`, which is
/// returned. `params` must be a JSON array (`"[]"` for none).
pub fn enqueue(
    conn: &Connection,
    peer: &str,
    modulation: &str,
    signal: &str,
    params: &str,
) -> Result<String, rusqlite::Error> {
    let id = Ulid::new().to_string();
    conn.execute(
        "INSERT INTO _outbox (request_id, peer, mod, signal, params) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![id, peer, modulation, signal, params],
    )?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// Wire id, kind inference, JSON mapping
// ---------------------------------------------------------------------------

/// The stable 128-bit wire request id for a stored `_outbox` id.
///
/// A well-formed ULID string maps to its own bytes, so caller-chosen ids
/// appear unchanged on the wire. Anything else (the trigger's hex, say)
/// is folded deterministically: identical input, identical wire id, on
/// every resend, which is what keeps the server's `_applied` idempotency
/// honest.
fn wire_request_id(stored: &str) -> [u8; 16] {
    if let Ok(u) = Ulid::from_string(stored) {
        return u.to_bytes();
    }
    // Two FNV-1a-style folds into the high and low halves.
    let mut h1: u64 = 0xcbf2_9ce4_8422_2325;
    let mut h2: u64 = 0x8422_2325_cbf2_9ce4;
    for &b in stored.as_bytes() {
        h1 = (h1 ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
        h2 = (h2.wrapping_add(b as u64).rotate_left(7)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&h1.to_be_bytes());
    out[8..].copy_from_slice(&h2.to_be_bytes());
    out
}

/// Labels the wire object read or write from the modulation and the
/// signal's leading keyword. Best effort only; the serving node is the
/// authority on read-vs-write.
fn infer_kind(modulation: &str, signal: &str) -> RequestKind {
    match modulation.split('/').next().unwrap_or(modulation) {
        "sql-sqlite" => {
            let first: String = signal
                .trim_start()
                .split(|c: char| c.is_whitespace() || c == '(' || c == ';')
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            match first.as_str() {
                "SELECT" | "WITH" | "PRAGMA" | "EXPLAIN" | "VALUES" => RequestKind::Query,
                _ => RequestKind::Execute,
            }
        }
        "sparql" => {
            // Walk past the prologue (PREFIX/BASE and their IRIs) until
            // the query form keyword shows up.
            for word in signal.split_whitespace() {
                let w = word.to_ascii_uppercase();
                match w.as_str() {
                    "SELECT" | "ASK" | "CONSTRUCT" | "DESCRIBE" => return RequestKind::Query,
                    "PREFIX" | "BASE" => continue,
                    _ if w.starts_with("PREFIX") || w.starts_with("BASE") => continue,
                    _ if w.contains(':') || w.starts_with('<') => continue,
                    _ => return RequestKind::Execute,
                }
            }
            RequestKind::Execute
        }
        // A chat send is always a write (chat protocol sec 3).
        "chat" => RequestKind::Execute,
        // help, projection, media, mods, ...: reads.
        _ => RequestKind::Query,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One JSON parameter as an engine [`Value`]. JSON knows no blobs;
/// booleans become sqlite's 0/1 integers.
fn json_to_value(j: &serde_json::Value) -> Result<Value, String> {
    use serde_json::Value as J;
    Ok(match j {
        J::Null => Value::Null,
        J::Bool(b) => Value::Integer(*b as i64),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Value::Real(f)
            } else {
                return Err(format!("parameter number {n} is out of range"));
            }
        }
        J::String(s) => Value::Text(s.clone()),
        J::Array(_) | J::Object(_) => {
            return Err("array/object parameters are not supported".to_string());
        }
    })
}

/// One engine [`Value`] as JSON for `_results`. Blobs go out hex-encoded
/// (the JSON fidelity loss is accepted in v1). Shared with the vtab
/// surface (`remote_query`'s `cells` column).
pub(crate) fn value_to_json(v: &Value) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        Value::Null => J::Null,
        Value::Integer(i) => J::from(*i),
        Value::Real(f) => serde_json::Number::from_f64(*f)
            .map(J::Number)
            .unwrap_or(J::Null),
        Value::Text(s) => J::from(s.clone()),
        Value::Blob(b) => J::from(hex(b)),
        Value::BlobRef { hash, .. } => J::from(hash.clone()),
    }
}

/// One result row as a JSON array aligned to the header columns. Cells
/// the wire omitted (NULLs) come back as JSON `null`. Shared with the
/// vtab surface (`remote_query`'s `row` column).
pub(crate) fn row_to_json(columns: &[String], row: &Row) -> String {
    let by_name: HashMap<&str, &Value> = row.cells.iter().map(|(n, v)| (n.as_str(), v)).collect();
    let arr: Vec<serde_json::Value> = columns
        .iter()
        .map(|c| {
            by_name
                .get(c.as_str())
                .map(|v| value_to_json(v))
                .unwrap_or(serde_json::Value::Null)
        })
        .collect();
    serde_json::Value::Array(arr).to_string()
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

/// Tunables for the [`OutboxWorker`].
#[derive(Clone, Debug)]
pub struct OutboxConfig {
    /// Longest gap between scans when nothing wakes the worker (the
    /// `PRAGMA data_version` fallback cadence).
    pub poll_interval: Duration,
    /// Delay before the first retry of a failed attempt.
    pub base_backoff: Duration,
    /// Ceiling on the exponential backoff.
    pub max_backoff: Duration,
    /// Rows older than this without completion flip to `expired`.
    pub expiry: Duration,
    /// Per-attempt bound on dialing/opening a stream, so one dead peer
    /// cannot stall the whole queue.
    pub open_timeout: Duration,
    /// Whether to install the `_outbox` `update_hook`. sqlite permits
    /// one update hook per connection, and the node pipeline claims it
    /// (for vibrations) when both share a [`DbHandle`]; set false then,
    /// and either live with the `poll_interval` fallback or call
    /// [`OutboxHandle::wake`] from a composite hook.
    pub install_update_hook: bool,
}

impl Default for OutboxConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(2),
            base_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(60),
            expiry: Duration::from_secs(3600),
            open_timeout: Duration::from_secs(10),
            install_update_hook: true,
        }
    }
}

/// One actionable `_outbox` row as the scan sees it.
struct PendingRow {
    id: String,
    peer: String,
    modulation: String,
    signal: String,
    params: String,
    age_secs: i64,
}

/// In-memory retry bookkeeping per row. Deliberately not persisted:
/// after a process restart the backoff simply starts over, which is
/// safe.
struct Backoff {
    attempts: u32,
    next_attempt: Instant,
}

/// What became of one processed row.
enum Step {
    /// Reached `done`/`denied`/`error`/`expired`; forget its backoff.
    Settled,
    /// Transient trouble (peer down, transport hiccup); back off and
    /// try again.
    Again,
}

/// Handle to a running worker: wake it or stop it.
pub struct OutboxHandle {
    wake: Arc<Notify>,
    shutdown: Arc<AtomicBool>,
    shutdown_notify: Arc<Notify>,
    task: JoinHandle<()>,
}

/// A cloneable waker split off the [`OutboxHandle`], for wiring into an
/// externally owned `update_hook` when
/// [`OutboxConfig::install_update_hook`] is false (one hook per
/// connection).
#[derive(Clone)]
pub struct OutboxWaker {
    wake: Arc<Notify>,
}

impl OutboxWaker {
    /// Asks the worker to scan now.
    pub fn wake(&self) {
        self.wake.notify_one();
    }
}

impl OutboxHandle {
    /// Asks the worker to scan now (the `_outbox` `update_hook` does the
    /// same automatically).
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// A cloneable waker independent of this handle's lifetime.
    pub fn waker(&self) -> OutboxWaker {
        OutboxWaker {
            wake: self.wake.clone(),
        }
    }

    /// Tells the worker to stop and waits for it.
    pub async fn shutdown(self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.shutdown_notify.notify_one();
        let _ = self.task.await;
    }
}

/// The send-side worker: moves `_outbox` rows over a [`Transport`].
pub struct OutboxWorker<T: Transport> {
    db: DbHandle,
    transport: Arc<T>,
    config: OutboxConfig,
    wake: Arc<Notify>,
    shutdown: Arc<AtomicBool>,
    shutdown_notify: Arc<Notify>,
}

impl<T: Transport> OutboxWorker<T> {
    /// Ensures the tables, installs the `_outbox` `update_hook` (when
    /// configured), and spawns the loop on the current tokio runtime.
    /// The returned [`OutboxHandle`] controls it.
    pub async fn spawn(
        db: DbHandle,
        transport: Arc<T>,
        config: OutboxConfig,
    ) -> Result<OutboxHandle, OutboxError> {
        let wake = Arc::new(Notify::new());
        let hook_wake = wake.clone();
        let install_hook = config.install_update_hook;
        let setup = db
            .call(move |conn| -> Result<(), rusqlite::Error> {
                ensure_outbox_tables(conn)?;
                if install_hook {
                    // Wake on _outbox writes only; the worker's own
                    // _results/_outbox writes just collapse into one
                    // pending wake.
                    conn.update_hook(Some(
                        move |_a: Action, _db: &str, table: &str, _rowid: i64| {
                            if table == "_outbox" {
                                hook_wake.notify_one();
                            }
                        },
                    ))?;
                }
                Ok(())
            })
            .await;
        match setup {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(OutboxError::Db(e)),
            Err(_) => return Err(OutboxError::DbClosed),
        }

        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_notify = Arc::new(Notify::new());
        let worker = OutboxWorker {
            db,
            transport,
            config,
            wake: wake.clone(),
            shutdown: shutdown.clone(),
            shutdown_notify: shutdown_notify.clone(),
        };
        let task = tokio::spawn(worker.run());
        Ok(OutboxHandle {
            wake,
            shutdown,
            shutdown_notify,
            task,
        })
    }

    async fn run(self) {
        let mut backoff: HashMap<String, Backoff> = HashMap::new();
        while !self.shutdown.load(Ordering::SeqCst) {
            let next_due = self.tick(&mut backoff).await;
            let wait = next_due
                .map(|d| d.min(self.config.poll_interval))
                .unwrap_or(self.config.poll_interval);
            tokio::select! {
                _ = self.wake.notified() => {}
                _ = self.shutdown_notify.notified() => {}
                _ = tokio::time::sleep(wait) => {
                    // The poll fallback: data_version moves on commits
                    // made by other connections, where update_hook stays
                    // silent. Every tick scans anyway, so this read is
                    // diagnostic.
                    let _ = self.data_version().await;
                }
            }
        }
        debug!("outbox worker stopped");
    }

    /// One pass over the actionable rows. Returns the shortest pending
    /// backoff, so the loop can sleep exactly that long.
    async fn tick(&self, backoff: &mut HashMap<String, Backoff>) -> Option<Duration> {
        let mut earliest: Option<Instant> = None;
        let propose = |candidate: Instant, earliest: &mut Option<Instant>| {
            *earliest = Some(earliest.map_or(candidate, |e| e.min(candidate)));
        };
        for row in self.scan().await {
            let now = Instant::now();
            if let Some(st) = backoff.get(&row.id)
                && st.next_attempt > now
            {
                propose(st.next_attempt, &mut earliest);
                continue;
            }
            // Expiry is judged from created_at, so it holds across
            // restarts.
            if row.age_secs >= self.config.expiry.as_secs() as i64 {
                self.mark_terminal(
                    &row.id,
                    "expired",
                    Some("request expired before completion"),
                )
                .await;
                backoff.remove(&row.id);
                continue;
            }
            match self.process_row(&row).await {
                Step::Settled => {
                    backoff.remove(&row.id);
                }
                Step::Again => {
                    let entry = backoff.entry(row.id.clone()).or_insert(Backoff {
                        attempts: 0,
                        next_attempt: Instant::now(),
                    });
                    entry.attempts += 1;
                    let when = Instant::now()
                        + backoff_delay(
                            entry.attempts,
                            self.config.base_backoff,
                            self.config.max_backoff,
                        );
                    entry.next_attempt = when;
                    propose(when, &mut earliest);
                }
            }
        }
        earliest.map(|t| t.saturating_duration_since(Instant::now()))
    }

    /// Ships one row and records what happened.
    async fn process_row(&self, row: &PendingRow) -> Step {
        // Params first: a malformed list can never succeed, so it is
        // terminal.
        let params = match parse_params(&row.params) {
            Ok(p) => p,
            Err(e) => {
                self.mark_terminal(&row.id, "error", Some(&e)).await;
                return Step::Settled;
            }
        };

        let Some(peer) = self.resolve_peer(&row.peer).await else {
            self.mark_terminal(
                &row.id,
                "error",
                Some(&format!(
                    "unknown peer {:?}: not in _peers and not a 64-hex endpoint id",
                    row.peer
                )),
            )
            .await;
            return Step::Settled;
        };

        // Bounded dial. Failing here leaves the row 'queued' for a
        // backed-off retry: nothing went out, so nothing is owed back.
        let mut stream = match timeout(self.config.open_timeout, self.transport.open(peer)).await {
            Ok(Ok((stream, _hello))) => stream,
            Ok(Err(e)) => {
                debug!(peer = %row.peer, error = %e, "outbox dial failed; will retry");
                return Step::Again;
            }
            Err(_elapsed) => {
                debug!(peer = %row.peer, "outbox dial timed out; will retry");
                return Step::Again;
            }
        };

        // The request is about to hit the wire: mark 'sent' now. A crash
        // after this point is fine because the resend reuses the same id
        // and the server's _applied answers without re-applying.
        self.mark_sent(&row.id).await;

        let req = Request {
            id: wire_request_id(&row.id),
            kind: infer_kind(&row.modulation, &row.signal),
            modulation: row.modulation.clone(),
            signal: row.signal.clone(),
            params,
            database: None,
            options: RequestOptions::default(),
        };
        if let Err(e) = stream.send(&req.to_envelope()).await {
            debug!(error = %e, "outbox send failed; will retry");
            return Step::Again;
        }
        let _ = stream.finish().await;

        self.read_response(row, &req, &mut stream).await
    }

    /// Drains the response stream, persisting rows and the terminal
    /// status.
    async fn read_response(&self, row: &PendingRow, req: &Request, stream: &mut T::Stream) -> Step {
        let mut reader = ResponseReader::new(req.id_string());
        let mut columns: Vec<String> = Vec::new();
        let mut result_rows: Vec<String> = Vec::new();
        let mut outcome: Option<Outcome> = None;

        loop {
            match stream.recv().await {
                Ok(Some(frame)) => match reader.accept(frame) {
                    Ok(ResponseEvent::Header(h)) => columns = h.columns,
                    Ok(ResponseEvent::Rows(rows)) => {
                        result_rows.extend(rows.iter().map(|r| row_to_json(&columns, r)));
                    }
                    Ok(ResponseEvent::Graph(g)) => {
                        // One _results row per graph chunk: a JSON array
                        // of the payload triples in N-Triples form.
                        let triples: Vec<serde_json::Value> = g
                            .payload
                            .iter()
                            .map(|t| serde_json::Value::from(format!("{t} .")))
                            .collect();
                        result_rows.push(serde_json::Value::Array(triples).to_string());
                    }
                    Ok(ResponseEvent::Generic(gnrc)) => {
                        // Passthrough frame: record the class so the
                        // application can tell something arrived.
                        result_rows.push(serde_json::json!({ "generic": gnrc.class }).to_string());
                    }
                    Ok(ResponseEvent::Done(_)) => {
                        outcome = Some(Outcome::Done);
                        break;
                    }
                    Ok(ResponseEvent::Denied(d)) => {
                        outcome = Some(Outcome::Denied(
                            d.reason.unwrap_or_else(|| "request denied".to_string()),
                        ));
                        break;
                    }
                    Ok(ResponseEvent::Error(e)) => {
                        let reason = e.reason.unwrap_or_default();
                        outcome = Some(Outcome::Error(format!("{}: {reason}", e.code)));
                        break;
                    }
                    Ok(ResponseEvent::Help { .. }) => {
                        outcome = Some(Outcome::Error(
                            "unexpected help response to an outbox request".to_string(),
                        ));
                        break;
                    }
                    Ok(ResponseEvent::Media(_)) => {
                        // A raw byte feed cannot live in _results.
                        outcome = Some(Outcome::Error(
                            "unexpected media response to an outbox request".to_string(),
                        ));
                        break;
                    }
                    Err(e) => {
                        // Choreography breach: stay 'sent' and retry.
                        debug!(error = %e, "outbox response out of choreography; will retry");
                        return Step::Again;
                    }
                },
                // Stream ended cleanly but no terminal frame: truncated.
                Ok(None) => break,
                Err(e) => {
                    debug!(error = %e, "outbox response transport error; will retry");
                    return Step::Again;
                }
            }
        }

        match outcome {
            Some(Outcome::Done) => {
                self.persist_done(&row.id, result_rows).await;
                Step::Settled
            }
            Some(Outcome::Denied(reason)) => {
                self.mark_terminal(&row.id, "denied", Some(&reason)).await;
                Step::Settled
            }
            Some(Outcome::Error(msg)) => {
                self.mark_terminal(&row.id, "error", Some(&msg)).await;
                Step::Settled
            }
            // The answer never fully landed. Stay 'sent' and retry; the
            // resend is idempotent.
            None => Step::Again,
        }
    }

    // --- db access ---------------------------------------------------------

    async fn scan(&self) -> Vec<PendingRow> {
        self.db
            .call(|conn| {
                let mut stmt = match conn.prepare(
                    "SELECT request_id, peer, mod, signal, params, \
                        CAST((julianday('now') - julianday(created_at)) * 86400 AS INTEGER) \
                     FROM _outbox \
                     WHERE status IN ('queued', 'sent') AND request_id IS NOT NULL \
                     ORDER BY created_at",
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(error = %e, "outbox scan prepare failed");
                        return Vec::new();
                    }
                };
                let rows = stmt.query_map([], |r| {
                    Ok(PendingRow {
                        id: r.get(0)?,
                        peer: r.get(1)?,
                        modulation: r.get(2)?,
                        signal: r.get(3)?,
                        params: r.get(4)?,
                        age_secs: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    })
                });
                match rows {
                    Ok(iter) => iter.filter_map(Result::ok).collect(),
                    Err(e) => {
                        warn!(error = %e, "outbox scan query failed");
                        Vec::new()
                    }
                }
            })
            .await
            .unwrap_or_default()
    }

    async fn resolve_peer(&self, peer: &str) -> Option<PeerId> {
        let peer_owned = peer.to_string();
        let found = self
            .db
            .call(move |conn| {
                conn.query_row(
                    "SELECT endpoint_id FROM _peers WHERE endpoint_id = ?1 OR name = ?1 LIMIT 1",
                    [&peer_owned],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .ok()
                .flatten()
            })
            .await
            .ok()
            .flatten();
        PeerId::from_str(&found.unwrap_or_else(|| peer.to_string())).ok()
    }

    async fn data_version(&self) -> i64 {
        self.db
            .call(|conn| {
                conn.query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
                    .unwrap_or(0)
            })
            .await
            .unwrap_or(0)
    }

    async fn mark_sent(&self, id: &str) {
        let rid = id.to_string();
        let res = self
            .db
            .call(move |conn| {
                conn.execute(
                    "UPDATE _outbox SET status = 'sent' \
                     WHERE request_id = ?1 AND status = 'queued'",
                    [&rid],
                )
            })
            .await;
        if let Ok(Err(e)) = res {
            warn!(error = %e, "failed to mark _outbox row sent");
        }
    }

    async fn mark_terminal(&self, id: &str, status: &str, error: Option<&str>) {
        let rid = id.to_string();
        let status = status.to_string();
        let error = error.map(str::to_string);
        let res = self
            .db
            .call(move |conn| {
                conn.execute(
                    "UPDATE _outbox SET status = ?2, error = ?3, \
                        completed_at = datetime('now') \
                     WHERE request_id = ?1",
                    rusqlite::params![rid, status, error],
                )
            })
            .await;
        if let Ok(Err(e)) = res {
            warn!(error = %e, "failed to write terminal _outbox status");
        }
    }

    /// Writes the result rows and flips the row to `done` in one
    /// transaction, clearing any earlier `_results` so a resend never
    /// duplicates.
    async fn persist_done(&self, id: &str, rows: Vec<String>) {
        let rid = id.to_string();
        let res = self
            .db
            .call(move |conn| -> Result<(), rusqlite::Error> {
                let tx = conn.transaction()?;
                tx.execute("DELETE FROM _results WHERE request_id = ?1", [&rid])?;
                for (i, row) in rows.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO _results (request_id, row_no, row) VALUES (?1, ?2, ?3)",
                        rusqlite::params![rid, i as i64, row],
                    )?;
                }
                tx.execute(
                    "UPDATE _outbox SET status = 'done', error = NULL, \
                        completed_at = datetime('now') \
                     WHERE request_id = ?1",
                    [&rid],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await;
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!(error = %e, "failed to persist _results/done"),
            Err(_) => warn!("db thread gone while persisting results"),
        }
    }
}

/// The terminal frame a response stream ended with.
enum Outcome {
    Done,
    Denied(String),
    Error(String),
}

fn parse_params(raw: &str) -> Result<Vec<Value>, String> {
    let json: Vec<serde_json::Value> =
        serde_json::from_str(raw).map_err(|e| format!("params is not a JSON array: {e}"))?;
    json.iter().map(json_to_value).collect()
}

/// `base * 2^(attempts-1)`, never above `max`.
fn backoff_delay(attempts: u32, base: Duration, max: Duration) -> Duration {
    let shift = attempts.saturating_sub(1).min(20);
    base.checked_mul(1u32 << shift).unwrap_or(max).min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_id_round_trips_ulid_and_folds_other() {
        let u = Ulid::new().to_string();
        assert_eq!(
            wire_request_id(&u),
            Ulid::from_string(&u).unwrap().to_bytes()
        );
        // Non-ULID tokens fold deterministically and stably.
        let a = wire_request_id("deadbeef");
        let b = wire_request_id("deadbeef");
        assert_eq!(a, b);
        assert_ne!(a, wire_request_id("deadbee0"));
    }

    #[test]
    fn kind_inference_sql() {
        assert_eq!(infer_kind("sql-sqlite", "  SELECT 1"), RequestKind::Query);
        assert_eq!(
            infer_kind("sql-sqlite", "with x as (select 1) select * from x"),
            RequestKind::Query
        );
        assert_eq!(
            infer_kind("sql-sqlite", "INSERT INTO t VALUES (1)"),
            RequestKind::Execute
        );
        assert_eq!(
            infer_kind("sql-sqlite", "update t set a=1"),
            RequestKind::Execute
        );
    }

    #[test]
    fn kind_inference_sparql() {
        assert_eq!(
            infer_kind("sparql", "SELECT ?s WHERE { ?s ?p ?o }"),
            RequestKind::Query
        );
        assert_eq!(
            infer_kind(
                "sparql",
                "PREFIX ex: <http://example.org/> ASK { ex:a ex:b ex:c }"
            ),
            RequestKind::Query
        );
        assert_eq!(
            infer_kind("sparql", "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }"),
            RequestKind::Query
        );
        assert_eq!(
            infer_kind(
                "sparql",
                "PREFIX ex: <http://example.org/> INSERT DATA { ex:a ex:b ex:c }"
            ),
            RequestKind::Execute
        );
        assert_eq!(
            infer_kind("sparql", "DELETE WHERE { ?s ?p ?o }"),
            RequestKind::Execute
        );
    }

    #[test]
    fn kind_inference_chat_is_execute() {
        assert_eq!(
            infer_kind("chat", "[] a rsntr:Message ; rsntr:body \"hi\" ."),
            RequestKind::Execute
        );
    }

    #[test]
    fn kind_inference_other_mods_default_to_query() {
        assert_eq!(infer_kind("help", ""), RequestKind::Query);
        assert_eq!(infer_kind("projection", "/"), RequestKind::Query);
        // A versioned mod pin resolves by its base name.
        assert_eq!(
            infer_kind("sql-sqlite/1", "DELETE FROM t"),
            RequestKind::Execute
        );
    }

    #[test]
    fn json_value_mapping() {
        assert_eq!(
            json_to_value(&serde_json::json!(null)).unwrap(),
            Value::Null
        );
        assert_eq!(
            json_to_value(&serde_json::json!(5)).unwrap(),
            Value::Integer(5)
        );
        assert_eq!(
            json_to_value(&serde_json::json!(true)).unwrap(),
            Value::Integer(1)
        );
        assert_eq!(
            json_to_value(&serde_json::json!("hi")).unwrap(),
            Value::Text("hi".into())
        );
        assert!(json_to_value(&serde_json::json!([1, 2])).is_err());
    }

    #[test]
    fn row_json_fills_nulls_by_column() {
        let row = Row {
            seq: 0,
            cells: vec![("body".to_string(), Value::Text("x".into()))],
        };
        let cols = vec!["id".to_string(), "body".to_string()];
        assert_eq!(row_to_json(&cols, &row), r#"[null,"x"]"#);
    }

    #[test]
    fn backoff_grows_and_caps() {
        let base = Duration::from_millis(100);
        let max = Duration::from_secs(1);
        assert_eq!(backoff_delay(1, base, max), Duration::from_millis(100));
        assert_eq!(backoff_delay(2, base, max), Duration::from_millis(200));
        assert_eq!(backoff_delay(3, base, max), Duration::from_millis(400));
        // Capped.
        assert_eq!(backoff_delay(10, base, max), max);
    }

    #[test]
    fn ddl_and_trigger_and_enqueue() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_outbox_tables(&conn).unwrap();
        ensure_outbox_tables(&conn).unwrap(); // idempotent

        // Direct INSERT without an id: the trigger backfills one.
        conn.execute(
            "INSERT INTO _outbox (peer, signal) VALUES ('p', 'SELECT 1')",
            [],
        )
        .unwrap();
        let (id, modulation): (String, String) = conn
            .query_row("SELECT request_id, mod FROM _outbox", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert!(!id.is_empty());
        assert_eq!(modulation, "sql-sqlite");

        // enqueue() mints a real ULID and carries the mod.
        let rid = enqueue(&conn, "p", "sparql", "SELECT ?s WHERE { ?s ?p ?o }", "[]").unwrap();
        assert!(Ulid::from_string(&rid).is_ok());
        let modulation: String = conn
            .query_row(
                "SELECT mod FROM _outbox WHERE request_id = ?1",
                [&rid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(modulation, "sparql");
    }
}
