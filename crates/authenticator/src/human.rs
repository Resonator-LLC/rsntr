//! The `"human"` tier: the authority tier. A statement request that
//! reaches it parks as a pending `_inbox` row (the same table knocks
//! park in; the node crate owns that DDL) and the live request ends in
//! Deny("pending human decision") - the park is the answer, not a stall.
//! The owner answers later over the owner channel ([`answer_inbox`], or
//! `rsntr inbox answer`); the answer feeds the `_decisions` cache so the
//! NEXT identical request gets through the `"cache"` tier, and with
//! remember it also writes generated `_policy` rows (allow-and-remember)
//! so the owner is only asked novel questions.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::cache::record_decision;
use crate::chain::Tier;
use crate::fingerprint::statement_fingerprint;
use crate::types::{Decision, Footprint};

/// The deny reason of a parked request; stable, clients may match it.
pub const PENDING_REASON: &str = "pending human decision";

/// What the human tier stores in the `_inbox.params` column of a parked
/// statement request: everything the answer flow needs, so answering
/// never re-derives a footprint.
#[derive(Debug, Serialize, Deserialize)]
pub struct ParkedParams {
    pub action: String,
    pub fingerprint: String,
    pub footprint: Footprint,
}

/// The human tier; registered in the chain as `"human"`.
#[derive(Debug, Default)]
pub struct HumanTier;

impl Tier for HumanTier {
    fn name(&self) -> &str {
        "human"
    }

    fn decide(
        &self,
        conn: &Connection,
        peer: &str,
        action: &str,
        footprint: &Footprint,
        statement: &str,
    ) -> Decision {
        // Footprint-less requests (knock, media, entrain) have their own
        // park/deny flows in the node; this tier only parks statements.
        if statement.is_empty() {
            return Decision::Escalate;
        }
        if footprint.has_denied() {
            return Decision::Escalate;
        }
        let fp = statement_fingerprint(action, statement, footprint);

        // At most one pending row per (peer, shape): retries of the same
        // request do not stack up while the owner thinks.
        let pending: Result<Option<()>, _> = conn
            .query_row(
                "SELECT 1 FROM _inbox WHERE peer = ?1 AND decision IS NULL \
                   AND json_extract(params, '$.fingerprint') = ?2",
                (peer, &fp),
                |_| Ok(()),
            )
            .optional();
        match pending {
            Ok(Some(())) => {
                return Decision::Deny {
                    reason: PENDING_REASON.to_string(),
                };
            }
            Ok(None) => {}
            Err(err) => {
                warn!(error = %err, "human tier inbox lookup failed; escalating");
                return Decision::Escalate;
            }
        }

        let params = ParkedParams {
            action: action.to_string(),
            fingerprint: fp,
            footprint: footprint.clone(),
        };
        let params_json = match serde_json::to_string(&params) {
            Ok(j) => j,
            Err(err) => {
                warn!(error = %err, "human tier params encode failed; escalating");
                return Decision::Escalate;
            }
        };
        let park = conn.execute(
            "INSERT INTO _inbox (request_id, peer, sql, params, received_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                ulid::Ulid::new().to_string(),
                peer,
                statement,
                params_json,
                now_rfc3339(),
            ),
        );
        match park {
            Ok(_) => Decision::Deny {
                reason: PENDING_REASON.to_string(),
            },
            Err(err) => {
                warn!(error = %err, "human tier failed to park in _inbox; escalating");
                Decision::Escalate
            }
        }
    }
}

/// What answering an `_inbox` row did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxAnswer {
    pub peer: String,
    /// True for a parked knock (empty `sql`): allowing one admits the
    /// peer instead of feeding the decision cache.
    pub knock: bool,
    /// The `_policy` table names written by remember (empty otherwise).
    pub remembered: Vec<String>,
}

/// Answer-flow failure; a plain message for CLI/UI display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerError(pub String);

impl std::fmt::Display for AnswerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AnswerError {}

impl From<rusqlite::Error> for AnswerError {
    fn from(e: rusqlite::Error) -> Self {
        Self(e.to_string())
    }
}

/// Answers one pending `_inbox` row on a direct connection: sets its
/// decision, and
///
/// - for a parked statement, records the answer in `_decisions` (the
///   next identical request hits the cache tier) and, with `remember`,
///   writes one generated `_policy` row per footprint table with the
///   answer's effect (allow-and-remember);
/// - for a parked knock (empty `sql`), an allow admits the peer into
///   `_peers`.
///
/// The CLI mirrors this flow as owner-channel envelopes.
pub fn answer_inbox(
    conn: &Connection,
    request_id: &str,
    allow: bool,
    remember: bool,
) -> Result<InboxAnswer, AnswerError> {
    let row: Option<(String, String, String, Option<String>)> = conn
        .query_row(
            "SELECT peer, sql, params, decision FROM _inbox WHERE request_id = ?1",
            [request_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((peer, sql, params, decision)) = row else {
        return Err(AnswerError(format!("no _inbox row with id {request_id}")));
    };
    if let Some(d) = decision {
        return Err(AnswerError(format!(
            "inbox row {request_id} is already decided ({d})"
        )));
    }
    let decision_str = if allow { "allow" } else { "deny" };
    conn.execute(
        "UPDATE _inbox SET decision = ?1, decided_by = 'human' WHERE request_id = ?2",
        (decision_str, request_id),
    )?;

    if sql.is_empty() {
        if allow {
            conn.execute(
                "INSERT OR IGNORE INTO _peers (endpoint_id, added_at, notes) \
                 VALUES (?1, ?2, ?3)",
                (
                    &peer,
                    now_rfc3339(),
                    format!("admitted via inbox {request_id}"),
                ),
            )?;
        }
        return Ok(InboxAnswer {
            peer,
            knock: true,
            remembered: Vec::new(),
        });
    }

    let parked: ParkedParams = serde_json::from_str(&params).map_err(|e| {
        AnswerError(format!(
            "inbox row {request_id} has unreadable params ({e}); answer it with plain SQL"
        ))
    })?;
    record_decision(
        conn,
        &peer,
        &parked.fingerprint,
        decision_str,
        Some(&format!("answered from _inbox {request_id}")),
        "human",
        None,
    )?;

    let mut remembered = Vec::new();
    if remember {
        let tables: Vec<String> = if parked.footprint.tables.is_empty() {
            vec!["*".to_string()]
        } else {
            parked.footprint.tables.keys().cloned().collect()
        };
        for table in tables {
            conn.execute(
                "INSERT INTO _policy (peer_or_group, table_name, action, effect, note) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    &peer,
                    &table,
                    &parked.action,
                    decision_str,
                    format!("remembered from inbox {request_id}"),
                ),
            )?;
            remembered.push(table);
        }
    }
    Ok(InboxAnswer {
        peer,
        knock: false,
        remembered,
    })
}

/// Current UTC time in RFC 3339 form with second precision (matching
/// the node's ledger timestamps; no chrono dependency).
pub(crate) fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days-since-epoch to (year, month, day). Howard Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::ensure_decisions_table;
    use crate::chain::Chain;
    use crate::policy::{PolicyTier, ensure_policy_table};
    use crate::types::ActionKind;

    /// The `_inbox` shape from the node crate (crates/node/src/ddl.rs),
    /// mirrored here so these tests stay crate-local.
    const INBOX_DDL: &str = "\
    CREATE TABLE IF NOT EXISTS _inbox (
      request_id   TEXT PRIMARY KEY,
      peer         TEXT NOT NULL,
      sql          TEXT NOT NULL,
      params       TEXT NOT NULL,
      decision     TEXT,
      decided_by   TEXT,
      received_at  TEXT NOT NULL
    );";

    fn setup(chain_json: &str) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(INBOX_DDL).unwrap();
        conn.execute_batch("CREATE TABLE _rsntr (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .unwrap();
        conn.execute(
            "INSERT INTO _rsntr (key, value) VALUES ('auth_chain', ?1)",
            [chain_json],
        )
        .unwrap();
        ensure_decisions_table(&conn).unwrap();
        ensure_policy_table(&conn).unwrap();
        conn
    }

    fn fp() -> Footprint {
        Footprint::from_tables(ActionKind::Read, [("notes", vec!["id", "body"])])
    }

    fn pending_ids(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT request_id FROM _inbox WHERE decision IS NULL")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn parks_once_and_denies_pending() {
        let conn = setup("[]");
        let d = HumanTier.decide(
            &conn,
            "alice",
            "read",
            &fp(),
            "SELECT id FROM notes WHERE id = 1",
        );
        assert_eq!(
            d,
            Decision::Deny {
                reason: PENDING_REASON.to_string()
            }
        );
        assert_eq!(pending_ids(&conn).len(), 1);
        // A literal variant of the same shape does not stack a second row.
        let d = HumanTier.decide(
            &conn,
            "alice",
            "read",
            &fp(),
            "SELECT id FROM notes WHERE id = 2",
        );
        assert_eq!(
            d,
            Decision::Deny {
                reason: PENDING_REASON.to_string()
            }
        );
        assert_eq!(pending_ids(&conn).len(), 1);
        // A different shape parks separately.
        HumanTier.decide(&conn, "alice", "read", &fp(), "SELECT body FROM notes");
        assert_eq!(pending_ids(&conn).len(), 2);
    }

    #[test]
    fn empty_statement_escalates() {
        let conn = setup("[]");
        assert_eq!(
            HumanTier.decide(&conn, "alice", "knock", &Footprint::default(), ""),
            Decision::Escalate
        );
        assert!(pending_ids(&conn).is_empty());
    }

    #[test]
    fn answer_unblocks_the_next_identical_request_via_cache() {
        let conn = setup(r#"["cache", "human"]"#);
        let mut chain = Chain::new();
        chain.register(Box::new(crate::cache::CacheTier));
        chain.register(Box::new(HumanTier));

        let sql1 = "SELECT id FROM notes WHERE id = 1";
        let d = chain.decide(&conn, "alice", "read", &fp(), sql1);
        assert_eq!(d.decided_by, "human");
        assert!(matches!(d.decision, Decision::Deny { ref reason } if reason == PENDING_REASON));

        let ids = pending_ids(&conn);
        assert_eq!(ids.len(), 1);
        let answered = answer_inbox(&conn, &ids[0], true, false).unwrap();
        assert_eq!(answered.peer, "alice");
        assert!(!answered.knock);
        assert!(answered.remembered.is_empty());

        // The NEXT identical request (different literal) hits the cache.
        let d = chain.decide(
            &conn,
            "alice",
            "read",
            &fp(),
            "SELECT id FROM notes WHERE id = 7",
        );
        assert_eq!(d.decided_by, "cache");
        assert_eq!(d.decision, Decision::Allow);

        // Answering twice is refused.
        assert!(answer_inbox(&conn, &ids[0], true, false).is_err());
        // Unknown ids are refused.
        assert!(answer_inbox(&conn, "nope", true, false).is_err());
    }

    #[test]
    fn deny_answer_is_remembered_in_the_cache() {
        let conn = setup(r#"["cache", "human"]"#);
        let mut chain = Chain::new();
        chain.register(Box::new(crate::cache::CacheTier));
        chain.register(Box::new(HumanTier));

        chain.decide(
            &conn,
            "mallory",
            "read",
            &fp(),
            "SELECT body FROM notes WHERE id = 1",
        );
        let ids = pending_ids(&conn);
        answer_inbox(&conn, &ids[0], false, false).unwrap();

        let d = chain.decide(
            &conn,
            "mallory",
            "read",
            &fp(),
            "SELECT body FROM notes WHERE id = 2",
        );
        assert_eq!(d.decided_by, "cache");
        assert!(matches!(d.decision, Decision::Deny { .. }));
    }

    #[test]
    fn allow_and_remember_writes_the_policy_row() {
        let conn = setup(r#"["policy", "human"]"#);
        let mut chain = Chain::new();
        chain.register(Box::new(PolicyTier));
        chain.register(Box::new(HumanTier));

        let d = chain.decide(
            &conn,
            "alice",
            "read",
            &fp(),
            "SELECT id FROM notes WHERE id = 1",
        );
        assert_eq!(d.decided_by, "human");

        let ids = pending_ids(&conn);
        let answered = answer_inbox(&conn, &ids[0], true, true).unwrap();
        assert_eq!(answered.remembered, vec!["notes".to_string()]);

        let (peer, table, action, effect): (String, String, String, String) = conn
            .query_row(
                "SELECT peer_or_group, table_name, action, effect FROM _policy",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (
                peer.as_str(),
                table.as_str(),
                action.as_str(),
                effect.as_str()
            ),
            ("alice", "notes", "read", "allow")
        );

        // The generated row now decides at the policy tier, no cache
        // involved in this chain.
        let d = chain.decide(
            &conn,
            "alice",
            "read",
            &fp(),
            "SELECT id FROM notes WHERE id = 9",
        );
        assert_eq!(d.decided_by, "policy");
        assert_eq!(d.decision, Decision::Allow);
    }

    #[test]
    fn knock_answer_admits_the_peer() {
        let conn = setup("[]");
        conn.execute_batch(
            "CREATE TABLE _peers (endpoint_id TEXT PRIMARY KEY, name TEXT, relay_url TEXT, \
             addrs TEXT, added_at TEXT NOT NULL, last_seen TEXT, notes TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO _inbox (request_id, peer, sql, params, received_at) \
             VALUES ('k1', 'stranger', '', 'knock: hello', ?1)",
            [now_rfc3339()],
        )
        .unwrap();
        let answered = answer_inbox(&conn, "k1", true, false).unwrap();
        assert!(answered.knock);
        let admitted: i64 = conn
            .query_row(
                "SELECT count(*) FROM _peers WHERE endpoint_id = 'stranger'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(admitted, 1);
    }
}
