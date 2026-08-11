//! DDL for the reserved node tables and the `_rsntr` metadata helpers.
//!
//! The node crate owns `_rsntr`, `_peers`, `_audit`, `_inbox`, `_applied`,
//! `_knock_budget`, `_media`, and `_projection`. `_policy` comes from the
//! authenticator crate; `_outbox`/`_results` belong to the surfaces crate.
//! The SPARQL store schema (`rdf_terms`, `rdf_triples`) is ensured here too
//! so the `sparql` modulation and the local `rdf_*` SQL functions always
//! have their tables.

use rusqlite::{Connection, OptionalExtension};

use resonator_protocol::Hello;
use resonator_transport::ENVELOPE_VERSION;

use crate::error::NodeError;

/// Peer registry: peers are rows, not config.
pub const PEERS_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _peers (
  endpoint_id  TEXT PRIMARY KEY,   -- 64-hex ed25519 public key
  name         TEXT,               -- local petname, no global meaning
  relay_url    TEXT,               -- optional last-known relay
  addrs        TEXT,               -- optional JSON list of direct socket addrs
  added_at     TEXT NOT NULL,
  last_seen    TEXT,
  notes        TEXT
);
";

/// Node metadata key-value table. Keys: endpoint_id, envelope_version,
/// modulations, encodings, auth_chain, auth_masks, help_text, hint.
pub const RSNTR_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _rsntr (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
";

/// The unconditional decision ledger. One row per request outcome;
/// `signal` holds the statement or modulation signal text.
pub const AUDIT_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _audit (
  id          INTEGER PRIMARY KEY,
  at          TEXT NOT NULL,
  direction   TEXT NOT NULL DEFAULT 'in',  -- in | out | local (owner channel)
  peer        TEXT NOT NULL,
  request_id  TEXT NOT NULL,
  signal      TEXT NOT NULL,
  footprint   TEXT NOT NULL,       -- JSON
  decision    TEXT NOT NULL,       -- allow | allow_narrowed | deny
  decided_by  TEXT NOT NULL,       -- policy | ... | peer-gate | node | default | owner
  reason      TEXT,
  rows_out    INTEGER,
  bytes_out   INTEGER,
  duration_ms INTEGER
);
";

/// Applied-write ledger: `request_id` is the idempotency key; a retransmit
/// of a recorded id is answered from `outcome` instead of applied twice.
pub const APPLIED_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _applied (
  request_id  TEXT PRIMARY KEY,    -- ULID, from the request envelope
  at          TEXT NOT NULL,
  outcome     TEXT NOT NULL        -- JSON {affected_rows, last_insert_rowid}
);
";

/// Incoming requests awaiting a human decision. A stranger's knock parks
/// here (empty `sql`, the knock text in `params`) when no automated tier
/// decides; the owner answers with `UPDATE _inbox SET decision = ...`.
pub const INBOX_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _inbox (
  request_id   TEXT PRIMARY KEY,
  peer         TEXT NOT NULL,
  sql          TEXT NOT NULL,
  params       TEXT NOT NULL,
  decision     TEXT,               -- NULL until owner sets allow/deny
  decided_by   TEXT,
  received_at  TEXT NOT NULL
);
";

/// Persisted token buckets for the knock rate limits. One row per bucket:
/// the knocking peer's endpoint id, and `'*'` for the shared global bucket.
pub const KNOCK_BUDGET_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _knock_budget (
  bucket      TEXT PRIMARY KEY,   -- peer endpoint_id, or '*' for global
  tokens      REAL NOT NULL,
  updated_at  REAL NOT NULL       -- unix epoch seconds of the last refill
);
";

/// Media sources the `media` modulation can serve. The request's
/// `rsntr:signal` names a row; the row's `command` runs via `sh -c` and its
/// stdout is the raw byte feed behind the `rsntr:Media` header. Access is
/// gated by `_policy` rows with `action = 'media'` whose `table_name` holds
/// the source name (or `'*'`).
///
/// A row whose `accepts` is non-NULL is additionally an audio-duplex
/// source: the `audio-duplex` modulation runs the same command with stdin
/// piped, `accepts` names the media type the command reads there, and the
/// open is gated by `_policy` action `audio-duplex` on the source name.
pub const MEDIA_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _media (
  name          TEXT PRIMARY KEY,
  command       TEXT NOT NULL,
  content_type  TEXT NOT NULL DEFAULT 'video/mp2t',
  accepts       TEXT,
  note          TEXT
);
";

/// Adds `_media.accepts` to databases created before audio-duplex.
/// Idempotent; the first (and so far only) column migration.
fn ensure_media_accepts_column(conn: &Connection) -> Result<(), NodeError> {
    let has: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_table_info('_media') WHERE name = 'accepts'",
        [],
        |r| r.get(0),
    )?;
    if has == 0 {
        conn.execute_batch("ALTER TABLE _media ADD COLUMN accepts TEXT")?;
    }
    Ok(())
}

/// Owner-authored resonance points served by the `projection` modulation.
/// `resource` is the policy gate table: NULL means the point is public;
/// otherwise the point is emitted only when `_policy` allows the caller the
/// point's action on that table, which keeps "the projection never lies"
/// true. `fields` is a JSON array of `{name, datatype, required, default,
/// one_of, hint}` objects; `params_order` a JSON array of field names.
pub const PROJECTION_TABLE_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _projection (
  point_iri       TEXT PRIMARY KEY,  -- stable IRI, minted by this node
  kind            TEXT NOT NULL,     -- excitable | radiant | sympathetic | bare | <IRI>
  label           TEXT,
  icon            TEXT,
  role            TEXT,              -- default | destructive
  path            TEXT NOT NULL DEFAULT '',
  ord             INTEGER NOT NULL DEFAULT 0,
  projects_path   TEXT,
  modulation      TEXT,
  signal          TEXT,
  params_order    TEXT,
  fields          TEXT,
  signal_template TEXT,
  fires           TEXT,
  resource        TEXT,              -- policy gate table, NULL = public
  note            TEXT
);
";

/// Wasm mod registrations served by the extism mods host (resonator-mods).
/// `wasm` is the plugin blob, `sha256` its hex digest verified at load,
/// `caps` the granted capability JSON array (e.g. `["clock","db_read"]`),
/// `config` the mod-visible key/value JSON, `limits` the runtime budgets
/// JSON (`{"timeout_ms": N, "memory_mb": M}`).
pub const MODULATIONS_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _modulations (
  name     TEXT PRIMARY KEY,
  wasm     BLOB,
  sha256   TEXT NOT NULL,
  enabled  INTEGER NOT NULL DEFAULT 0,
  caps     TEXT NOT NULL DEFAULT '[]',
  config   TEXT NOT NULL DEFAULT '{}',
  limits   TEXT NOT NULL DEFAULT '{}',
  note     TEXT
);
";

/// Creates `_modulations` if it does not exist yet. Idempotent; also part
/// of [`ensure_node_tables`].
pub fn ensure_modulations_table(conn: &Connection) -> Result<(), NodeError> {
    conn.execute_batch(MODULATIONS_DDL)?;
    Ok(())
}

/// The modulations a fresh node serves. `chat` is builtin (PLAN sec 5);
/// its user-space tables are ensured by the handler, and `rsntr chat
/// init` adds the projection points and policy rows that make it useful.
pub const DEFAULT_MODS: [&str; 7] = [
    "help",
    "sql-sqlite",
    "sparql",
    "projection",
    "media",
    "audio-duplex",
    "chat",
];

/// Creates every reserved table this crate owns, plus the authenticator's
/// tables (`_policy`, `_scripts`, `_decisions`) and the SPARQL store
/// schema. Idempotent.
pub fn ensure_node_tables(conn: &Connection) -> Result<(), NodeError> {
    for ddl in [
        PEERS_DDL,
        RSNTR_DDL,
        AUDIT_DDL,
        APPLIED_DDL,
        INBOX_DDL,
        KNOCK_BUDGET_DDL,
        MEDIA_DDL,
        PROJECTION_TABLE_DDL,
        MODULATIONS_DDL,
        COMMENTS_DDL,
    ] {
        conn.execute_batch(ddl)?;
    }
    ensure_media_accepts_column(conn)?;
    migrate_modulation(conn, "audio-duplex")?;
    resonator_authenticator::ensure_auth_tables(conn)?;
    resonator_sparql::store::ensure_schema(conn).map_err(|e| NodeError::Sparql(e.0))?;
    seed_table_comments(conn)?;
    Ok(())
}

/// Human-readable table descriptions, keyed by table name. The console
/// surfaces these next to every table and prompts the operator to write
/// one for any table that lacks it: a database a human can read starts
/// with knowing what each table is for. Scaffolds (chat init, example
/// mod seeds) ship comments for the tables they create.
pub const COMMENTS_DDL: &str = "\
CREATE TABLE IF NOT EXISTS _comments (
  name        TEXT PRIMARY KEY,   -- table name
  comment     TEXT NOT NULL,      -- what the table is for, for humans
  updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
";

/// Seeds the canonical descriptions of every reserved table (and the RDF
/// store pair) so a fresh node is fully documented out of the box.
/// INSERT OR IGNORE: an owner's rewording is never overwritten.
fn seed_table_comments(conn: &Connection) -> Result<(), NodeError> {
    const SEED: &[(&str, &str)] = &[
        (
            "_rsntr",
            "node key/value config: endpoint id, advertised modulations, hello hint, help text",
        ),
        (
            "_peers",
            "admitted peers: endpoint id, petname, dial address hints",
        ),
        (
            "_policy",
            "authenticator rules: who may do what on which table (allow/deny)",
        ),
        (
            "_audit",
            "the ledger: every decision on every request, who asked and what was decided",
        ),
        (
            "_applied",
            "idempotency ledger: request ids already executed, so resends never double-apply",
        ),
        (
            "_inbox",
            "requests parked for the owner: knocks and human-tier escalations",
        ),
        (
            "_knock_budget",
            "knock rate limiting: per-key and global token buckets",
        ),
        (
            "_media",
            "media sources: name to command mapping for the media modulation",
        ),
        (
            "_projection",
            "resonance points: the capability menu this node projects to observers",
        ),
        (
            "_modulations",
            "registered wasm mods: blob, hash, capabilities, config, budgets",
        ),
        (
            "_comments",
            "these table descriptions: what each table is for, shown in the console",
        ),
        (
            "_outbox",
            "durable calls to peers: retried with backoff until answered or expired",
        ),
        ("_results", "answers to _outbox calls, keyed by request id"),
        (
            "_scripts",
            "script-tier deciders for the authenticator chain",
        ),
        (
            "_decisions",
            "cached authenticator decisions by request shape",
        ),
        (
            "rdf_terms",
            "RDF store dictionary: every term (IRI, literal, blank) by id",
        ),
        (
            "rdf_triples",
            "RDF store triples: subject/predicate/object/graph over rdf_terms ids",
        ),
    ];
    let mut stmt =
        conn.prepare("INSERT OR IGNORE INTO _comments (name, comment) VALUES (?1, ?2)")?;
    for (name, comment) in SEED {
        stmt.execute((name, comment))?;
    }
    Ok(())
}

/// One `_rsntr` value; `None` when absent.
pub fn get_rsntr(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM _rsntr WHERE key = ?1", [key], |r| {
        r.get(0)
    })
    .optional()
    .ok()
    .flatten()
}

/// Upserts one `_rsntr` key.
pub fn set_rsntr(conn: &Connection, key: &str, value: &str) -> Result<(), NodeError> {
    conn.execute(
        "INSERT INTO _rsntr (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = ?2",
        (key, value),
    )?;
    Ok(())
}

/// Seeds the `_rsntr` defaults a serving node needs, without overwriting
/// anything the owner already set: envelope_version, modulations,
/// encodings, and an auth_chain of the cache and policy tiers (script
/// and human are registered but only run when the owner adds them to
/// `auth_chain`). Idempotent.
pub fn seed_rsntr_defaults(conn: &Connection) -> Result<(), NodeError> {
    let mods = serde_json::to_string(&DEFAULT_MODS).expect("static json");
    for (key, value) in [
        ("envelope_version", ENVELOPE_VERSION),
        ("modulations", mods.as_str()),
        ("encodings", r#"["turtle"]"#),
        ("auth_chain", r#"["cache","policy"]"#),
    ] {
        conn.execute(
            "INSERT OR IGNORE INTO _rsntr (key, value) VALUES (?1, ?2)",
            (key, value),
        )?;
    }
    Ok(())
}

/// Appends a builtin added after a node was initialized to the stored
/// `_rsntr.modulations` list. The transport gate fast-fails any
/// modulation missing from that list, so without this an existing node
/// never serves a new builtin. Runs at most once per database per
/// modulation (a marker key), so an operator who later removes the tag
/// keeps it removed.
fn migrate_modulation(conn: &Connection, name: &str) -> Result<(), NodeError> {
    let marker = format!("migrated_mod_{name}");
    if get_rsntr(conn, &marker).is_some() {
        return Ok(());
    }
    if let Some(raw) = get_rsntr(conn, "modulations")
        && let Ok(mut mods) = serde_json::from_str::<Vec<String>>(&raw)
    {
        if !mods.iter().any(|m| m == name) {
            mods.push(name.to_string());
            let json = serde_json::to_string(&mods).expect("string list json");
            set_rsntr(conn, "modulations", &json)?;
        }
        set_rsntr(conn, &marker, "1")?;
    }
    Ok(())
}

/// The modulations this node serves, from `_rsntr.modulations` (a JSON
/// array), always including `"help"`.
pub fn served_modulations(conn: &Connection) -> Vec<String> {
    let raw = get_rsntr(conn, "modulations");
    let mut mods: Vec<String> = match raw {
        Some(s) => serde_json::from_str::<Vec<String>>(&s).unwrap_or_else(|_| {
            if s.trim().is_empty() {
                Vec::new()
            } else {
                vec![s]
            }
        }),
        None => Vec::new(),
    };
    if !mods.iter().any(|m| m == "help") {
        mods.push("help".to_string());
    }
    mods
}

/// Names of the enabled `_modulations` rows (the wasm mods the hello must
/// advertise on top of the builtins). Empty on any error, including a
/// missing table.
pub fn enabled_mod_names(conn: &Connection) -> Vec<String> {
    let mut stmt =
        match conn.prepare("SELECT name FROM _modulations WHERE enabled = 1 ORDER BY name") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
    stmt.query_map([], |r| r.get::<_, String>(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// Builds this node's `rsntr:Hello` from `_rsntr`: version, encodings
/// (turtle guaranteed), served modulations, and the hint.
pub fn node_hello(conn: &Connection) -> Hello {
    let envelope_version =
        get_rsntr(conn, "envelope_version").unwrap_or_else(|| ENVELOPE_VERSION.to_string());
    let mut encodings: Vec<String> = get_rsntr(conn, "encodings")
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default();
    if !encodings.iter().any(|e| e == "turtle") {
        encodings.insert(0, "turtle".to_string());
    }
    // Builtins first, then the enabled wasm mods; `_rsntr.modulations`
    // itself stays builtin-only.
    let mut mods = served_modulations(conn);
    for name in enabled_mod_names(conn) {
        if !mods.contains(&name) {
            mods.push(name);
        }
    }
    Hello {
        envelope_version,
        encodings,
        mods,
        hint: get_rsntr(conn, "hint"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_is_idempotent_and_complete() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_node_tables(&conn).unwrap();
        ensure_node_tables(&conn).unwrap();
        for t in [
            "_peers",
            "_rsntr",
            "_audit",
            "_applied",
            "_inbox",
            "_knock_budget",
            "_media",
            "_projection",
            "_modulations",
            "_policy",
            "_scripts",
            "_decisions",
            "rdf_terms",
            "rdf_triples",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "missing table {t}");
        }
    }

    #[test]
    fn seed_defaults_do_not_overwrite() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_node_tables(&conn).unwrap();
        set_rsntr(&conn, "auth_chain", r#"["policy","human"]"#).unwrap();
        seed_rsntr_defaults(&conn).unwrap();
        assert_eq!(
            get_rsntr(&conn, "auth_chain").unwrap(),
            r#"["policy","human"]"#
        );
        assert_eq!(
            get_rsntr(&conn, "envelope_version").unwrap(),
            ENVELOPE_VERSION
        );
    }

    #[test]
    fn hello_from_rsntr() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_node_tables(&conn).unwrap();
        seed_rsntr_defaults(&conn).unwrap();
        set_rsntr(&conn, "hint", "query mod 'help' for usage").unwrap();
        let h = node_hello(&conn);
        assert_eq!(h.envelope_version, ENVELOPE_VERSION);
        assert!(h.encodings.iter().any(|e| e == "turtle"));
        for m in DEFAULT_MODS {
            assert!(h.mods.iter().any(|x| x == m), "missing mod {m}");
        }
        assert_eq!(h.hint.as_deref(), Some("query mod 'help' for usage"));
    }

    #[test]
    fn hello_unions_enabled_modulations() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_node_tables(&conn).unwrap();
        seed_rsntr_defaults(&conn).unwrap();
        conn.execute(
            "INSERT INTO _modulations (name, sha256, enabled) VALUES \
             ('time', 'x', 1), ('disabled-one', 'y', 0)",
            [],
        )
        .unwrap();
        let h = node_hello(&conn);
        assert!(h.mods.iter().any(|m| m == "time"));
        assert!(!h.mods.iter().any(|m| m == "disabled-one"));
        // Builtins survive the union.
        assert!(h.mods.iter().any(|m| m == "sql-sqlite"));
        // `_rsntr.modulations` itself is untouched.
        assert!(!get_rsntr(&conn, "modulations").unwrap().contains("time"));
    }
}
