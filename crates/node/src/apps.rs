//! The app registry: which apps share this node and what each keeps in
//! it (docs/shared-node.md). Every process that can open the directory
//! is an owner; `_apps` is discovery, not permission.
//!
//! An app registers on open (one upsert; `last_seen` refreshed at most
//! every few minutes) and declares the tables it owns: a `_comments`
//! row per table (the console mandates them) plus the ownership record,
//! so `rsntr apps list`, the system graph (`rsntr:App`, `rsntr:Table`)
//! and any SPARQL over it answer "what is here, and whose is it".

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::clock::now_rfc3339;
use crate::error::NodeError;

/// The registry table.
pub const APPS_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _apps (
  app_id      TEXT PRIMARY KEY,           -- reverse-DNS id or a slug
  name        TEXT NOT NULL,
  version     TEXT,
  lib_version TEXT NOT NULL,              -- resonator crate version the app links
  platform    TEXT NOT NULL,              -- ios | android | macos | linux | windows
  tables      TEXT NOT NULL DEFAULT '[]', -- JSON array: tables this app declared
  first_seen  TEXT NOT NULL,
  last_seen   TEXT NOT NULL,
  note        TEXT
);
";

/// Who an app is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfo {
    /// `[A-Za-z0-9._-]{1,128}`, not starting with `_`; reverse-DNS by
    /// convention (`am.resonator.kontrolator`).
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

/// One table an app declares as its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableDecl {
    pub name: String,
    /// The `_comments` description (mandatory: a database a human can
    /// read starts with knowing what each table is for).
    pub comment: String,
}

/// One `_apps` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppRow {
    pub app_id: String,
    pub name: String,
    pub version: Option<String>,
    pub lib_version: String,
    pub platform: String,
    pub tables: Vec<String>,
    pub first_seen: String,
    pub last_seen: String,
    pub note: Option<String>,
}

/// Tables no app may claim: reserved (`_` prefix) and the scaffolds the
/// library owns.
const UNCLAIMABLE: &[&str] = &[
    "pages",
    "rdf_terms",
    "rdf_triples",
    "chat_messages",
    "chat_rooms",
    "chat_members",
    "chat_log_v",
];

/// Validates an app id.
pub fn validate_app_id(id: &str) -> Result<(), NodeError> {
    let ok = !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('_')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    if ok {
        Ok(())
    } else {
        Err(NodeError::Sparql(format!(
            "invalid app id {id:?}: 1..=128 of [A-Za-z0-9._-], not starting with '_'"
        )))
    }
}

/// The table-name prefix convention for an app: its last dotted label,
/// lowercased, non-alphanumerics folded to `_`, plus `_`.
pub fn slug(app_id: &str) -> String {
    let last = app_id.rsplit('.').next().unwrap_or(app_id);
    let mut s: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    s.push('_');
    s
}

/// The platform string recorded for this build.
pub fn platform() -> &'static str {
    std::env::consts::OS
}

/// The library version recorded for this build.
pub fn lib_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Upserts the app's row: `first_seen` is kept, everything else is
/// refreshed. Idempotent; one statement (safe from any connection).
pub fn register_app(conn: &Connection, app: &AppInfo) -> Result<(), NodeError> {
    validate_app_id(&app.id)?;
    let now = now_rfc3339();
    conn.execute(
        "INSERT INTO _apps (app_id, name, version, lib_version, platform, first_seen, last_seen) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) \
         ON CONFLICT(app_id) DO UPDATE SET name = excluded.name, version = excluded.version, \
           lib_version = excluded.lib_version, platform = excluded.platform, \
           last_seen = excluded.last_seen",
        (
            &app.id,
            app.name.as_deref().unwrap_or(&app.id),
            &app.version,
            lib_version(),
            platform(),
            &now,
        ),
    )?;
    Ok(())
}

/// Refreshes `last_seen` when it is older than `max_age_secs` (cheap:
/// one read, maybe one write). Returns whether it wrote.
pub fn touch_app(conn: &Connection, app_id: &str, max_age_secs: i64) -> Result<bool, NodeError> {
    let stale: Option<i64> = conn
        .query_row(
            "SELECT CAST((julianday('now') - julianday(last_seen)) * 86400 AS INTEGER) \
             FROM _apps WHERE app_id = ?1",
            [app_id],
            |r| r.get(0),
        )
        .optional()?;
    match stale {
        Some(age) if age >= max_age_secs => {
            conn.execute(
                "UPDATE _apps SET last_seen = ?2 WHERE app_id = ?1",
                (app_id, now_rfc3339()),
            )?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Every registered app.
pub fn list_apps(conn: &Connection) -> Result<Vec<AppRow>, NodeError> {
    let mut stmt = conn.prepare(
        "SELECT app_id, name, version, lib_version, platform, tables, first_seen, last_seen, note \
         FROM _apps ORDER BY app_id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let tables: String = r.get(5)?;
            Ok(AppRow {
                app_id: r.get(0)?,
                name: r.get(1)?,
                version: r.get(2)?,
                lib_version: r.get(3)?,
                platform: r.get(4)?,
                tables: serde_json::from_str(&tables).unwrap_or_default(),
                first_seen: r.get(6)?,
                last_seen: r.get(7)?,
                note: r.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The app that declared `table`, if any.
pub fn claimed_by(conn: &Connection, table: &str) -> Result<Option<String>, NodeError> {
    let mut stmt = conn.prepare("SELECT app_id, tables FROM _apps")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (app_id, tables) = row?;
        let tables: Vec<String> = serde_json::from_str(&tables).unwrap_or_default();
        if tables.iter().any(|t| t == table) {
            return Ok(Some(app_id));
        }
    }
    Ok(None)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, NodeError> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type IN ('table', 'view') AND name = ?1",
        [table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Validates a declaration and returns the statements that apply it,
/// in order: one `_comments` insert per table (INSERT OR IGNORE: an
/// owner's rewording is never overwritten), then the merged
/// `_apps.tables` update. Callers run them on their owner channel;
/// [`declare_tables`] runs them directly. Rules: the table must exist
/// (create it first), must not be reserved or a library scaffold, and
/// must not be claimed by another app; a name outside the
/// `<slug>_` convention only warns.
pub fn declare_statements(
    conn: &Connection,
    app_id: &str,
    tables: &[TableDecl],
) -> Result<Vec<(String, Vec<String>)>, NodeError> {
    validate_app_id(app_id)?;
    let prefix = slug(app_id);
    let mut owned: Vec<String> = conn
        .query_row(
            "SELECT tables FROM _apps WHERE app_id = ?1",
            [app_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let mut statements = Vec::new();
    for decl in tables {
        let name = decl.name.as_str();
        if name.starts_with('_') || UNCLAIMABLE.contains(&name) {
            return Err(NodeError::Sparql(format!(
                "table {name:?} is reserved; apps declare their own tables"
            )));
        }
        if !table_exists(conn, name)? {
            return Err(NodeError::Sparql(format!(
                "table {name:?} does not exist; create it before declaring it"
            )));
        }
        if let Some(other) = claimed_by(conn, name)?
            && other != app_id
        {
            return Err(NodeError::Sparql(format!(
                "table {name:?} is already declared by app {other}"
            )));
        }
        if decl.comment.trim().is_empty() {
            return Err(NodeError::Sparql(format!(
                "table {name:?} needs a comment: say what it is for"
            )));
        }
        if !name.starts_with(&prefix) {
            tracing::warn!(
                table = name,
                app = app_id,
                "declared table does not carry the app's prefix {prefix:?} (convention)"
            );
        }
        statements.push((
            "INSERT OR IGNORE INTO _comments (name, comment) VALUES (?1, ?2)".to_string(),
            vec![name.to_string(), decl.comment.trim().to_string()],
        ));
        if !owned.iter().any(|t| t == name) {
            owned.push(name.to_string());
        }
    }
    owned.sort();
    statements.push((
        "UPDATE _apps SET tables = ?2 WHERE app_id = ?1".to_string(),
        vec![
            app_id.to_string(),
            serde_json::to_string(&owned).unwrap_or_else(|_| "[]".into()),
        ],
    ));
    Ok(statements)
}

/// [`declare_statements`] applied on `conn` in one immediate
/// transaction; the app must be registered first.
pub fn declare_tables(
    conn: &mut Connection,
    app_id: &str,
    tables: &[TableDecl],
) -> Result<(), NodeError> {
    let registered: bool = conn
        .query_row(
            "SELECT 1 FROM _apps WHERE app_id = ?1",
            [app_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !registered {
        return Err(NodeError::Sparql(format!(
            "app {app_id} is not registered; open the node with its AppInfo first"
        )));
    }
    let statements = declare_statements(conn, app_id, tables)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for (sql, params) in &statements {
        tx.execute(sql, rusqlite::params_from_iter(params.iter()))?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddl::ensure_node_tables;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_node_tables(&conn).unwrap();
        conn.execute_batch(
            "CREATE TABLE notebook_pages (id INTEGER PRIMARY KEY, body TEXT); \
             CREATE TABLE other_stuff (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn ids_and_slugs() {
        assert!(validate_app_id("org.example.notebook").is_ok());
        assert!(validate_app_id("_hidden").is_err());
        assert!(validate_app_id("").is_err());
        assert!(validate_app_id("has space").is_err());
        assert_eq!(slug("org.example.Note-Book"), "note_book_");
        assert_eq!(slug("kontrolator"), "kontrolator_");
    }

    #[test]
    fn register_keeps_first_seen_and_touch_is_lazy() {
        let conn = db();
        let app = AppInfo {
            id: "org.example.notebook".into(),
            name: Some("Notebook".into()),
            version: Some("1.0".into()),
        };
        register_app(&conn, &app).unwrap();
        let first = list_apps(&conn).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].name, "Notebook");
        assert_eq!(first[0].platform, std::env::consts::OS);
        let again = AppInfo {
            version: Some("1.1".into()),
            ..app.clone()
        };
        register_app(&conn, &again).unwrap();
        let second = list_apps(&conn).unwrap();
        assert_eq!(second[0].first_seen, first[0].first_seen);
        assert_eq!(second[0].version.as_deref(), Some("1.1"));
        assert!(!touch_app(&conn, &app.id, 300).unwrap(), "fresh: no write");
        assert!(touch_app(&conn, &app.id, 0).unwrap(), "stale: refreshed");
        assert!(!touch_app(&conn, "nobody", 0).unwrap());
    }

    #[test]
    fn declarations_validate_and_merge() {
        let mut conn = db();
        let app = AppInfo {
            id: "org.example.notebook".into(),
            name: None,
            version: None,
        };
        let decl = |n: &str| TableDecl {
            name: n.into(),
            comment: format!("{n}: test"),
        };
        assert!(
            declare_tables(&mut conn, &app.id, &[decl("notebook_pages")]).is_err(),
            "unregistered app"
        );
        register_app(&conn, &app).unwrap();
        declare_tables(&mut conn, &app.id, &[decl("notebook_pages")]).unwrap();
        // Off-convention name: allowed with a warning; merged, not replaced.
        declare_tables(&mut conn, &app.id, &[decl("other_stuff")]).unwrap();
        let rows = list_apps(&conn).unwrap();
        assert_eq!(rows[0].tables, vec!["notebook_pages", "other_stuff"]);
        let comment: String = conn
            .query_row(
                "SELECT comment FROM _comments WHERE name = 'notebook_pages'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(comment, "notebook_pages: test");
        assert_eq!(
            claimed_by(&conn, "other_stuff").unwrap().as_deref(),
            Some("org.example.notebook")
        );
        for bad in ["_peers", "pages", "chat_messages", "missing_table"] {
            assert!(
                declare_statements(&conn, &app.id, &[decl(bad)]).is_err(),
                "{bad} must be refused"
            );
        }
        let other = AppInfo {
            id: "org.example.other".into(),
            name: None,
            version: None,
        };
        register_app(&conn, &other).unwrap();
        let err = declare_statements(&conn, &other.id, &[decl("notebook_pages")])
            .expect_err("claimed elsewhere");
        assert!(err.to_string().contains("org.example.notebook"));
        assert!(
            declare_statements(
                &conn,
                &app.id,
                &[TableDecl {
                    name: "notebook_pages".into(),
                    comment: "  ".into()
                }]
            )
            .is_err(),
            "a comment is mandatory"
        );
    }
}
