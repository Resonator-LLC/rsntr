//! Node directory plumbing: the database file, the key file, the
//! `_rsntr` defaults, and the `_peers` / `_media` registries.
//!
//! A node lives in one directory: `rsntr.db` (WAL, so the CLI and a
//! running `rsntr serve` share it across processes) and `rsntr.key`
//! (the ed25519 secret key as 64 hex chars, no passphrase in v1).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};

use resonator_node::{node_hello, seed_rsntr_defaults, set_rsntr};
use resonator_protocol::{Hello, Value};
use resonator_transport::{PeerId, endpoint_id_from_secret, parse_ticket};

use crate::channel::{self, OwnerChannel, Prefer};

/// Database file name inside a node directory.
pub const DB_FILE: &str = "rsntr.db";
/// Key file name inside a node directory.
pub const KEY_FILE: &str = "rsntr.key";
/// Web capability token file inside a node directory (a secret like
/// `rsntr.key`, so a file with 0600 rather than a `_rsntr` row: the
/// database is reachable by peers under wildcard policy grants and
/// travels with backups).
pub const WEB_TOKEN_FILE: &str = "rsntr.web-token";

/// The hello hint placed in `_rsntr` at init: a one-liner telling a bare
/// connector how to ask for usage.
pub const DEFAULT_HINT: &str =
    "resonator node; send an rsntr:Query with modulation 'help' for usage, or type HELP";

/// The owner help overview placed in `_rsntr.help_text` at init; the help
/// handler prepends it to its policy-derived facts.
pub const DEFAULT_HELP_TEXT: &str = "This is a resonator node.\n\
Send an rsntr:Query with modulation 'sql-sqlite' to run SQL, \
'sparql' to query the RDF store, 'projection' for my capability menu, \
or 'help' for usage.";

/// Path to the database file of a node directory.
pub fn db_path(dir: &Path) -> PathBuf {
    dir.join(DB_FILE)
}

/// Path to the key file of a node directory.
pub fn key_path(dir: &Path) -> PathBuf {
    dir.join(KEY_FILE)
}

/// Loads the persistent web capability token of a node directory,
/// minting (or, with `rotate`, replacing) it when needed. The token is
/// stable across serve runs so signed-in browsers and installed PWAs
/// keep working after a restart.
#[cfg(feature = "web")]
pub fn load_or_mint_web_token(dir: &Path, rotate: bool) -> Result<String> {
    let path = dir.join(WEB_TOKEN_FILE);
    if !rotate && path.exists() {
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let token = raw.trim();
        if token.len() == 43
            && token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Ok(token.to_string());
        }
        bail!(
            "{} does not hold a web token; delete it to mint a fresh one",
            path.display()
        );
    }
    let token = resonator_web::mint_token();
    std::fs::write(&path, format!("{token}\n"))
        .with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restricting {}", path.display()))?;
    }
    Ok(token)
}

/// Opens the node database with the cross-process settings every CLI
/// entry point uses (WAL, busy timeout). Refuses on an uninitialized
/// directory. This connection does not carry the `rdf_*` SQL functions;
/// the serving side opens via `resonator_node::open_node_db` instead.
pub fn open_db(dir: &Path) -> Result<Connection> {
    let path = db_path(dir);
    if !path.exists() {
        bail!(
            "no node database at {}; run `rsntr init {}` first",
            path.display(),
            dir.display()
        );
    }
    let conn = Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;
    configure(&conn)?;
    Ok(conn)
}

pub(crate) fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_millis(5_000))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    Ok(())
}

/// `rsntr init <dir>`: creates the directory, generates `rsntr.key`,
/// creates `rsntr.db` with every reserved table (including `_policy`,
/// `_outbox`/`_results`, and the RDF store schema), and seeds the
/// `_rsntr` defaults (modulations help/sql-sqlite/sparql/projection/
/// media, encodings turtle, auth_chain cache+policy). Refuses to touch an
/// initialized directory.
pub fn init_dir(dir: &Path) -> Result<PeerId> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let kpath = key_path(dir);
    let dpath = db_path(dir);
    if kpath.exists() || dpath.exists() {
        bail!(
            "{} is already initialized (rsntr.key or rsntr.db exists)",
            dir.display()
        );
    }

    let secret = iroh::SecretKey::generate();
    let bytes = secret.to_bytes();
    let id = endpoint_id_from_secret(bytes);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(&kpath, format!("{hex}\n"))
        .with_context(|| format!("writing {}", kpath.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&kpath, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restricting {}", kpath.display()))?;
    }

    // open_node_db creates the file, switches to WAL, and ensures every
    // reserved table (incl. _policy and rdf_terms/rdf_triples).
    let conn = resonator_node::open_node_db(&dpath)
        .map_err(|e| anyhow::anyhow!("creating {}: {e}", dpath.display()))?;
    configure(&conn)?;
    seed_rsntr_defaults(&conn).map_err(|e| anyhow::anyhow!("seeding _rsntr: {e}"))?;
    resonator_surfaces::ensure_outbox_tables(&conn)
        .map_err(|e| anyhow::anyhow!("creating _outbox/_results: {e}"))?;
    for (key, value) in [
        ("endpoint_id", id.to_string()),
        ("hint", DEFAULT_HINT.to_string()),
        ("help_text", DEFAULT_HELP_TEXT.to_string()),
    ] {
        set_rsntr(&conn, key, &value).map_err(|e| anyhow::anyhow!("writing _rsntr: {e}"))?;
    }
    Ok(id)
}

/// Loads the ed25519 secret key bytes from `rsntr.key`.
pub fn load_secret(dir: &Path) -> Result<[u8; 32]> {
    let path = key_path(dir);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("no key file at {}; run `rsntr init` first", path.display()))?;
    let hex = text.trim();
    if hex.len() != 64 || !hex.is_ascii() {
        bail!("{}: expected 64 hex chars", path.display());
    }
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
            .with_context(|| format!("{}: not hex", path.display()))?;
    }
    Ok(bytes)
}

/// `rsntr id`: this node's endpoint id, derived from the key file with no
/// endpoint bind.
pub fn node_id(dir: &Path) -> Result<PeerId> {
    Ok(endpoint_id_from_secret(load_secret(dir)?))
}

/// Builds this node's hello from `_rsntr` (via the node crate), with the
/// default hint filled in when the owner set none.
pub fn hello_from_db(conn: &Connection) -> Hello {
    let mut hello = node_hello(conn);
    if hello.hint.is_none() {
        hello.hint = Some(DEFAULT_HINT.to_string());
    }
    hello
}

/// `rsntr peer add <name> <endpoint_id_or_ticket> [addr]...`: upserts a
/// `_peers` row through the owner channel (docs/owner-channel.md sec 5).
/// The target may be a 64-hex endpoint id or a dialing ticket (whose
/// embedded direct addresses are stored as dial hints, merged with any
/// explicit ones). Returns the peer id and the stored addresses.
pub fn peer_add(
    dir: &Path,
    name: &str,
    target: &str,
    addrs: &[SocketAddr],
) -> Result<(PeerId, Vec<SocketAddr>)> {
    peer_add_with(dir, Prefer::Auto, name, target, addrs)
}

/// [`peer_add`] with an explicit transport preference.
pub fn peer_add_with(
    dir: &Path,
    prefer: Prefer,
    name: &str,
    target: &str,
    addrs: &[SocketAddr],
) -> Result<(PeerId, Vec<SocketAddr>)> {
    let (id, mut all) = match target.parse::<PeerId>() {
        Ok(id) => (id, Vec::new()),
        Err(_) => parse_ticket(target).map_err(|e| {
            anyhow::anyhow!("{target:?} is neither a 64-hex endpoint id nor a ticket: {e}")
        })?,
    };
    all.extend(addrs.iter().copied());
    all.sort();
    all.dedup();
    let addrs_json = if all.is_empty() {
        None
    } else {
        Some(serde_json::to_string(
            &all.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
        )?)
    };
    let id_hex = id.to_string();
    channel::block_on(async move {
        let ch = OwnerChannel::open(dir, prefer).await?;
        channel::execute(
            &ch,
            "INSERT INTO _peers (endpoint_id, name, addrs, added_at) \
             VALUES (?1, ?2, ?3, datetime('now')) \
             ON CONFLICT (endpoint_id) DO UPDATE SET name = ?2, addrs = ?3",
            vec![
                Value::Text(id_hex),
                Value::Text(name.to_string()),
                addrs_json.map(Value::Text).unwrap_or(Value::Null),
            ],
        )
        .await
    })??;
    Ok((id, all))
}

/// `rsntr media add`: upserts a `_media` source row through the owner
/// channel. The command runs under `sh -c` on the serving host; its
/// stdout is the feed.
pub fn media_add(
    dir: &Path,
    name: &str,
    command: &str,
    content_type: &str,
    note: Option<&str>,
) -> Result<()> {
    media_add_with(dir, Prefer::Auto, name, command, content_type, note)
}

/// [`media_add`] with an explicit transport preference.
pub fn media_add_with(
    dir: &Path,
    prefer: Prefer,
    name: &str,
    command: &str,
    content_type: &str,
    note: Option<&str>,
) -> Result<()> {
    media_add_duplex(dir, prefer, name, command, content_type, None, note)
}

/// The full upsert: `accepts` non-None makes the row an audio-duplex
/// source (the command's stdin reads bytes in that media type).
pub fn media_add_duplex(
    dir: &Path,
    prefer: Prefer,
    name: &str,
    command: &str,
    content_type: &str,
    accepts: Option<&str>,
    note: Option<&str>,
) -> Result<()> {
    channel::block_on(async move {
        let ch = OwnerChannel::open(dir, prefer).await?;
        channel::execute(
            &ch,
            "INSERT INTO _media (name, command, content_type, accepts, note) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT (name) DO UPDATE SET command = ?2, content_type = ?3, \
             accepts = ?4, note = ?5",
            vec![
                Value::Text(name.to_string()),
                Value::Text(command.to_string()),
                Value::Text(content_type.to_string()),
                accepts
                    .map(|a| Value::Text(a.to_string()))
                    .unwrap_or(Value::Null),
                note.map(|n| Value::Text(n.to_string()))
                    .unwrap_or(Value::Null),
            ],
        )
        .await
    })??;
    Ok(())
}

/// `rsntr media list`: all `_media` rows as (name, content_type,
/// command), read as an owner-channel Query.
pub fn media_list(dir: &Path) -> Result<Vec<(String, String, String)>> {
    media_list_with(dir, Prefer::Auto)
}

/// [`media_list`] with an explicit transport preference.
pub fn media_list_with(dir: &Path, prefer: Prefer) -> Result<Vec<(String, String, String)>> {
    let (_cols, rows, _done) = channel::block_on(async move {
        let ch = OwnerChannel::open(dir, prefer).await?;
        channel::query_rows(
            &ch,
            "SELECT name, content_type, command FROM _media ORDER BY name",
            vec![],
        )
        .await
    })??;
    Ok(rows
        .iter()
        .map(|row| {
            (
                channel::cell_text(row, "name").unwrap_or_default(),
                channel::cell_text(row, "content_type").unwrap_or_default(),
                channel::cell_text(row, "command").unwrap_or_default(),
            )
        })
        .collect())
}

/// `rsntr media allow <peer> <source>`: inserts the `_policy` row letting
/// `peer` (a petname, a 64-hex endpoint id, or `*`) open `source` (a
/// `_media` name, or `*`). The source name rides in `table_name`. The
/// petname resolution is a local read; the row rides the owner channel.
pub fn media_allow(dir: &Path, peer: &str, source: &str) -> Result<String> {
    media_allow_with(dir, Prefer::Auto, peer, source)
}

/// [`media_allow`] with an explicit transport preference.
pub fn media_allow_with(dir: &Path, prefer: Prefer, peer: &str, source: &str) -> Result<String> {
    let peer_id = if peer == "*" {
        "*".to_string()
    } else {
        let conn = open_db(dir)?;
        resolve_peer(&conn, peer)?.0.to_string()
    };
    let peer_param = peer_id.clone();
    channel::block_on(async move {
        let ch = OwnerChannel::open(dir, prefer).await?;
        channel::execute(
            &ch,
            "INSERT INTO _policy (peer_or_group, table_name, action, effect, note) \
             VALUES (?1, ?2, 'media', 'allow', 'rsntr media allow')",
            vec![Value::Text(peer_param), Value::Text(source.to_string())],
        )
        .await
    })??;
    Ok(peer_id)
}

/// All stored peer dial hints: each `_peers` row's endpoint id with its
/// parsed `addrs`, for seeding the serving endpoint's address lookup in
/// offline / LAN setups. Rows with corrupt ids or addresses are skipped.
pub async fn peer_dial_hints(
    db: &resonator_node::DbHandle,
) -> Result<Vec<(PeerId, Vec<SocketAddr>)>> {
    db.call(|conn| {
        let mut stmt =
            conn.prepare("SELECT endpoint_id, addrs FROM _peers WHERE addrs IS NOT NULL")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .filter_map(Result::ok)
            .filter_map(|(id_text, addrs_json)| {
                let id: PeerId = id_text.parse().ok()?;
                let texts: Vec<String> = serde_json::from_str(&addrs_json).ok()?;
                let addrs: Vec<SocketAddr> = texts.iter().filter_map(|t| t.parse().ok()).collect();
                Some((id, addrs))
            })
            .collect();
        Ok::<_, rusqlite::Error>(rows)
    })
    .await
    .map_err(|e| anyhow::anyhow!("reading _peers dial hints: {e}"))?
    .map_err(|e| anyhow::anyhow!("reading _peers dial hints: {e}"))
}

/// Resolves a request target: a local petname from `_peers`, or a literal
/// 64-hex endpoint id (whose `_peers` row, if any, supplies dial hints).
/// Merges observed dial addresses into a KNOWN peer's `_peers.addrs`:
/// fresh addresses go first, the stored ones follow, deduplicated and
/// capped. Returns false (and writes nothing) for peers without a row -
/// observing a stranger's address must not admit it. Backs the
/// transport's [`resonator_transport::AddrSink`], which keeps dial
/// hints current across peer restarts (every serve run binds new
/// ports).
pub fn merge_peer_addrs(
    conn: &Connection,
    endpoint_hex: &str,
    observed: &[SocketAddr],
) -> Result<bool> {
    const MAX_ADDRS: usize = 8;
    let row: Option<Option<String>> = conn
        .query_row(
            "SELECT addrs FROM _peers WHERE endpoint_id = ?1",
            [endpoint_hex],
            |r| r.get(0),
        )
        .optional()?;
    let Some(stored_json) = row else {
        return Ok(false);
    };
    let stored: Vec<String> = match stored_json {
        None => Vec::new(),
        Some(raw) => serde_json::from_str(&raw).unwrap_or_default(),
    };
    let mut merged: Vec<String> = observed.iter().map(|a| a.to_string()).collect();
    for a in stored {
        if !merged.contains(&a) {
            merged.push(a);
        }
    }
    merged.truncate(MAX_ADDRS);
    let json = serde_json::to_string(&merged)?;
    conn.execute(
        "UPDATE _peers SET addrs = ?2 WHERE endpoint_id = ?1",
        (endpoint_hex, json),
    )?;
    Ok(true)
}

pub fn resolve_peer(conn: &Connection, target: &str) -> Result<(PeerId, Vec<SocketAddr>)> {
    let row: Option<(String, Option<String>)> = if let Ok(id) = target.parse::<PeerId>() {
        Some((
            id.to_string(),
            conn.query_row(
                "SELECT addrs FROM _peers WHERE endpoint_id = ?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .flatten(),
        ))
    } else {
        conn.query_row(
            "SELECT endpoint_id, addrs FROM _peers WHERE name = ?1",
            [target],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    };
    let Some((id_text, addrs_json)) = row else {
        bail!("unknown peer {target:?}: not an endpoint id and not a name in _peers");
    };
    let id: PeerId = id_text
        .parse()
        .map_err(|e| anyhow::anyhow!("corrupt _peers.endpoint_id {id_text:?}: {e}"))?;
    let addrs = match addrs_json {
        None => Vec::new(),
        Some(raw) => {
            let texts: Vec<String> =
                serde_json::from_str(&raw).context("_peers.addrs is not a JSON array")?;
            texts
                .iter()
                .map(|t| t.parse::<SocketAddr>())
                .collect::<Result<Vec<_>, _>>()
                .context("_peers.addrs contains an invalid socket address")?
        }
    };
    Ok((id, addrs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use resonator_node::DEFAULT_MODS;

    fn read_rsntr(conn: &Connection, key: &str) -> Option<String> {
        conn.query_row("SELECT value FROM _rsntr WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()
        .unwrap()
    }

    #[test]
    fn merge_peer_addrs_freshens_known_peers_only() {
        let tmp = TempDir::new("store-merge-addrs");
        init_dir(tmp.path()).expect("init");
        let conn = open_db(tmp.path()).expect("open");
        let hex = "aa".repeat(32);
        conn.execute(
            "INSERT INTO _peers (endpoint_id, name, addrs, added_at) \
             VALUES (?1, 'p', '[\"10.0.0.1:1000\",\"10.0.0.2:1000\"]', 'now')",
            [&hex],
        )
        .unwrap();

        // Fresh addresses go first; stored ones follow, deduplicated.
        let observed = vec![
            "10.0.0.1:2000".parse().unwrap(),
            "10.0.0.2:1000".parse().unwrap(),
        ];
        assert!(merge_peer_addrs(&conn, &hex, &observed).expect("merge"));
        let json: String = conn
            .query_row(
                "SELECT addrs FROM _peers WHERE endpoint_id = ?1",
                [&hex],
                |r| r.get(0),
            )
            .unwrap();
        let addrs: Vec<String> = serde_json::from_str(&json).unwrap();
        assert_eq!(
            addrs,
            vec!["10.0.0.1:2000", "10.0.0.2:1000", "10.0.0.1:1000"]
        );

        // A stranger's observation writes nothing.
        let stranger = "bb".repeat(32);
        assert!(!merge_peer_addrs(&conn, &stranger, &observed).expect("no-op"));
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM _peers WHERE endpoint_id = ?1",
                [&stranger],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn init_writes_key_defaults_and_schema() {
        let tmp = TempDir::new("store-init");
        let id = init_dir(tmp.path()).expect("init");
        assert_eq!(node_id(tmp.path()).expect("id"), id);

        // The key file is 64 hex chars.
        let key = std::fs::read_to_string(key_path(tmp.path())).expect("key");
        assert_eq!(key.trim().len(), 64);
        assert!(key.trim().chars().all(|c| c.is_ascii_hexdigit()));

        let conn = open_db(tmp.path()).expect("open");
        // Every reserved table plus the RDF store schema exists.
        for table in [
            "_rsntr",
            "_peers",
            "_policy",
            "_audit",
            "_applied",
            "_inbox",
            "_knock_budget",
            "_media",
            "_projection",
            "rdf_terms",
            "rdf_triples",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .expect("count");
            assert_eq!(n, 1, "missing table {table}");
        }

        // `_rsntr` defaults: the v3 modulations and identity keys.
        let mods = read_rsntr(&conn, "modulations").expect("modulations");
        assert_eq!(mods, serde_json::to_string(&DEFAULT_MODS).unwrap());
        assert_eq!(
            read_rsntr(&conn, "endpoint_id").as_deref(),
            Some(id.to_string().as_str())
        );
        assert_eq!(
            read_rsntr(&conn, "encodings").as_deref(),
            Some(r#"["turtle"]"#)
        );
        assert_eq!(
            read_rsntr(&conn, "auth_chain").as_deref(),
            Some(r#"["cache","policy"]"#)
        );
        assert_eq!(read_rsntr(&conn, "hint").as_deref(), Some(DEFAULT_HINT));
        assert!(
            read_rsntr(&conn, "help_text")
                .unwrap()
                .contains("resonator node")
        );

        // Re-init refuses.
        assert!(init_dir(tmp.path()).is_err());
    }

    #[test]
    fn hello_mirrors_rsntr() {
        let tmp = TempDir::new("store-hello");
        init_dir(tmp.path()).expect("init");
        let conn = open_db(tmp.path()).expect("open");
        let hello = hello_from_db(&conn);
        assert_eq!(
            hello.envelope_version,
            resonator_transport::ENVELOPE_VERSION
        );
        assert_eq!(hello.encodings, vec!["turtle"]);
        for m in DEFAULT_MODS {
            assert!(hello.mods.iter().any(|x| x == m), "missing mod {m}");
        }
        assert_eq!(hello.hint.as_deref(), Some(DEFAULT_HINT));
    }

    #[test]
    fn peer_add_and_resolve() {
        let tmp = TempDir::new("store-peers");
        init_dir(tmp.path()).expect("init");
        let id_hex = "ab".repeat(32);
        let addr: SocketAddr = "127.0.0.1:4433".parse().expect("addr");
        let (id, stored) = peer_add(tmp.path(), "bob", &id_hex, &[addr]).expect("peer add");
        assert_eq!(id.to_string(), id_hex);
        assert_eq!(stored, vec![addr]);

        let conn = open_db(tmp.path()).expect("open");
        let (by_name, addrs) = resolve_peer(&conn, "bob").expect("by name");
        assert_eq!(by_name.to_string(), id_hex);
        assert_eq!(addrs, vec![addr]);
        let (by_id, addrs2) = resolve_peer(&conn, &id_hex).expect("by id");
        assert_eq!(by_id, by_name);
        assert_eq!(addrs2, vec![addr]);
        assert!(resolve_peer(&conn, "nobody").is_err());

        // Upsert replaces name and addrs.
        peer_add(tmp.path(), "robert", &id_hex, &[]).expect("upsert");
        let (again, addrs3) = resolve_peer(&conn, "robert").expect("renamed");
        assert_eq!(again, by_name);
        assert!(addrs3.is_empty());

        // A garbage target is neither an id nor a ticket.
        assert!(peer_add(tmp.path(), "x", "not-a-thing", &[]).is_err());
    }

    #[test]
    fn media_registry_round_trip() {
        let tmp = TempDir::new("store-media");
        init_dir(tmp.path()).expect("init");
        media_add(
            tmp.path(),
            "cam",
            "cat /dev/null",
            "video/mp2t",
            Some("test"),
        )
        .expect("media add");
        let listed = media_list(tmp.path()).expect("list");
        assert_eq!(
            listed,
            vec![(
                "cam".to_string(),
                "video/mp2t".to_string(),
                "cat /dev/null".to_string()
            )]
        );
        let peer_hex = "cd".repeat(32);
        peer_add(tmp.path(), "eve", &peer_hex, &[]).expect("peer add");
        let allowed = media_allow(tmp.path(), "eve", "cam").expect("allow");
        assert_eq!(allowed, peer_hex);
        let conn = open_db(tmp.path()).expect("open");
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM _policy WHERE peer_or_group = ?1 \
                 AND table_name = 'cam' AND action = 'media' AND effect = 'allow'",
                [&peer_hex],
                |r| r.get(0),
            )
            .expect("policy row");
        assert_eq!(n, 1);
    }
}
