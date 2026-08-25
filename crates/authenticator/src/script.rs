//! The `"script"` tier: owner-authored Rhai scripts from the `_scripts`
//! table. This crate owns the `_scripts` DDL.
//!
//! Frozen script API (v1):
//!
//! - constants: `peer` (string), `action` (string), `statement`
//!   (string), `footprint` (map of table name -> array of column names);
//! - the script's value must be one of `allow()`, `deny("reason")`, or
//!   `escalate()`.
//!
//! Enabled scripts run in name order; the first non-escalate value wins.
//! A script that errors, panics, times out on the instruction budget, or
//! returns anything else counts as Escalate: the tier never fails open.
//!
//! The tier itself is behind the `script` feature; the `_scripts` DDL is
//! not. A database is the same shape either way, so one written by a
//! build that can run scripts still opens in a build that cannot — it
//! simply never consults the table.

#[cfg(feature = "script")]
use rhai::{Dynamic, Engine, Scope};
use rusqlite::Connection;
#[cfg(feature = "script")]
use tracing::warn;

#[cfg(feature = "script")]
use crate::chain::Tier;
#[cfg(feature = "script")]
use crate::types::{Decision, Footprint};

/// Owner-authored decision scripts; `body` is Rhai source.
pub const SCRIPTS_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _scripts (
  name     TEXT PRIMARY KEY,
  body     TEXT NOT NULL,
  enabled  INTEGER NOT NULL DEFAULT 1
);
";

/// Creates `_scripts` if it does not exist yet. Idempotent.
pub fn ensure_scripts_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCRIPTS_DDL)
}

/// The scripted tier; registered in the chain as `"script"`.
#[cfg(feature = "script")]
#[derive(Debug, Clone)]
pub struct ScriptTier {
    /// Rhai operations one evaluation may spend before it is aborted
    /// (and counts as Escalate).
    max_operations: u64,
}

/// Generous for policy logic, far below anything that could stall the
/// db thread noticeably.
#[cfg(feature = "script")]
const DEFAULT_MAX_OPERATIONS: u64 = 100_000;

#[cfg(feature = "script")]
impl Default for ScriptTier {
    fn default() -> Self {
        Self {
            max_operations: DEFAULT_MAX_OPERATIONS,
        }
    }
}

#[cfg(feature = "script")]
impl ScriptTier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the per-evaluation instruction budget.
    pub fn with_max_operations(max_operations: u64) -> Self {
        Self { max_operations }
    }
}

/// What a script hands back through `allow()` / `deny(r)` / `escalate()`.
#[cfg(feature = "script")]
#[derive(Debug, Clone)]
enum Verdict {
    Allow,
    Deny(String),
    Escalate,
}

#[cfg(feature = "script")]
fn build_engine(max_operations: u64) -> Engine {
    let mut engine = Engine::new();
    engine.set_max_operations(max_operations);
    engine
        .register_type_with_name::<Verdict>("Verdict")
        .register_fn("allow", || Verdict::Allow)
        .register_fn("deny", |reason: &str| Verdict::Deny(reason.to_string()))
        .register_fn("escalate", || Verdict::Escalate);
    engine
}

#[cfg(feature = "script")]
fn footprint_map(footprint: &Footprint) -> rhai::Map {
    let mut map = rhai::Map::new();
    for (table, cols) in &footprint.tables {
        let arr: rhai::Array = cols.iter().map(|c| Dynamic::from(c.clone())).collect();
        map.insert(table.as_str().into(), Dynamic::from(arr));
    }
    map
}

#[cfg(feature = "script")]
impl Tier for ScriptTier {
    fn name(&self) -> &str {
        "script"
    }

    fn decide(
        &self,
        conn: &Connection,
        peer: &str,
        action: &str,
        footprint: &Footprint,
        statement: &str,
    ) -> Decision {
        let scripts: Vec<(String, String)> = {
            let mut stmt = match conn
                .prepare_cached("SELECT name, body FROM _scripts WHERE enabled = 1 ORDER BY name")
            {
                Ok(s) => s,
                Err(err) => {
                    warn!(error = %err, "script tier query failed; escalating");
                    return Decision::Escalate;
                }
            };
            match stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .and_then(|rows| rows.collect())
            {
                Ok(rows) => rows,
                Err(err) => {
                    warn!(error = %err, "script tier rows failed; escalating");
                    return Decision::Escalate;
                }
            }
        };
        if scripts.is_empty() {
            return Decision::Escalate;
        }

        let engine = build_engine(self.max_operations);
        for (name, body) in scripts {
            let mut scope = Scope::new();
            scope.push_constant("peer", peer.to_string());
            scope.push_constant("action", action.to_string());
            scope.push_constant("statement", statement.to_string());
            scope.push_constant("footprint", footprint_map(footprint));
            let value = match engine.eval_with_scope::<Dynamic>(&mut scope, &body) {
                Ok(v) => v,
                Err(err) => {
                    warn!(script = %name, error = %err, "script failed; counts as escalate");
                    continue;
                }
            };
            match value.try_cast::<Verdict>() {
                Some(Verdict::Allow) => return Decision::Allow,
                Some(Verdict::Deny(reason)) => {
                    return Decision::Deny {
                        reason: format!("script {name}: {reason}"),
                    };
                }
                Some(Verdict::Escalate) => continue,
                None => {
                    warn!(
                        script = %name,
                        "script returned a non-verdict value; counts as escalate"
                    );
                    continue;
                }
            }
        }
        Decision::Escalate
    }
}

#[cfg(all(test, feature = "script"))]
mod tests {
    use super::*;
    use crate::types::ActionKind;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_scripts_table(&conn).unwrap();
        conn
    }

    fn add_script(conn: &Connection, name: &str, body: &str) {
        conn.execute(
            "INSERT OR REPLACE INTO _scripts (name, body, enabled) VALUES (?1, ?2, 1)",
            (name, body),
        )
        .unwrap();
    }

    fn fp() -> Footprint {
        Footprint::from_tables(ActionKind::Read, [("notes", vec!["id", "body"])])
    }

    fn decide(conn: &Connection, peer: &str, action: &str, statement: &str) -> Decision {
        ScriptTier::default().decide(conn, peer, action, &fp(), statement)
    }

    #[test]
    fn allow_deny_escalate() {
        let conn = setup();
        add_script(
            &conn,
            "gate",
            r#"
            if peer == "alice" && action == "read" { allow() }
            else if action == "write" { deny("writes are scripted off") }
            else { escalate() }
            "#,
        );
        assert_eq!(decide(&conn, "alice", "read", "SELECT 1"), Decision::Allow);
        match decide(&conn, "bob", "write", "INSERT INTO notes VALUES (1)") {
            Decision::Deny { reason } => {
                assert!(reason.contains("writes are scripted off"), "{reason}");
            }
            other => panic!("expected deny, got {other:?}"),
        }
        assert_eq!(decide(&conn, "bob", "read", "SELECT 1"), Decision::Escalate);
    }

    #[test]
    fn scripts_see_footprint_and_statement() {
        let conn = setup();
        add_script(
            &conn,
            "fp",
            r#"
            if "notes" in footprint && footprint.notes.len() == 2
               && statement.contains("SELECT") { allow() } else { escalate() }
            "#,
        );
        assert_eq!(
            decide(&conn, "alice", "read", "SELECT id, body FROM notes"),
            Decision::Allow
        );
        assert_eq!(decide(&conn, "alice", "read", "nope"), Decision::Escalate);
    }

    #[test]
    fn errors_fail_closed_as_escalate() {
        let conn = setup();
        add_script(&conn, "boom", r#"throw "kaput";"#);
        assert_eq!(
            decide(&conn, "alice", "read", "SELECT 1"),
            Decision::Escalate
        );
        // A syntactically broken script escalates too.
        add_script(&conn, "broken", "if (((");
        assert_eq!(
            decide(&conn, "alice", "read", "SELECT 1"),
            Decision::Escalate
        );
        // A script returning a non-verdict value escalates.
        add_script(&conn, "wrongtype", "42");
        assert_eq!(
            decide(&conn, "alice", "read", "SELECT 1"),
            Decision::Escalate
        );
    }

    #[test]
    fn instruction_budget_aborts_runaway_scripts() {
        let conn = setup();
        add_script(&conn, "spin", "let x = 0; loop { x += 1; } allow()");
        let tier = ScriptTier::with_max_operations(10_000);
        assert_eq!(
            tier.decide(&conn, "alice", "read", &fp(), "SELECT 1"),
            Decision::Escalate
        );
    }

    #[test]
    fn scripts_run_in_name_order_first_verdict_wins() {
        let conn = setup();
        add_script(&conn, "b-allow", "allow()");
        add_script(&conn, "a-deny", r#"deny("first")"#);
        match decide(&conn, "alice", "read", "SELECT 1") {
            Decision::Deny { reason } => assert!(reason.contains("a-deny")),
            other => panic!("expected deny, got {other:?}"),
        }
        // Disabled scripts do not run.
        conn.execute("UPDATE _scripts SET enabled = 0 WHERE name = 'a-deny'", [])
            .unwrap();
        assert_eq!(decide(&conn, "alice", "read", "SELECT 1"), Decision::Allow);
    }

    #[test]
    fn no_scripts_or_missing_table_escalates() {
        let conn = setup();
        assert_eq!(
            decide(&conn, "alice", "read", "SELECT 1"),
            Decision::Escalate
        );
        let bare = Connection::open_in_memory().unwrap();
        assert_eq!(
            ScriptTier::default().decide(&bare, "alice", "read", &fp(), "SELECT 1"),
            Decision::Escalate
        );
    }
}
