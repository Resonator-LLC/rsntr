//! The agent event vocabulary shared by `rsntr hook` (daemon-side
//! commands) and `rsntr await` (the harness wake primitive): the same
//! two JSON shapes read from the same tables past a rowid cursor, so an
//! agent learns one event format.
//!
//! - `{"event":"message", id, scope, from, at, received_at, body,
//!    blob_hash, blob_name}` — an incoming chat message (own sends are
//!   skipped; the cursor still advances over them);
//! - `{"event":"inbox", id, peer, kind, message, received_at}` — a knock
//!   or escalation parked for the owner and still awaiting a decision.

use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::store;

/// One incoming chat message past a cursor.
#[derive(Debug, Clone)]
pub struct MessageEvent {
    pub rowid: i64,
    pub id: String,
    pub scope: String,
    pub from: String,
    pub at: String,
    pub received_at: String,
    pub body: String,
    pub blob_hash: Option<String>,
    pub blob_name: Option<String>,
    /// Threading: the message this one answers (`rsntr:re`).
    pub re: Option<String>,
    /// Tags (`rsntr:tag`), sorted.
    pub tags: Vec<String>,
}

/// One `_inbox` row past a cursor.
#[derive(Debug, Clone)]
pub struct InboxEvent {
    pub rowid: i64,
    pub id: String,
    pub peer: String,
    /// `"knock"` or `"statement"`.
    pub kind: String,
    pub message: String,
    pub received_at: String,
}

/// True when `table` exists in the node database.
pub fn table_exists(dir: &Path, table: &str) -> bool {
    let Ok(conn) = store::open_db(dir) else {
        return false;
    };
    conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// The current max rowid of `table` (0 when absent or empty).
pub fn max_rowid(dir: &Path, table: &str) -> Result<i64> {
    if !table_exists(dir, table) {
        return Ok(0);
    }
    let conn = store::open_db(dir)?;
    Ok(conn.query_row(
        &format!("SELECT COALESCE(max(rowid), 0) FROM \"{table}\""),
        [],
        |r| r.get(0),
    )?)
}

/// Incoming messages past `cursor` (rows authored by `self_id` are
/// skipped). Returns the advanced cursor over every row scanned.
pub fn new_message_events(
    dir: &Path,
    cursor: i64,
    self_id: &str,
) -> Result<(i64, Vec<MessageEvent>)> {
    if !table_exists(dir, "chat_messages") {
        return Ok((cursor, Vec::new()));
    }
    let conn = store::open_db(dir)?;
    // A scaffold from before threading gains the columns here too.
    resonator_node::ensure_chat_columns(&conn)?;
    let mut stmt = conn.prepare(
        "SELECT rowid, id, scope, sender, at, received_at, body, blob_hash, blob_name, \
                re_id, tags \
         FROM chat_messages WHERE rowid > ?1 ORDER BY rowid",
    )?;
    let mut new_cursor = cursor;
    let mut events = Vec::new();
    let rows = stmt.query_map([cursor], |r| {
        Ok(MessageEvent {
            rowid: r.get(0)?,
            id: r.get(1)?,
            scope: r.get(2)?,
            from: r.get(3)?,
            at: r.get(4)?,
            received_at: r.get(5)?,
            body: r.get(6)?,
            blob_hash: r.get(7)?,
            blob_name: r.get(8)?,
            re: r.get(9)?,
            tags: resonator_node::tags_from_json(r.get::<_, Option<String>>(10)?.as_deref()),
        })
    })?;
    for row in rows {
        let ev = row?;
        new_cursor = new_cursor.max(ev.rowid);
        if ev.from != self_id {
            events.push(ev);
        }
    }
    Ok((new_cursor, events))
}

/// The rowid of the oldest `_inbox` row still awaiting a decision, if
/// any: where a first `rsntr await` starts, so open knocks and parked
/// requests are replayed rather than skipped.
pub fn oldest_pending_inbox_rowid(dir: &Path) -> Result<Option<i64>> {
    if !table_exists(dir, "_inbox") {
        return Ok(None);
    }
    let conn = store::open_db(dir)?;
    Ok(conn.query_row(
        "SELECT min(rowid) FROM _inbox WHERE decision IS NULL",
        [],
        |r| r.get::<_, Option<i64>>(0),
    )?)
}

/// `_inbox` rows past `cursor` that still await a decision, with the
/// cursor advanced over every row scanned (a row already decided is a
/// wake nobody needs; skipping it keeps a first-run replay to the open
/// requests only).
pub fn new_inbox_events(dir: &Path, cursor: i64) -> Result<(i64, Vec<InboxEvent>)> {
    let conn = store::open_db(dir)?;
    let mut stmt = conn.prepare(
        "SELECT rowid, request_id, peer, sql, params, received_at, decision \
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
            r.get::<_, Option<String>>(6)?,
        ))
    })?;
    for row in rows {
        let (rowid, id, peer, sql, params, received_at, decision) = row?;
        new_cursor = new_cursor.max(rowid);
        if decision.is_some() {
            continue;
        }
        let sql = sql.unwrap_or_default();
        let knock = sql.is_empty();
        events.push(InboxEvent {
            rowid,
            id,
            peer,
            kind: if knock { "knock" } else { "statement" }.to_string(),
            message: if knock {
                params.unwrap_or_default()
            } else {
                sql
            },
            received_at,
        });
    }
    Ok((new_cursor, events))
}

/// The stable JSON shape of a message event.
pub fn message_json(ev: &MessageEvent) -> serde_json::Value {
    json!({
        "event": "message",
        "id": ev.id,
        "scope": ev.scope,
        "from": ev.from,
        "at": ev.at,
        "received_at": ev.received_at,
        "body": ev.body,
        "blob_hash": ev.blob_hash,
        "blob_name": ev.blob_name,
        "re": ev.re,
        "tags": ev.tags,
    })
}

/// The stable JSON shape of an inbox event.
pub fn inbox_json(ev: &InboxEvent) -> serde_json::Value {
    json!({
        "event": "inbox",
        "id": ev.id,
        "peer": ev.peer,
        "kind": ev.kind,
        "message": ev.message,
        "received_at": ev.received_at,
    })
}
