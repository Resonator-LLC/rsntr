//! Daemon event hooks: owner-configured commands the serving node runs
//! when something new arrives, so an idle agent or harness can be woken.
//!
//! Configuration lives in the reserved `_hooks` table (`rsntr hook
//! add|list|rm|enable|disable`). While a node serves, a [`HookRunner`]
//! task listens to the pipeline's table observer; when `chat_messages`
//! or `_inbox` gains rows, each matching enabled hook runs as `sh -c
//! <command>` with one JSON event on stdin:
//!
//! - `{"event":"message","scope":…,"id":…,"from":…,"at":…,"body":…}` —
//!   an incoming chat message (own sends never fire hooks);
//! - `{"event":"inbox","id":…,"peer":…,"kind":…,"message":…}` — a knock
//!   or escalation parked for the owner.
//!
//! A hook's `event` is `message`, `inbox`, or `*`. Commands run with the
//! daemon owner's local power (they are owner-configured, the same trust
//! as the control socket), serialized, and killed after
//! [`HOOK_TIMEOUT`]; their stderr goes to the daemon log.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::task::JoinHandle;

use resonator_protocol::Value;

use crate::channel::{self, OwnerChannel, Prefer};
use crate::store;

/// The `_hooks` table (reserved; ensured at serve start and by `hook
/// add`).
pub const HOOKS_DDL: &str = "CREATE TABLE IF NOT EXISTS _hooks (\
  id          INTEGER PRIMARY KEY AUTOINCREMENT,\
  event       TEXT NOT NULL,\
  command     TEXT NOT NULL,\
  enabled     INTEGER NOT NULL DEFAULT 1,\
  created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))\
)";

/// The event names a hook may subscribe to.
pub const EVENTS: [&str; 3] = ["message", "inbox", "*"];

/// A hook command is killed after this long (`RSNTR_HOOK_TIMEOUT_MS`
/// overrides, mainly for tests).
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(30);

fn hook_timeout() -> Duration {
    std::env::var("RSNTR_HOOK_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(HOOK_TIMEOUT)
}

/// One `_hooks` row.
#[derive(Debug, Clone)]
pub struct HookRow {
    pub id: i64,
    pub event: String,
    pub command: String,
    pub enabled: bool,
    pub created_at: String,
}

async fn ensure_table(ch: &OwnerChannel) -> Result<()> {
    channel::execute(ch, HOOKS_DDL, vec![]).await?;
    Ok(())
}

/// `rsntr hook add <event> <command>`: registers an enabled hook and
/// returns its row id.
pub async fn hook_add(dir: &Path, prefer: Prefer, event: &str, command: &str) -> Result<i64> {
    if !EVENTS.contains(&event) {
        bail!("unknown event {event:?}; one of: {}", EVENTS.join(", "));
    }
    let ch = OwnerChannel::open(dir, prefer).await?;
    ensure_table(&ch).await?;
    let done = channel::execute(
        &ch,
        "INSERT INTO _hooks (event, command) VALUES (?1, ?2)",
        vec![
            Value::Text(event.to_string()),
            Value::Text(command.to_string()),
        ],
    )
    .await?;
    Ok(done.last_insert_rowid.unwrap_or(0))
}

/// `rsntr hook list`: every `_hooks` row.
pub async fn hook_list(dir: &Path, prefer: Prefer) -> Result<Vec<HookRow>> {
    let ch = OwnerChannel::open(dir, prefer).await?;
    ensure_table(&ch).await?;
    let (_cols, rows, _done) = channel::query_rows(
        &ch,
        "SELECT id, event, command, enabled, created_at FROM _hooks ORDER BY id",
        vec![],
    )
    .await?;
    Ok(rows
        .iter()
        .map(|row| HookRow {
            id: channel::cell_text(row, "id")
                .and_then(|t| t.parse().ok())
                .unwrap_or(0),
            event: channel::cell_text(row, "event").unwrap_or_default(),
            command: channel::cell_text(row, "command").unwrap_or_default(),
            enabled: channel::cell_text(row, "enabled").as_deref() != Some("0"),
            created_at: channel::cell_text(row, "created_at").unwrap_or_default(),
        })
        .collect())
}

/// `rsntr hook enable|disable <id>`: true when the row existed.
pub async fn hook_set_enabled(dir: &Path, prefer: Prefer, id: i64, enabled: bool) -> Result<bool> {
    let ch = OwnerChannel::open(dir, prefer).await?;
    ensure_table(&ch).await?;
    let done = channel::execute(
        &ch,
        "UPDATE _hooks SET enabled = ?1 WHERE id = ?2",
        vec![Value::Integer(i64::from(enabled)), Value::Integer(id)],
    )
    .await?;
    Ok(done.affected_rows.unwrap_or(0) > 0)
}

/// `rsntr hook rm <id>`: true when the row existed.
pub async fn hook_rm(dir: &Path, prefer: Prefer, id: i64) -> Result<bool> {
    let ch = OwnerChannel::open(dir, prefer).await?;
    ensure_table(&ch).await?;
    let done = channel::execute(
        &ch,
        "DELETE FROM _hooks WHERE id = ?1",
        vec![Value::Integer(id)],
    )
    .await?;
    Ok(done.affected_rows.unwrap_or(0) > 0)
}

// ---------------------------------------------------------------------
// The serving-side runner
// ---------------------------------------------------------------------

/// The serving node's hook dispatcher. Fed table names by the pipeline's
/// table observer over an unbounded channel (a send never blocks a
/// commit); reads new rows past its cursors on its own connections.
pub struct HookRunner;

impl HookRunner {
    /// Spawns the runner for a serving node directory. The handle is
    /// aborted at shutdown.
    pub fn spawn(
        dir: PathBuf,
        mut rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    ) -> JoinHandle<()> {
        // Cursors snapshot before the task starts (and before the caller
        // installs the observer): a row committed right after serve
        // start must not race the initialization and be skipped.
        let self_id = store::node_id(&dir)
            .map(|id| id.to_string())
            .unwrap_or_default();
        let mut chat_cursor = max_rowid(&dir, "chat_messages").unwrap_or(0);
        let mut inbox_cursor = max_rowid(&dir, "_inbox").unwrap_or(0);
        let mut hooks = load_enabled(&dir).unwrap_or_default();
        tokio::spawn(async move {
            while let Some(first) = rx.recv().await {
                // Debounce one burst into one read per table.
                let mut dirty: HashSet<String> = HashSet::new();
                dirty.insert(first);
                let quiet = tokio::time::Instant::now() + Duration::from_millis(100);
                loop {
                    match tokio::time::timeout_at(quiet, rx.recv()).await {
                        Ok(Some(table)) => {
                            dirty.insert(table);
                        }
                        Ok(None) => return,
                        Err(_elapsed) => break,
                    }
                }

                if dirty.contains("_hooks") {
                    match load_enabled(&dir) {
                        Ok(rows) => hooks = rows,
                        Err(e) => tracing::warn!(error = %e, "reloading _hooks failed"),
                    }
                }
                if dirty.contains("chat_messages") {
                    match new_message_events(&dir, chat_cursor, &self_id) {
                        Ok((cursor, events)) => {
                            chat_cursor = cursor;
                            dispatch(&hooks, "message", &events).await;
                        }
                        Err(e) => tracing::warn!(error = %e, "reading new chat messages failed"),
                    }
                }
                if dirty.contains("_inbox") {
                    match new_inbox_events(&dir, inbox_cursor) {
                        Ok((cursor, events)) => {
                            inbox_cursor = cursor;
                            dispatch(&hooks, "inbox", &events).await;
                        }
                        Err(e) => tracing::warn!(error = %e, "reading new inbox rows failed"),
                    }
                }
            }
        })
    }
}

fn max_rowid(dir: &Path, table: &str) -> Result<i64> {
    let conn = store::open_db(dir)?;
    let exists: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get(0),
    )?;
    if exists == 0 {
        return Ok(0);
    }
    Ok(conn.query_row(
        &format!("SELECT COALESCE(max(rowid), 0) FROM \"{table}\""),
        [],
        |r| r.get(0),
    )?)
}

fn load_enabled(dir: &Path) -> Result<Vec<HookRow>> {
    let conn = store::open_db(dir)?;
    let exists: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = '_hooks'",
        [],
        |r| r.get(0),
    )?;
    if exists == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT id, event, command, enabled, created_at FROM _hooks WHERE enabled = 1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(HookRow {
                id: r.get(0)?,
                event: r.get(1)?,
                command: r.get(2)?,
                enabled: r.get::<_, i64>(3)? != 0,
                created_at: r.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// New incoming messages past `cursor` as event payloads (own sends are
/// skipped; a hook wakes on what arrived, not on what this node said).
fn new_message_events(dir: &Path, cursor: i64, self_id: &str) -> Result<(i64, Vec<String>)> {
    let conn = store::open_db(dir)?;
    let mut stmt = conn.prepare(
        "SELECT rowid, id, scope, sender, at, body, blob_hash, blob_name \
         FROM chat_messages WHERE rowid > ?1 ORDER BY rowid",
    )?;
    let mut new_cursor = cursor;
    let mut events = Vec::new();
    let rows = stmt.query_map([cursor], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, Option<String>>(7)?,
        ))
    })?;
    for row in rows {
        let (rowid, id, scope, sender, at, body, blob_hash, blob_name) = row?;
        new_cursor = new_cursor.max(rowid);
        if sender == self_id {
            continue;
        }
        events.push(
            json!({
                "event": "message",
                "id": id,
                "scope": scope,
                "from": sender,
                "at": at,
                "body": body,
                "blob_hash": blob_hash,
                "blob_name": blob_name,
            })
            .to_string(),
        );
    }
    Ok((new_cursor, events))
}

/// New `_inbox` rows past `cursor` as event payloads.
fn new_inbox_events(dir: &Path, cursor: i64) -> Result<(i64, Vec<String>)> {
    let conn = store::open_db(dir)?;
    let mut stmt = conn.prepare(
        "SELECT rowid, request_id, peer, sql, params, received_at \
         FROM _inbox WHERE rowid > ?1 ORDER BY rowid",
    )?;
    let mut new_cursor = cursor;
    let mut events = Vec::new();
    let rows = stmt.query_map([cursor], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, String>(5)?,
        ))
    })?;
    for row in rows {
        let (rowid, request_id, peer, sql, params, received_at) = row?;
        new_cursor = new_cursor.max(rowid);
        let sql = sql.unwrap_or_default();
        let knock = sql.is_empty();
        events.push(
            json!({
                "event": "inbox",
                "id": request_id,
                "peer": peer,
                "kind": if knock { "knock" } else { "statement" },
                "message": if knock { params.unwrap_or_default() } else { sql },
                "received_at": received_at,
            })
            .to_string(),
        );
    }
    Ok((new_cursor, events))
}

/// Runs every hook matching `event` for each payload, serialized.
async fn dispatch(hooks: &[HookRow], event: &str, payloads: &[String]) {
    for payload in payloads {
        for hook in hooks {
            if hook.event != "*" && hook.event != event {
                continue;
            }
            run_hook(hook, payload).await;
        }
    }
}

async fn run_hook(hook: &HookRow, payload: &str) {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(&hook.command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);
    // Own process group, so the timeout can take down `sh -c` pipelines.
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(hook = hook.id, error = %e, "hook failed to spawn");
            return;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes()).await;
        let _ = stdin.write_all(b"\n").await;
        // Dropping closes the pipe; the hook sees EOF after one event.
    }
    #[cfg(unix)]
    let pgid = child.id().map(|pid| pid as i32);
    match tokio::time::timeout(hook_timeout(), child.wait()).await {
        Ok(Ok(status)) => {
            if !status.success() {
                tracing::warn!(hook = hook.id, %status, "hook command failed");
            }
        }
        Ok(Err(e)) => tracing::warn!(hook = hook.id, error = %e, "waiting on the hook failed"),
        Err(_elapsed) => {
            tracing::warn!(hook = hook.id, "hook command timed out; killing it");
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
    // A hook run ends here, stragglers included: sweep the group so a
    // backgrounded or timed-out pipeline member never outlives its run.
    #[cfg(unix)]
    if let Some(pgid) = pgid {
        unsafe { libc::kill(-pgid, libc::SIGKILL) };
    }
}
