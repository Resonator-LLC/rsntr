//! The `_decisions` table and the `"cache"` tier.
//!
//! Live decisions can be expensive (human, script, later ai), so final
//! answers are remembered keyed on `(peer, statement fingerprint)`; the
//! fingerprint is the normalized statement shape plus footprint (see
//! [`crate::fingerprint`]), never raw text, so parameterized repeats
//! hit. The tier is configured first in `auth_chain`, answers on a hit,
//! and abstains on a miss so the later tiers decide. Answered `_inbox`
//! rows feed this table via [`crate::human::answer_inbox`]. This crate
//! owns the `_decisions` DDL.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension};
use tracing::warn;

use crate::chain::Tier;
use crate::fingerprint::statement_fingerprint;
use crate::types::{Decision, Footprint};

/// Remembered final answers; one row per `(peer, fingerprint)`.
pub const DECISIONS_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _decisions (
  peer        TEXT NOT NULL,      -- EndpointId text
  fingerprint TEXT NOT NULL,      -- statement_fingerprint hex
  decision    TEXT NOT NULL,      -- allow | deny
  reason      TEXT,
  decided_by  TEXT NOT NULL,      -- human | script | ...
  created_at  TEXT NOT NULL,
  expires_at  REAL,               -- unix epoch seconds; NULL = no expiry
  PRIMARY KEY (peer, fingerprint)
);
";

/// Creates `_decisions` if it does not exist yet. Idempotent.
pub fn ensure_decisions_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(DECISIONS_DDL)
}

/// Records (or replaces) one remembered decision. `decision` is
/// `"allow"` or `"deny"`; `ttl_secs` bounds staleness (None = no expiry).
pub fn record_decision(
    conn: &Connection,
    peer: &str,
    fingerprint: &str,
    decision: &str,
    reason: Option<&str>,
    decided_by: &str,
    ttl_secs: Option<f64>,
) -> rusqlite::Result<()> {
    let now = unix_now();
    conn.execute(
        "INSERT INTO _decisions \
         (peer, fingerprint, decision, reason, decided_by, created_at, expires_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(peer, fingerprint) DO UPDATE SET \
           decision = ?3, reason = ?4, decided_by = ?5, created_at = ?6, expires_at = ?7",
        (
            peer,
            fingerprint,
            decision,
            reason,
            decided_by,
            crate::human::now_rfc3339(),
            ttl_secs.map(|t| now + t),
        ),
    )?;
    Ok(())
}

/// Drops every cached decision (e.g. after a `_policy` change).
pub fn invalidate_decisions(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM _decisions", [])?;
    Ok(())
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The decision-cache tier; registered in the chain as `"cache"`.
#[derive(Debug, Default)]
pub struct CacheTier;

impl Tier for CacheTier {
    fn name(&self) -> &str {
        "cache"
    }

    fn decide(
        &self,
        conn: &Connection,
        peer: &str,
        action: &str,
        footprint: &Footprint,
        statement: &str,
    ) -> Decision {
        // A statement the authorizer refused is never served from
        // memory; the policy tier states the denial.
        if footprint.has_denied() {
            return Decision::Escalate;
        }
        let fp = statement_fingerprint(action, statement, footprint);
        let row: Result<Option<(String, Option<String>)>, _> = conn
            .query_row(
                "SELECT decision, reason FROM _decisions \
                 WHERE peer = ?1 AND fingerprint = ?2 \
                   AND (expires_at IS NULL OR expires_at > ?3)",
                (peer, &fp, unix_now()),
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional();
        match row {
            Ok(Some((decision, reason))) => match decision.as_str() {
                "allow" => Decision::Allow,
                "deny" => Decision::Deny {
                    reason: reason.unwrap_or_else(|| "denied by remembered decision".into()),
                },
                other => {
                    warn!(decision = %other, "unrecognized _decisions row; escalating");
                    Decision::Escalate
                }
            },
            Ok(None) => Decision::Escalate,
            Err(err) => {
                // Missing table or query failure: abstain, never fail
                // open.
                warn!(error = %err, "cache tier query failed; escalating");
                Decision::Escalate
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ActionKind;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_decisions_table(&conn).unwrap();
        conn
    }

    fn fp() -> Footprint {
        Footprint::from_tables(ActionKind::Read, [("notes", vec!["id"])])
    }

    #[test]
    fn miss_escalates_and_hit_answers() {
        let conn = setup();
        let f = fp();
        let sql = "SELECT id FROM notes WHERE id = 7";
        assert_eq!(
            CacheTier.decide(&conn, "alice", "read", &f, sql),
            Decision::Escalate
        );
        let key = statement_fingerprint("read", sql, &f);
        record_decision(&conn, "alice", &key, "allow", None, "human", None).unwrap();
        // A parameter variant of the same shape hits.
        assert_eq!(
            CacheTier.decide(
                &conn,
                "alice",
                "read",
                &f,
                "SELECT id FROM notes WHERE id = 99"
            ),
            Decision::Allow
        );
        // Another peer misses.
        assert_eq!(
            CacheTier.decide(&conn, "bob", "read", &f, sql),
            Decision::Escalate
        );
    }

    #[test]
    fn deny_hits_carry_the_reason() {
        let conn = setup();
        let f = fp();
        let key = statement_fingerprint("read", "SELECT 1", &f);
        record_decision(
            &conn,
            "alice",
            &key,
            "deny",
            Some("owner said no"),
            "human",
            None,
        )
        .unwrap();
        match CacheTier.decide(&conn, "alice", "read", &f, "SELECT 1") {
            Decision::Deny { reason } => assert_eq!(reason, "owner said no"),
            other => panic!("expected deny, got {other:?}"),
        }
    }

    #[test]
    fn expired_rows_do_not_hit() {
        let conn = setup();
        let f = fp();
        let key = statement_fingerprint("read", "SELECT 1", &f);
        record_decision(&conn, "alice", &key, "allow", None, "human", Some(-10.0)).unwrap();
        assert_eq!(
            CacheTier.decide(&conn, "alice", "read", &f, "SELECT 1"),
            Decision::Escalate
        );
    }

    #[test]
    fn missing_table_escalates() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(
            CacheTier.decide(&conn, "alice", "read", &fp(), "SELECT 1"),
            Decision::Escalate
        );
    }

    #[test]
    fn denied_constructs_bypass_the_cache() {
        let conn = setup();
        let mut f = fp();
        f.deny_construct("attach");
        let key = statement_fingerprint("write", "ATTACH 'x' AS y", &f);
        record_decision(&conn, "alice", &key, "allow", None, "human", None).unwrap();
        assert_eq!(
            CacheTier.decide(&conn, "alice", "write", &f, "ATTACH 'x' AS y"),
            Decision::Escalate
        );
    }
}
