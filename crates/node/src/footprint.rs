//! Footprint collection and enforcement via `sqlite3_set_authorizer`
//! (rusqlite `Connection::authorizer`).
//!
//! During the decide prepare the authorizer runs in *collect* mode,
//! recording the ground-truth footprint (tables -> columns, statement
//! kind) while denying the categorically banned actions (`ATTACH`,
//! non-allowlisted `PRAGMA`, `load_extension`, DDL, client-issued
//! transaction control). During execution it is re-armed in *enforce*
//! mode: anything outside the approved footprint is denied, so the
//! decision and the execution cannot diverge even if the schema changed
//! in between (view swap).

use std::sync::{Arc, Mutex};

use resonator_authenticator::{ActionKind, Footprint};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

/// The sqlite function whose call is always denied.
const LOAD_EXTENSION: &str = "load_extension";

/// Which authority lane a statement runs under
/// (docs/owner-channel.md sec 2). The remote path carries the full
/// categorical ban set; on the owner channel DDL, any PRAGMA, and
/// transaction control are permitted, while ATTACH/DETACH and
/// load_extension stay banned (they reach beyond the served database).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lane {
    #[default]
    Remote,
    Owner,
}

/// What the collect pass learned about a statement.
#[derive(Debug, Default)]
pub struct CollectState {
    /// tables -> columns touched, plus denied construct names. `kind` is
    /// finalized by [`finish`](Self::finish).
    pub footprint: Footprint,
    /// The statement writes (INSERT/UPDATE/DELETE reached the authorizer).
    pub wrote: bool,
    /// The statement is an allowlisted PRAGMA.
    pub pragma: bool,
    /// Set when a banned action was denied during prepare; the recorded
    /// reason is what the requester and `_audit` see.
    pub denied: Option<String>,
}

impl CollectState {
    /// The statement kind implied by the collected actions. On the remote
    /// lane DDL, transaction control, and banned pragmas never get this
    /// far: they are denied during the collect prepare itself. On the
    /// owner lane DDL registers as a write through its `sqlite_master`
    /// touches.
    pub fn kind(&self) -> ActionKind {
        if self.wrote {
            ActionKind::Write
        } else if self.pragma {
            ActionKind::Other
        } else {
            ActionKind::Read
        }
    }

    /// The finalized footprint with `kind` stamped in.
    pub fn finish(mut self) -> Footprint {
        self.footprint.kind = self.kind();
        self.footprint
    }
}

/// Returns `(construct, reason)` for a categorically banned action, or
/// `None` when the action may proceed. Shared by both modes so collect and
/// enforce can never disagree about the bans. `lane` shrinks the set for
/// the owner channel: DDL, PRAGMA, and transaction control pass; ATTACH,
/// DETACH, and load_extension stay banned on every lane.
fn banned(
    action: &AuthAction<'_>,
    pragma_allowlist: &[String],
    lane: Lane,
) -> Option<(&'static str, String)> {
    match action {
        AuthAction::Attach { .. } | AuthAction::Detach { .. } => {
            Some(("attach", "ATTACH/DETACH is not allowed".into()))
        }
        AuthAction::Pragma { pragma_name, .. } => {
            if lane == Lane::Owner
                || pragma_allowlist
                    .iter()
                    .any(|p| p.eq_ignore_ascii_case(pragma_name))
            {
                None
            } else {
                Some(("pragma", format!("PRAGMA {pragma_name} is not allowlisted")))
            }
        }
        AuthAction::Function { function_name } => {
            if function_name.eq_ignore_ascii_case(LOAD_EXTENSION) {
                Some(("load_extension", "load_extension is not allowed".into()))
            } else {
                None
            }
        }
        AuthAction::Transaction { .. } | AuthAction::Savepoint { .. } => {
            if lane == Lane::Owner {
                None
            } else {
                Some((
                    "transaction",
                    "client-issued transaction control is not allowed".into(),
                ))
            }
        }
        AuthAction::CreateIndex { .. }
        | AuthAction::CreateTable { .. }
        | AuthAction::CreateTempIndex { .. }
        | AuthAction::CreateTempTable { .. }
        | AuthAction::CreateTempTrigger { .. }
        | AuthAction::CreateTempView { .. }
        | AuthAction::CreateTrigger { .. }
        | AuthAction::CreateView { .. }
        | AuthAction::DropIndex { .. }
        | AuthAction::DropTable { .. }
        | AuthAction::DropTempIndex { .. }
        | AuthAction::DropTempTable { .. }
        | AuthAction::DropTempTrigger { .. }
        | AuthAction::DropTempView { .. }
        | AuthAction::DropTrigger { .. }
        | AuthAction::DropView { .. }
        | AuthAction::AlterTable { .. }
        | AuthAction::Reindex { .. }
        | AuthAction::Analyze { .. }
        | AuthAction::CreateVtable { .. }
        | AuthAction::DropVtable { .. } => {
            if lane == Lane::Owner {
                None
            } else {
                Some(("ddl", "DDL is not allowed".into()))
            }
        }
        AuthAction::Unknown { code, .. } => Some((
            "unknown",
            format!("unknown sqlite action code {code} denied"),
        )),
        _ => None,
    }
}

/// Collect-mode authorizer callback: records the footprint, denies the
/// banned actions and records why.
pub fn collect_authorizer(
    state: Arc<Mutex<CollectState>>,
    pragma_allowlist: Arc<Vec<String>>,
    lane: Lane,
) -> impl for<'r> FnMut(AuthContext<'r>) -> Authorization + Send + 'static {
    move |ctx: AuthContext<'_>| {
        let mut st = match state.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some((construct, reason)) = banned(&ctx.action, &pragma_allowlist, lane) {
            st.footprint.deny_construct(construct);
            st.denied.get_or_insert(reason);
            return Authorization::Deny;
        }
        match ctx.action {
            AuthAction::Read {
                table_name,
                column_name,
            } => {
                st.footprint
                    .tables
                    .entry(table_name.to_owned())
                    .or_default()
                    .insert(column_name.to_owned());
            }
            AuthAction::Insert { table_name } | AuthAction::Delete { table_name } => {
                st.footprint
                    .tables
                    .entry(table_name.to_owned())
                    .or_default();
                st.wrote = true;
            }
            AuthAction::Update {
                table_name,
                column_name,
            } => {
                st.footprint
                    .tables
                    .entry(table_name.to_owned())
                    .or_default()
                    .insert(column_name.to_owned());
                st.wrote = true;
            }
            AuthAction::Pragma { .. } => st.pragma = true,
            _ => {}
        }
        Authorization::Allow
    }
}

/// The decision's approved shape, closed over by the enforce callback.
/// The footprint's `kind` is authoritative for what may write. `lane`
/// carries the ban set the statement was collected under, so enforcement
/// cannot re-ban what the owner channel permitted (or vice versa).
#[derive(Debug, Clone)]
pub struct Approved {
    pub footprint: Footprint,
    pub lane: Lane,
}

impl Approved {
    fn read_ok(&self, table: &str, column: &str) -> bool {
        self.footprint
            .tables
            .get(table)
            .is_some_and(|cols| cols.contains(column))
    }

    fn write_ok(&self, table: &str) -> bool {
        self.footprint.kind == ActionKind::Write && self.footprint.tables.contains_key(table)
    }
}

/// Enforce-mode authorizer callback: denies anything outside `approved`
/// (and the banned actions, unconditionally), recording the first deny
/// reason in `reason`.
pub fn enforce_authorizer(
    approved: Approved,
    pragma_allowlist: Arc<Vec<String>>,
    reason: Arc<Mutex<Option<String>>>,
) -> impl for<'r> FnMut(AuthContext<'r>) -> Authorization + Send + 'static {
    let record = move |msg: String| {
        let mut r = match reason.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        r.get_or_insert(msg);
    };
    let lane = approved.lane;
    move |ctx: AuthContext<'_>| {
        if let Some((_, msg)) = banned(&ctx.action, &pragma_allowlist, lane) {
            record(msg);
            return Authorization::Deny;
        }
        match ctx.action {
            AuthAction::Read {
                table_name,
                column_name,
            } => {
                if approved.read_ok(table_name, column_name) {
                    Authorization::Allow
                } else {
                    record(format!(
                        "read of {table_name}.{column_name} exceeds the approved footprint"
                    ));
                    Authorization::Deny
                }
            }
            AuthAction::Insert { table_name } | AuthAction::Delete { table_name } => {
                if approved.write_ok(table_name) {
                    Authorization::Allow
                } else {
                    record(format!(
                        "write to {table_name} exceeds the approved footprint"
                    ));
                    Authorization::Deny
                }
            }
            AuthAction::Update {
                table_name,
                column_name,
            } => {
                if approved.write_ok(table_name) && approved.read_ok(table_name, column_name) {
                    Authorization::Allow
                } else {
                    record(format!(
                        "update of {table_name}.{column_name} exceeds the approved footprint"
                    ));
                    Authorization::Deny
                }
            }
            // Allowlisted pragma (banned() above passed it) only legal when
            // the decision approved a pragma statement; the owner lane
            // permits any pragma.
            AuthAction::Pragma { pragma_name, .. } => {
                if lane == Lane::Owner || approved.footprint.kind == ActionKind::Other {
                    Authorization::Allow
                } else {
                    record(format!(
                        "PRAGMA {pragma_name} was not part of the approved statement"
                    ));
                    Authorization::Deny
                }
            }
            AuthAction::Select | AuthAction::Function { .. } | AuthAction::Recursive => {
                Authorization::Allow
            }
            // DDL and transaction control reach here only on the owner
            // lane (banned() denies them on the remote lane).
            other if lane == Lane::Owner => {
                let _ = other;
                Authorization::Allow
            }
            other => {
                record(format!("action {other:?} denied by the enforce authorizer"));
                Authorization::Deny
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn collect_lane(
        conn: &Connection,
        sql: &str,
        lane: Lane,
    ) -> (Result<(), String>, CollectState) {
        let state = Arc::new(Mutex::new(CollectState::default()));
        let allow: Arc<Vec<String>> = Arc::new(vec!["table_info".into()]);
        conn.authorizer(Some(collect_authorizer(state.clone(), allow, lane)))
            .unwrap();
        let prep = conn.prepare(sql).map(drop).map_err(|e| e.to_string());
        conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
            .unwrap();
        let st = Arc::try_unwrap(state).unwrap().into_inner().unwrap();
        (prep, st)
    }

    fn collect(conn: &Connection, sql: &str) -> (Result<(), String>, CollectState) {
        collect_lane(conn, sql, Lane::Remote)
    }

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT, secret TEXT);
             CREATE TABLE journal (id INTEGER PRIMARY KEY, entry TEXT);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn read_footprint_captures_tables_and_columns() {
        let conn = test_conn();
        let (prep, st) = collect(&conn, "SELECT id, body FROM notes");
        prep.unwrap();
        assert!(!st.wrote);
        let fp = st.finish();
        assert_eq!(fp.kind, ActionKind::Read);
        let cols = fp.tables.get("notes").unwrap();
        assert!(cols.contains("id") && cols.contains("body"));
        assert!(!cols.contains("secret"));
    }

    #[test]
    fn join_touches_both_tables() {
        let conn = test_conn();
        let (prep, st) = collect(
            &conn,
            "SELECT n.body, j.entry FROM notes n JOIN journal j ON n.id = j.id",
        );
        prep.unwrap();
        let fp = st.finish();
        assert!(fp.tables.contains_key("notes"));
        assert!(fp.tables.contains_key("journal"));
    }

    #[test]
    fn write_footprint_sets_kind() {
        let conn = test_conn();
        let (prep, st) = collect(&conn, "INSERT INTO notes (body) VALUES ('x')");
        prep.unwrap();
        assert!(st.wrote);
        assert_eq!(st.finish().kind, ActionKind::Write);

        let (prep, st) = collect(&conn, "UPDATE notes SET body = 'y' WHERE id = 1");
        prep.unwrap();
        let fp = st.finish();
        assert_eq!(fp.kind, ActionKind::Write);
        assert!(fp.tables.get("notes").unwrap().contains("body"));
    }

    #[test]
    fn bans_are_denied_and_recorded() {
        let conn = test_conn();
        for (sql, construct) in [
            ("ATTACH DATABASE ':memory:' AS other", "attach"),
            ("PRAGMA journal_mode = DELETE", "pragma"),
            ("CREATE TABLE evil (x)", "ddl"),
            ("DROP TABLE notes", "ddl"),
            ("BEGIN", "transaction"),
            ("SELECT load_extension('evil')", "load_extension"),
        ] {
            let (prep, st) = collect(&conn, sql);
            assert!(prep.is_err(), "{sql} should fail to prepare");
            assert!(st.denied.is_some(), "{sql} should record a deny reason");
            assert!(
                st.footprint.denied.contains(construct),
                "{sql} should record construct {construct}, got {:?}",
                st.footprint.denied
            );
        }
    }

    #[test]
    fn allowlisted_pragma_is_kind_other() {
        let conn = test_conn();
        let (prep, st) = collect(&conn, "PRAGMA table_info(notes)");
        prep.unwrap();
        assert!(st.pragma);
        assert_eq!(st.finish().kind, ActionKind::Other);
    }

    #[test]
    fn owner_lane_lifts_ddl_pragma_and_transaction_bans() {
        let conn = test_conn();
        for sql in [
            "CREATE TABLE fine (x)",
            "DROP TABLE journal",
            "PRAGMA journal_mode = DELETE",
        ] {
            let (prep, st) = collect_lane(&conn, sql, Lane::Owner);
            prep.unwrap_or_else(|e| panic!("{sql} should prepare on the owner lane: {e}"));
            assert!(st.denied.is_none(), "{sql} should not record a deny");
        }
        // The residual bans hold on the owner lane too.
        for (sql, construct) in [
            ("ATTACH DATABASE ':memory:' AS other", "attach"),
            ("SELECT load_extension('evil')", "load_extension"),
        ] {
            let (prep, st) = collect_lane(&conn, sql, Lane::Owner);
            assert!(prep.is_err(), "{sql} must stay banned for the owner");
            assert!(st.footprint.denied.contains(construct));
        }
    }

    #[test]
    fn owner_ddl_collects_as_write() {
        let conn = test_conn();
        let (prep, st) = collect_lane(&conn, "CREATE TABLE gadgets (id INTEGER)", Lane::Owner);
        prep.unwrap();
        assert!(st.wrote, "DDL registers as a write via sqlite_master");
        assert_eq!(st.finish().kind, ActionKind::Write);
    }

    #[test]
    fn enforce_denies_outside_footprint() {
        let conn = test_conn();
        let approved = Approved {
            footprint: Footprint::from_tables(ActionKind::Read, [("notes", vec!["id", "body"])]),
            lane: Lane::Remote,
        };
        let reason: Arc<Mutex<Option<String>>> = Arc::default();
        conn.authorizer(Some(enforce_authorizer(
            approved,
            Arc::new(Vec::new()),
            reason.clone(),
        )))
        .unwrap();
        // Within the footprint: fine.
        conn.prepare("SELECT id, body FROM notes").unwrap();
        // The secret column exceeds it.
        assert!(conn.prepare("SELECT secret FROM notes").is_err());
        // A write under a read approval is denied.
        assert!(
            conn.prepare("INSERT INTO notes (body) VALUES ('x')")
                .is_err()
        );
        conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
            .unwrap();
        assert!(
            reason
                .lock()
                .unwrap()
                .as_deref()
                .unwrap()
                .contains("exceeds")
        );
    }
}
