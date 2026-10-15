//! Node directory plumbing: the database file, the key file, the
//! `_rsntr` defaults, and the `_peers` / `_media` registries.
//!
//! A node lives in one directory: `rsntr.db` (WAL, so the CLI and a
//! running `rsntr serve` share it across processes) and `rsntr.key`
//! (the ed25519 secret key as 64 hex chars, no passphrase in v1).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

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

/// Where `rsntr` looks for the node when no directory is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirSource {
    /// `$RSNTR_NODE`.
    Env,
    /// The current directory, because it holds `rsntr.db`.
    Cwd,
    /// The per-user default ([`default_node_dir`]).
    Default,
}

impl DirSource {
    pub fn as_str(self) -> &'static str {
        match self {
            DirSource::Env => "env",
            DirSource::Cwd => "cwd",
            DirSource::Default => "default",
        }
    }
}

/// The per-user default node directory: one node per device, shared by
/// every tool of the user (docs/shared-node.md). macOS
/// `~/Library/Application Support/rsntr/node`, Linux
/// `$XDG_DATA_HOME/rsntr/node` (else `~/.local/share/rsntr/node`),
/// Windows `%APPDATA%\rsntr\node`; `.` when no home is known.
pub fn default_node_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library").join("Application Support"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| home.map(|h| h.join(".local").join("share")))
    };
    match base {
        Some(b) => b.join("rsntr").join("node"),
        None => PathBuf::from("."),
    }
}

/// Resolves the directory a command uses when `-d` is omitted, in
/// order: `$RSNTR_NODE`; the current directory when it already holds
/// `rsntr.db` (scripts that `cd` into a node keep working); the
/// per-user default.
pub fn resolve_default_dir() -> (PathBuf, DirSource) {
    if let Some(env) = std::env::var_os("RSNTR_NODE")
        && !env.is_empty()
    {
        return (PathBuf::from(env), DirSource::Env);
    }
    if Path::new(DB_FILE).exists() {
        return (PathBuf::from("."), DirSource::Cwd);
    }
    (default_node_dir(), DirSource::Default)
}

/// [`resolve_default_dir`] memoized for clap defaults.
pub fn cli_default_dir() -> PathBuf {
    static DIR: std::sync::LazyLock<PathBuf> = std::sync::LazyLock::new(|| resolve_default_dir().0);
    DIR.clone()
}

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
    // tmp + rename: another app reading mid-write must see the old
    // token or the new one, never a torn file.
    let tmp = dir.join(format!("{WEB_TOKEN_FILE}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, format!("{token}\n"))
        .with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restricting {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("moving the web token into {}", path.display()))?;
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
    let conn = open_db_at(&path)?;
    configure(&conn)?;
    verify_identity(dir, &conn)?;
    Ok(conn)
}

/// A side connection to a node database by path: the busy timeout set
/// before anything else and the schema-compat check, nothing more. For
/// the short-lived connections serving surfaces open beside the
/// pipeline (dial-time `_peers` reads, address sinks, the blob gate).
pub fn open_db_at(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    conn.busy_timeout(resonator_node::BUSY_TIMEOUT)?;
    resonator_node::check_schema_compat(&conn).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(conn)
}

/// A directory mixed from two nodes (a database copied beside the wrong
/// key) would serve one identity's data under another's name; refuse it.
fn verify_identity(dir: &Path, conn: &Connection) -> Result<()> {
    if let Some(stored) = resonator_node::get_rsntr(conn, "endpoint_id")
        && let Ok(id) = node_id(dir)
        && stored != id.to_string()
    {
        bail!(
            "{} holds the database of node {stored} but the key of node {id}; \
             the directory mixes two nodes",
            dir.display()
        );
    }
    Ok(())
}

pub(crate) fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(resonator_node::BUSY_TIMEOUT)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    Ok(())
}

/// The init lock file name: initialization of a shared directory by
/// several apps at once must produce one identity.
pub const INIT_LOCK_FILE: &str = "rsntr.init.lock";

/// Held while a directory is initialized (blocking exclusive lock; a
/// filesystem without locks proceeds unlocked with a warning).
struct InitLock {
    file: Option<std::fs::File>,
}

impl InitLock {
    fn acquire(dir: &Path) -> Result<Self> {
        let path = dir.join(INIT_LOCK_FILE);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        match file.lock() {
            Ok(()) => Ok(Self { file: Some(file) }),
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
                tracing::warn!(path = %path.display(), "file locking unsupported; initializing unlocked");
                Ok(Self { file: None })
            }
            Err(e) => Err(anyhow::anyhow!("locking {}: {e}", path.display())),
        }
    }
}

impl Drop for InitLock {
    fn drop(&mut self) {
        if let Some(f) = &self.file {
            let _ = f.unlock();
        }
    }
}

/// `rsntr init <dir>`: creates the directory, generates `rsntr.key`,
/// creates `rsntr.db` with every reserved table (including `_policy`,
/// `_outbox`/`_results`, and the RDF store schema), and seeds the
/// `_rsntr` defaults (modulations help/sql-sqlite/sparql/projection/
/// media, encodings turtle, auth_chain cache+policy+human). Refuses to touch an
/// initialized directory.
pub fn init_dir(dir: &Path) -> Result<PeerId> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let _lock = InitLock::acquire(dir)?;
    init_locked(dir)
}

/// Initializes `dir` unless it already holds a node, in which case its
/// id is returned: what an embedding calls on every start, safe for
/// several apps starting on one empty shared directory at once (the
/// init lock serializes them; exactly one generates the key).
pub fn ensure_dir_initialized(dir: &Path) -> Result<PeerId> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let _lock = InitLock::acquire(dir)?;
    if key_path(dir).exists() {
        return node_id(dir);
    }
    init_locked(dir)
}

fn init_locked(dir: &Path) -> Result<PeerId> {
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
    // create_new: even without the lock two initializers cannot both
    // write a key; the loser sees AlreadyExists.
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut key_file = match opts.open(&kpath) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => bail!(
            "{} is already initialized (rsntr.key exists)",
            dir.display()
        ),
        Err(e) => return Err(e).with_context(|| format!("creating {}", kpath.display())),
    };
    {
        use std::io::Write;
        key_file
            .write_all(format!("{hex}\n").as_bytes())
            .with_context(|| format!("writing {}", kpath.display()))?;
        key_file.sync_all().ok();
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
    conn: &mut Connection,
    endpoint_hex: &str,
    observed: &[SocketAddr],
) -> Result<bool> {
    const MAX_ADDRS: usize = 8;
    // Read-modify-write under the write lock: two processes observing
    // one peer at once must not lose each other's addresses.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let row: Option<Option<String>> = tx
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
    tx.execute(
        "UPDATE _peers SET addrs = ?2 WHERE endpoint_id = ?1",
        (endpoint_hex, json),
    )?;
    tx.commit()?;
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
        let mut conn = open_db(tmp.path()).expect("open");
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
        assert!(merge_peer_addrs(&mut conn, &hex, &observed).expect("merge"));
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
        assert!(!merge_peer_addrs(&mut conn, &stranger, &observed).expect("no-op"));
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
            Some(r#"["cache","policy","human"]"#)
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

// ---------------------------------------------------------------------
// Peer visibility: `rsntr peer list` / `rsntr peer status`
// ---------------------------------------------------------------------

/// One peer as `rsntr peer list` reports it.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub name: Option<String>,
    pub endpoint_id: String,
    /// `_peers.last_seen`: written only by presence beacons, so often
    /// NULL — evidence, not a liveness verdict.
    pub last_seen: Option<String>,
    pub last_seen_age_secs: Option<i64>,
    pub addr_count: usize,
    /// `_outbox` rows for this peer still queued or sent (not done).
    pub pending_outbox: i64,
}

/// Every admitted peer with presence evidence and undelivered counts.
/// Ephemeral watch identities are skipped.
pub fn peer_list(conn: &Connection) -> Result<Vec<PeerInfo>> {
    let mut stmt = conn.prepare(
        "SELECT p.endpoint_id, p.name, p.addrs, p.last_seen, \
                CASE WHEN p.last_seen IS NULL THEN NULL \
                     ELSE CAST((julianday('now') - julianday(p.last_seen)) * 86400 AS INTEGER) END, \
                (SELECT count(*) FROM _outbox o \
                  WHERE (o.peer = p.endpoint_id OR (p.name IS NOT NULL AND o.peer = p.name)) \
                    AND o.status IN ('queued', 'sent')) \
         FROM _peers p \
         WHERE p.name IS NULL OR p.name NOT IN ('_watch', '_await') \
         ORDER BY p.name IS NULL, p.name, p.endpoint_id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let addrs: Option<String> = r.get(2)?;
            Ok(PeerInfo {
                endpoint_id: r.get(0)?,
                name: r.get(1)?,
                addr_count: addrs
                    .and_then(|a| serde_json::from_str::<Vec<String>>(&a).ok())
                    .map(|v| v.len())
                    .unwrap_or(0),
                last_seen: r.get(3)?,
                last_seen_age_secs: r.get(4)?,
                pending_outbox: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `rsntr peer status <name>`: one peer's evidence, plus its outbox
/// breakdown. `reachable` is filled by the caller's `--probe`.
#[derive(Debug, Clone)]
pub struct PeerStatus {
    pub info: PeerInfo,
    /// `(status, count)` over this peer's `_outbox` rows.
    pub outbox_by_status: Vec<(String, i64)>,
    pub reachable: Option<bool>,
}

pub fn peer_status(conn: &Connection, target: &str) -> Result<PeerStatus> {
    let (id, _addrs) = resolve_peer(conn, target)?;
    let hex = id.to_string();
    let info = peer_list(conn)?
        .into_iter()
        .find(|p| p.endpoint_id == hex)
        .unwrap_or(PeerInfo {
            name: None,
            endpoint_id: hex.clone(),
            last_seen: None,
            last_seen_age_secs: None,
            addr_count: 0,
            pending_outbox: 0,
        });
    let mut stmt = conn.prepare(
        "SELECT status, count(*) FROM _outbox \
         WHERE peer = ?1 OR (?2 IS NOT NULL AND peer = ?2) \
         GROUP BY status ORDER BY status",
    )?;
    let outbox_by_status = stmt
        .query_map((&hex, &info.name), |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(PeerStatus {
        info,
        outbox_by_status,
        reachable: None,
    })
}
