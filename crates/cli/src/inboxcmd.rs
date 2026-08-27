//! `rsntr inbox list` / `rsntr inbox answer`: the owner's view of the
//! `_inbox` parking table, driven over the owner channel as ordinary
//! `sql-sqlite` envelopes (docs/owner-channel.md).
//!
//! Answering mirrors `resonator_authenticator::answer_inbox` as
//! owner-channel statements: the row's decision is set, a parked
//! statement feeds the `_decisions` cache (so the next identical request
//! hits the `"cache"` tier), `--remember` writes generated `_policy`
//! rows (allow-and-remember), and allowing a parked knock admits the
//! peer into `_peers`.

use std::path::Path;

use anyhow::{Result, bail};

use resonator_authenticator::ParkedParams;
use resonator_protocol::Value;

use crate::channel::{self, OwnerChannel, Prefer};

/// RFC 3339 UTC seconds, computed by sqlite so both transports stamp
/// ledger rows the same way the node does.
const NOW_SQL: &str = "strftime('%Y-%m-%dT%H:%M:%SZ', 'now')";

/// One `_inbox` row, for listing.
#[derive(Debug, Clone)]
pub struct InboxRow {
    pub request_id: String,
    pub peer: String,
    /// `"knock"` (empty `sql`) or `"statement"`.
    pub kind: String,
    /// The knock message, or the parked statement text.
    pub summary: String,
    pub decision: Option<String>,
    pub received_at: String,
}

/// What `rsntr inbox answer` did.
#[derive(Debug, Clone)]
pub struct AnswerReport {
    pub request_id: String,
    pub peer: String,
    /// `"allow"` or `"deny"`.
    pub decision: String,
    /// True when the answered row was a parked knock.
    pub knock: bool,
    /// The `_policy` table names written by `--remember`.
    pub remembered: Vec<String>,
    /// The `table:action` grants written by `--grant` on a knock allow.
    pub granted: Vec<String>,
}

/// Lists `_inbox` rows, pending only unless `all`.
pub async fn inbox_list_with(dir: &Path, prefer: Prefer, all: bool) -> Result<Vec<InboxRow>> {
    let ch = OwnerChannel::open(dir, prefer).await?;
    let sql = if all {
        "SELECT request_id, peer, sql, params, decision, received_at \
         FROM _inbox ORDER BY received_at, request_id"
    } else {
        "SELECT request_id, peer, sql, params, decision, received_at \
         FROM _inbox WHERE decision IS NULL ORDER BY received_at, request_id"
    };
    let (_cols, rows, _done) = channel::query_rows(&ch, sql, vec![]).await?;
    Ok(rows
        .iter()
        .map(|row| {
            let sql_text = channel::cell_text(row, "sql").unwrap_or_default();
            let params = channel::cell_text(row, "params").unwrap_or_default();
            let knock = sql_text.is_empty();
            InboxRow {
                request_id: channel::cell_text(row, "request_id").unwrap_or_default(),
                peer: channel::cell_text(row, "peer").unwrap_or_default(),
                kind: (if knock { "knock" } else { "statement" }).to_string(),
                summary: if knock { params } else { sql_text },
                decision: channel::cell_text(row, "decision"),
                received_at: channel::cell_text(row, "received_at").unwrap_or_default(),
            }
        })
        .collect())
}

/// Answers one pending `_inbox` row; see the module doc for the effects.
pub async fn inbox_answer_with(
    dir: &Path,
    prefer: Prefer,
    request_id: &str,
    allow: bool,
    remember: bool,
    grants: &[String],
) -> Result<AnswerReport> {
    let ch = OwnerChannel::open(dir, prefer).await?;
    let (_cols, rows, _done) = channel::query_rows(
        &ch,
        "SELECT peer, sql, params, decision FROM _inbox WHERE request_id = ?1",
        vec![Value::Text(request_id.to_string())],
    )
    .await?;
    let Some(row) = rows.first() else {
        bail!("no _inbox row with id {request_id}");
    };
    if let Some(d) = channel::cell_text(row, "decision") {
        bail!("inbox row {request_id} is already decided ({d})");
    }
    let peer = channel::cell_text(row, "peer").unwrap_or_default();
    let sql_text = channel::cell_text(row, "sql").unwrap_or_default();
    let params_text = channel::cell_text(row, "params").unwrap_or_default();
    let decision = if allow { "allow" } else { "deny" };

    channel::execute(
        &ch,
        "UPDATE _inbox SET decision = ?1, decided_by = 'human' WHERE request_id = ?2",
        vec![
            Value::Text(decision.to_string()),
            Value::Text(request_id.to_string()),
        ],
    )
    .await?;

    if sql_text.is_empty() {
        let mut granted = Vec::new();
        if allow {
            channel::execute(
                &ch,
                &format!(
                    "INSERT OR IGNORE INTO _peers (endpoint_id, added_at, notes) \
                     VALUES (?1, {NOW_SQL}, ?2)"
                ),
                vec![
                    Value::Text(peer.clone()),
                    Value::Text(format!("admitted via inbox {request_id}")),
                ],
            )
            .await?;
            // Admission alone authorizes nothing: the chain still
            // tail-denies every request until `_policy` says otherwise.
            // `--grant table=action` writes those rows in the same
            // breath ('=' because both tables and actions may contain
            // colons: `chat:direct`, `mod:cameras`).
            for grant in grants {
                let Some((table, action)) = grant.split_once('=') else {
                    bail!(
                        "--grant {grant:?}: expected table=action (e.g. 'shop_products=read', '*=mod:cameras')"
                    );
                };
                channel::execute(
                    &ch,
                    "INSERT INTO _policy (peer_or_group, table_name, action, effect, note) \
                     SELECT ?1, ?2, ?3, 'allow', ?4 \
                     WHERE NOT EXISTS (SELECT 1 FROM _policy \
                        WHERE peer_or_group = ?1 AND table_name = ?2 AND action = ?3 AND effect = 'allow')",
                    vec![
                        Value::Text(peer.clone()),
                        Value::Text(table.to_string()),
                        Value::Text(action.to_string()),
                        Value::Text(format!("granted with knock admit {request_id}")),
                    ],
                )
                .await?;
                granted.push(grant.clone());
            }
        }
        return Ok(AnswerReport {
            request_id: request_id.to_string(),
            peer,
            decision: decision.to_string(),
            knock: true,
            remembered: Vec::new(),
            granted,
        });
    }

    let parked: ParkedParams = serde_json::from_str(&params_text).map_err(|e| {
        anyhow::anyhow!(
            "inbox row {request_id} has unreadable params ({e}); \
             answer it with plain SQL via `rsntr query`"
        )
    })?;
    channel::execute(
        &ch,
        &format!(
            "INSERT INTO _decisions \
             (peer, fingerprint, decision, reason, decided_by, created_at, expires_at) \
             VALUES (?1, ?2, ?3, ?4, 'human', {NOW_SQL}, NULL) \
             ON CONFLICT(peer, fingerprint) DO UPDATE SET \
               decision = ?3, reason = ?4, decided_by = 'human', \
               created_at = {NOW_SQL}, expires_at = NULL"
        ),
        vec![
            Value::Text(peer.clone()),
            Value::Text(parked.fingerprint.clone()),
            Value::Text(decision.to_string()),
            Value::Text(format!("answered from _inbox {request_id}")),
        ],
    )
    .await?;

    let mut remembered = Vec::new();
    if remember {
        let tables: Vec<String> = if parked.footprint.tables.is_empty() {
            vec!["*".to_string()]
        } else {
            parked.footprint.tables.keys().cloned().collect()
        };
        for table in tables {
            channel::execute(
                &ch,
                "INSERT INTO _policy (peer_or_group, table_name, action, effect, note) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                vec![
                    Value::Text(peer.clone()),
                    Value::Text(table.clone()),
                    Value::Text(parked.action.clone()),
                    Value::Text(decision.to_string()),
                    Value::Text(format!("remembered from inbox {request_id}")),
                ],
            )
            .await?;
            remembered.push(table);
        }
    }
    Ok(AnswerReport {
        request_id: request_id.to_string(),
        peer,
        decision: decision.to_string(),
        knock: false,
        remembered,
        granted: Vec::new(),
    })
}
