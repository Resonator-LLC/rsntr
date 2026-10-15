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
        db: resonator_node::DbHandle,
        mut rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    ) -> JoinHandle<()> {
        // Cursors snapshot before the task starts (and before the caller
        // installs the observer): a row committed right after serve
        // start must not race the initialization and be skipped. They
        // persist in `_rsntr` across serve runs, so a message that
        // landed seconds before the previous server went away still
        // runs its hook in the next one (the lease moves between apps).
        let self_id = store::node_id(&dir)
            .map(|id| id.to_string())
            .unwrap_or_default();
        let (stored_chat, stored_inbox) = stored_cursors(&dir);
        let mut chat_cursor = stored_chat
            .unwrap_or_else(|| crate::events::max_rowid(&dir, "chat_messages").unwrap_or(0));
        let mut inbox_cursor =
            stored_inbox.unwrap_or_else(|| crate::events::max_rowid(&dir, "_inbox").unwrap_or(0));
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
                let mut moved = false;
                if dirty.contains("chat_messages") {
                    match crate::events::new_message_events(&dir, chat_cursor, &self_id) {
                        Ok((cursor, events)) => {
                            moved |= cursor != chat_cursor;
                            chat_cursor = cursor;
                            let payloads: Vec<String> = events
                                .iter()
                                .map(|e| crate::events::message_json(e).to_string())
                                .collect();
                            dispatch(&hooks, "message", &payloads).await;
                        }
                        Err(e) => tracing::warn!(error = %e, "reading new chat messages failed"),
                    }
                }
                if dirty.contains("_inbox") {
                    match crate::events::new_inbox_events(&dir, inbox_cursor) {
                        Ok((cursor, events)) => {
                            moved |= cursor != inbox_cursor;
                            inbox_cursor = cursor;
                            let payloads: Vec<String> = events
                                .iter()
                                .map(|e| crate::events::inbox_json(e).to_string())
                                .collect();
                            dispatch(&hooks, "inbox", &payloads).await;
                        }
                        Err(e) => tracing::warn!(error = %e, "reading new inbox rows failed"),
                    }
                }
                if moved {
                    // Through the pipeline's own connection: an own
                    // commit, so the watcher never mistakes it for a
                    // foreign write.
                    let (c, i) = (chat_cursor, inbox_cursor);
                    let _ = db
                        .call(move |conn| {
                            resonator_node::set_rsntr(conn, CURSOR_CHAT_KEY, &c.to_string())?;
                            resonator_node::set_rsntr(conn, CURSOR_INBOX_KEY, &i.to_string())
                        })
                        .await;
                }
            }
        })
    }
}

/// `_rsntr` keys holding the runner's last-seen rowids.
pub const CURSOR_CHAT_KEY: &str = "hooks_cursor_chat";
pub const CURSOR_INBOX_KEY: &str = "hooks_cursor_inbox";

fn stored_cursors(dir: &Path) -> (Option<i64>, Option<i64>) {
    let Ok(conn) = store::open_db(dir) else {
        return (None, None);
    };
    let read = |key: &str| {
        resonator_node::get_rsntr(&conn, key).and_then(|v| v.trim().parse::<i64>().ok())
    };
    (read(CURSOR_CHAT_KEY), read(CURSOR_INBOX_KEY))
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
    #[cfg(unix)]
    let pgid = child.id().map(|pid| pid as i32);
    // Handing the event over is inside the deadline, not before it. A
    // payload outgrows the 64 KiB a pipe holds in ordinary use (a
    // parked `turtle load` chunk is 64 KiB of triples, a chat body may
    // be 65536 bytes) and a hook that never reads its stdin (most of
    // them; the manual's own example is a `touch`) leaves that write
    // blocked for as long as the command runs. Waiting was already
    // armed against a slow hook; writing was not, and the runner is one
    // serialized task, so one such hook stopped every hook on the node.
    let mut stdin = child.stdin.take();
    let finished = tokio::time::timeout(hook_timeout(), async {
        if let Some(mut pipe) = stdin.take() {
            let _ = pipe.write_all(payload.as_bytes()).await;
            let _ = pipe.write_all(b"\n").await;
            // Dropping closes the pipe; the hook sees EOF after one event.
        }
        child.wait().await
    })
    .await;
    match finished {
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
