//! The `_policy` table tier: `(peer_or_group, table_name, action,
//! effect, note)` rows, evaluated most-specific-first with `*`
//! wildcards. This crate owns the `_policy` DDL.
//!
//! Specificity: peer+table beats peer+`*` beats `*`+table beats
//! `*`+`*`; at equal specificity deny wins. For statement requests
//! every footprint table must resolve to `allow`; one `deny` denies
//! the whole request, and a table with no matching row makes the tier
//! abstain (Escalate) so a later tier can decide. Requests with an
//! empty footprint (knock, media, entrain) are evaluated against the
//! single wildcard resource, matched only by `table_name = '*'` rows.
//! A footprint carrying authorizer-denied constructs is denied
//! outright.

use rusqlite::Connection;
use tracing::warn;

use crate::chain::Tier;
use crate::types::{Decision, Footprint};

/// Policy rules; `action` is an open string set (read | write | knock |
/// media | entrain | mod:<name> | ...), `effect` is allow | deny.
pub const POLICY_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _policy (
  peer_or_group TEXT NOT NULL,      -- EndpointId text, group name, or '*'
  table_name    TEXT NOT NULL,      -- table, or '*'
  action        TEXT NOT NULL,      -- read | write | knock | media | entrain
  effect        TEXT NOT NULL,      -- allow | deny
  note          TEXT
);
";

/// Creates `_policy` if it does not exist yet. Idempotent.
pub fn ensure_policy_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(POLICY_DDL)
}

/// The table-driven policy tier; registered in the chain as `"policy"`.
#[derive(Debug, Default)]
pub struct PolicyTier;

/// Per-resource resolution.
enum Resolved {
    Allow,
    Deny,
    Unmatched,
}

impl PolicyTier {
    /// Most-specific-first lookup for one resource (table name or `"*"`).
    fn resolve(
        &self,
        conn: &Connection,
        peer: &str,
        table: &str,
        action: &str,
    ) -> Result<Resolved, rusqlite::Error> {
        let mut stmt = conn.prepare_cached(
            "SELECT peer_or_group, table_name, effect FROM _policy \
             WHERE action = ?1 \
               AND (peer_or_group = ?2 OR peer_or_group = '*') \
               AND (table_name = ?3 OR table_name = '*')",
        )?;
        let rows = stmt.query_map((action, peer, table), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        // score: peer match outweighs table match, so peer+table (3) >
        // peer+* (2) > *+table (1) > *+* (0).
        let mut best: Option<(u8, bool)> = None; // (score, is_allow)
        for row in rows {
            let (p, t, effect) = row?;
            let score = u8::from(p != "*") * 2 + u8::from(t != "*");
            let is_allow = effect == "allow";
            best = match best {
                None => Some((score, is_allow)),
                Some((s, _)) if score > s => Some((score, is_allow)),
                // same specificity: deny wins
                Some((s, a)) if score == s => Some((s, a && is_allow)),
                Some(kept) => Some(kept),
            };
        }
        Ok(match best {
            None => Resolved::Unmatched,
            Some((_, true)) => Resolved::Allow,
            Some((_, false)) => Resolved::Deny,
        })
    }
}

impl Tier for PolicyTier {
    fn name(&self) -> &str {
        "policy"
    }

    fn decide(
        &self,
        conn: &Connection,
        peer: &str,
        action: &str,
        footprint: &Footprint,
        _statement: &str,
    ) -> Decision {
        if footprint.has_denied() {
            let constructs: Vec<&str> = footprint.denied.iter().map(String::as_str).collect();
            return Decision::Deny {
                reason: format!(
                    "policy: statement uses denied constructs: {}",
                    constructs.join(", ")
                ),
            };
        }

        // Footprint-less requests (knock, media, entrain) are one
        // wildcard resource.
        let wildcard = ["*".to_string()];
        let resources: Vec<&str> = if footprint.is_empty() {
            wildcard.iter().map(String::as_str).collect()
        } else {
            footprint.tables.keys().map(String::as_str).collect()
        };

        let mut unmatched = false;
        for table in resources {
            match self.resolve(conn, peer, table, action) {
                Ok(Resolved::Allow) => {}
                Ok(Resolved::Deny) => {
                    return Decision::Deny {
                        reason: format!("policy: {action} on {table} denied for {peer}"),
                    };
                }
                Ok(Resolved::Unmatched) => unmatched = true,
                Err(err) => {
                    // Missing table or query failure: abstain, never
                    // fail open.
                    warn!(error = %err, "policy tier query failed; escalating");
                    return Decision::Escalate;
                }
            }
        }
        if unmatched {
            Decision::Escalate
        } else {
            Decision::Allow
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ActionKind;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_policy_table(&conn).unwrap();
        conn
    }

    fn add_rule(conn: &Connection, peer: &str, table: &str, action: &str, effect: &str) {
        conn.execute(
            "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
             VALUES (?1, ?2, ?3, ?4)",
            (peer, table, action, effect),
        )
        .unwrap();
    }

    fn fp(tables: &[&str]) -> Footprint {
        Footprint::from_tables(
            ActionKind::Read,
            tables.iter().map(|t| (*t, Vec::<String>::new())),
        )
    }

    fn decide(conn: &Connection, peer: &str, action: &str, f: &Footprint) -> Decision {
        PolicyTier.decide(conn, peer, action, f, "")
    }

    #[test]
    fn ensure_policy_table_is_idempotent() {
        let conn = setup();
        ensure_policy_table(&conn).unwrap();
        add_rule(&conn, "*", "*", "read", "allow");
        // Re-running must not drop existing rows.
        ensure_policy_table(&conn).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM _policy", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn wildcard_precedence_most_specific_wins() {
        let conn = setup();
        add_rule(&conn, "*", "*", "read", "allow");
        add_rule(&conn, "*", "secrets", "read", "deny");
        add_rule(&conn, "alice", "*", "read", "allow");
        add_rule(&conn, "alice", "secrets", "read", "deny");

        // peer+table (deny) beats peer+* (allow).
        assert!(matches!(
            decide(&conn, "alice", "read", &fp(&["secrets"])),
            Decision::Deny { .. }
        ));
        // peer+* (allow) beats *+table (deny) for another table.
        add_rule(&conn, "*", "notes", "read", "deny");
        assert_eq!(
            decide(&conn, "alice", "read", &fp(&["notes"])),
            Decision::Allow
        );
        // For a stranger, *+table (deny) beats *+* (allow).
        assert!(matches!(
            decide(&conn, "bob", "read", &fp(&["notes"])),
            Decision::Deny { .. }
        ));
        // Stranger on an unconstrained table falls to *+* allow.
        assert_eq!(
            decide(&conn, "bob", "read", &fp(&["public"])),
            Decision::Allow
        );
    }

    #[test]
    fn same_specificity_deny_wins() {
        let conn = setup();
        add_rule(&conn, "alice", "notes", "read", "allow");
        add_rule(&conn, "alice", "notes", "read", "deny");
        assert!(matches!(
            decide(&conn, "alice", "read", &fp(&["notes"])),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn unmatched_table_escalates() {
        let conn = setup();
        add_rule(&conn, "alice", "notes", "read", "allow");
        // "notes" allowed but "journal" has no rule: abstain.
        assert_eq!(
            decide(&conn, "alice", "read", &fp(&["notes", "journal"])),
            Decision::Escalate
        );
        // No rules at all for this peer/action either.
        assert_eq!(
            decide(&conn, "bob", "write", &fp(&["notes"])),
            Decision::Escalate
        );
    }

    #[test]
    fn one_denied_table_denies_the_request() {
        let conn = setup();
        add_rule(&conn, "alice", "notes", "read", "allow");
        add_rule(&conn, "alice", "journal", "read", "deny");
        assert!(matches!(
            decide(&conn, "alice", "read", &fp(&["notes", "journal"])),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn empty_footprint_matches_wildcard_rows_only() {
        let conn = setup();
        add_rule(&conn, "alice", "notes", "knock", "allow");
        // A table-specific row does not cover the wildcard resource.
        assert_eq!(
            decide(&conn, "alice", "knock", &fp(&[])),
            Decision::Escalate
        );
        add_rule(&conn, "alice", "*", "knock", "allow");
        assert_eq!(decide(&conn, "alice", "knock", &fp(&[])), Decision::Allow);
    }

    #[test]
    fn string_actions_gate_mods_and_media() {
        let conn = setup();
        add_rule(&conn, "alice", "*", "mod:time", "allow");
        add_rule(&conn, "*", "*", "media", "deny");
        assert_eq!(
            decide(&conn, "alice", "mod:time", &fp(&[])),
            Decision::Allow
        );
        assert!(matches!(
            decide(&conn, "alice", "media", &fp(&[])),
            Decision::Deny { .. }
        ));
        // Unknown action: no rows, abstain.
        assert_eq!(
            decide(&conn, "alice", "entrain", &fp(&[])),
            Decision::Escalate
        );
    }

    #[test]
    fn authorizer_denied_constructs_deny_outright() {
        let conn = setup();
        add_rule(&conn, "*", "*", "write", "allow");
        let mut f = fp(&["notes"]);
        f.kind = ActionKind::Other;
        f.deny_construct("attach");
        let d = decide(&conn, "alice", "write", &f);
        match d {
            Decision::Deny { reason } => assert!(reason.contains("attach")),
            other => panic!("expected deny, got {other:?}"),
        }
    }

    #[test]
    fn missing_policy_table_escalates() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(
            decide(&conn, "alice", "read", &fp(&["notes"])),
            Decision::Escalate
        );
    }
}
