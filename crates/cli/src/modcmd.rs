//! `rsntr mod ...` over the owner channel (docs/owner-channel.md sec 5):
//! the `_modulations` registry rows ride ordinary `sql-sqlite` envelopes
//! instead of direct database writes. The wasm blob travels as an inline
//! blob parameter; past roughly 180 KiB its base64 form would not fit a
//! socket frame, so large blobs take the in-process transport, where the
//! envelope is handed to the dispatcher decoded and the frame budget
//! does not bind (doc sec 5.2). Registration takes effect at the next
//! `rsntr serve` start either way (the registry loads once).

use std::path::Path;

use anyhow::{Result, bail};

use resonator_mods::ModRow;
use resonator_protocol::Value;

use crate::channel::{self, OwnerChannel, Prefer};

/// Wasm size past which the inline blob parameter cannot fit a 256 KiB
/// socket frame once base64-inflated; the CLI then goes in-process.
const WASM_SOCKET_MAX: usize = 180 * 1024;

/// Effective preference for a request that carries the wasm blob.
fn blob_prefer(prefer: Prefer, wasm_len: usize) -> Result<Prefer> {
    if wasm_len <= WASM_SOCKET_MAX {
        return Ok(prefer);
    }
    match prefer {
        Prefer::Socket => bail!(
            "the wasm is {wasm_len} bytes; its inline blob parameter does not fit \
             a control-socket frame. Drop --socket: the in-process transport \
             carries it, and registration takes effect at the next serve start \
             regardless"
        ),
        _ => Ok(Prefer::Local),
    }
}

fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if ok {
        Ok(())
    } else {
        bail!("invalid mod name {name:?}: use ascii letters, digits, '-', '_', '.'")
    }
}

/// `rsntr mod add`: registers (or replaces) a mod, wasm as an inline
/// blob parameter. New rows start disabled; replacing keeps the enabled
/// bit. Returns the stored sha256.
pub fn mod_add(
    dir: &Path,
    prefer: Prefer,
    name: &str,
    wasm: &[u8],
    caps: &[String],
    note: Option<&str>,
) -> Result<String> {
    check_name(name)?;
    let sha = resonator_mods::sha256_hex(wasm);
    let caps_json = serde_json::to_string(caps)?;
    let prefer = blob_prefer(prefer, wasm.len())?;
    let sha_param = sha.clone();
    channel::block_on(async move {
        let ch = OwnerChannel::open(dir, prefer).await?;
        channel::execute(
            &ch,
            "INSERT INTO _modulations (name, wasm, sha256, enabled, caps, note) \
             VALUES (?1, ?2, ?3, 0, ?4, ?5) \
             ON CONFLICT(name) DO UPDATE SET wasm = ?2, sha256 = ?3, caps = ?4, note = ?5",
            vec![
                Value::Text(name.to_string()),
                Value::Blob(wasm.to_vec()),
                Value::Text(sha_param),
                Value::Text(caps_json),
                note.map(|n| Value::Text(n.to_string()))
                    .unwrap_or(Value::Null),
            ],
        )
        .await
    })??;
    Ok(sha)
}

/// `rsntr mod list`: the registry rows, without the blobs.
pub fn mod_list(dir: &Path, prefer: Prefer) -> Result<Vec<ModRow>> {
    let (_cols, rows, _done) = channel::block_on(async move {
        let ch = OwnerChannel::open(dir, prefer).await?;
        channel::query_rows(
            &ch,
            "SELECT name, sha256, enabled, caps, note FROM _modulations ORDER BY name",
            vec![],
        )
        .await
    })??;
    Ok(rows
        .iter()
        .map(|row| ModRow {
            name: channel::cell_text(row, "name").unwrap_or_default(),
            sha256: channel::cell_text(row, "sha256").unwrap_or_default(),
            enabled: matches!(channel::cell(row, "enabled"), Value::Integer(n) if *n != 0),
            caps: channel::cell_text(row, "caps")
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default(),
            note: channel::cell_text(row, "note"),
        })
        .collect())
}

/// `rsntr mod enable` / `disable`: flips the enabled bit; false when no
/// such row exists. The serving registry loads at start, so a flip takes
/// effect at the next `rsntr serve` even when it rides the live socket.
pub fn mod_set_enabled(dir: &Path, prefer: Prefer, name: &str, enabled: bool) -> Result<bool> {
    let done = channel::block_on(async move {
        let ch = OwnerChannel::open(dir, prefer).await?;
        channel::execute(
            &ch,
            "UPDATE _modulations SET enabled = ?2 WHERE name = ?1",
            vec![
                Value::Text(name.to_string()),
                Value::Integer(i64::from(enabled)),
            ],
        )
        .await
    })??;
    Ok(done.affected_rows.unwrap_or(0) > 0)
}

/// `rsntr mod rm`: deletes a registry row; false when none existed.
pub fn mod_rm(dir: &Path, prefer: Prefer, name: &str) -> Result<bool> {
    let done = channel::block_on(async move {
        let ch = OwnerChannel::open(dir, prefer).await?;
        channel::execute(
            &ch,
            "DELETE FROM _modulations WHERE name = ?1",
            vec![Value::Text(name.to_string())],
        )
        .await
    })??;
    Ok(done.affected_rows.unwrap_or(0) > 0)
}

/// `rsntr mod describe`: the row (wasm and recorded sha) rides a Query
/// envelope; the wasm is verified and loaded natively to call its
/// `describe()` export. In-process unless `--socket` forces the socket
/// (a stored wasm larger than one frame cannot ride it back).
pub fn describe(
    dir: &Path,
    prefer: Prefer,
    name: &str,
    default_timeout_ms: u64,
) -> Result<resonator_mods::Descriptor> {
    let effective = match prefer {
        Prefer::Auto => Prefer::Local,
        other => other,
    };
    let (_cols, rows, _done) = channel::block_on(async move {
        let ch = OwnerChannel::open(dir, effective).await?;
        channel::query_rows(
            &ch,
            "SELECT wasm, sha256 FROM _modulations WHERE name = ?1",
            vec![Value::Text(name.to_string())],
        )
        .await
    })??;
    let Some(row) = rows.first() else {
        bail!("no mod named {name:?} (or no wasm stored)");
    };
    let Value::Blob(wasm) = channel::cell(row, "wasm") else {
        bail!("no mod named {name:?} (or no wasm stored)");
    };
    let recorded = channel::cell_text(row, "sha256").unwrap_or_default();
    let sha = resonator_mods::sha256_hex(wasm);
    if !sha.eq_ignore_ascii_case(recorded.trim()) {
        bail!("sha256 mismatch: row says {recorded}, blob is {sha}");
    }
    resonator_mods::describe_wasm(wasm, default_timeout_ms).map_err(|e| anyhow::anyhow!("{e}"))
}

// ---------------------------------------------------------------------
// Remote install: `rsntr mod push` / `rsntr mod rm --peer`
// ---------------------------------------------------------------------

/// Imports `wasm_path` into this node's own blob store so the target can
/// fetch it back, returning the `blake3:<hex>` hash and byte size. When
/// this node's daemon holds the store lock, the import is delegated to
/// it over the owner-lane `mod-install` affordance (one path param);
/// otherwise it runs in-process. Mirrors the chat attachment path.
pub async fn import_own_wasm(dir: &Path, wasm_path: &Path) -> Result<(String, u64)> {
    use anyhow::Context;
    let ch = OwnerChannel::open(dir, Prefer::Auto).await?;
    if ch.is_socket() {
        let abs = std::path::absolute(wasm_path)
            .with_context(|| format!("resolving {}", wasm_path.display()))?;
        let mut request = resonator_protocol::Request::new(
            resonator_protocol::RequestKind::Execute,
            "mod-install",
            "",
        );
        request.params = vec![Value::Text(abs.to_string_lossy().into_owned())];
        let id = request.id_string();
        match ch.send(&request.to_envelope(), &id).await? {
            crate::client::QueryOutcome::Rows { columns, rows, .. } => {
                let row = rows.first().context("import returned no row")?;
                let hash = cell(&columns, row, "hash").context("import returned no hash")?;
                let bytes = cell(&columns, row, "bytes")
                    .and_then(|b| b.parse().ok())
                    .unwrap_or(0);
                Ok((hash, bytes))
            }
            crate::client::QueryOutcome::Denied(d) => {
                bail!("blob import denied: {}", d.reason.unwrap_or_default())
            }
            crate::client::QueryOutcome::Failed(e) => {
                bail!(
                    "blob import failed [{}]: {}",
                    e.code,
                    e.reason.unwrap_or_default()
                )
            }
            other => bail!("unexpected blob import response: {other:?}"),
        }
    } else {
        let (hash, bytes) = resonator_transport::blob_import(&dir.join("blobs"), wasm_path)
            .await
            .map_err(|e| anyhow::anyhow!("importing {}: {e}", wasm_path.display()))?;
        Ok((format!("blake3:{hash}"), bytes))
    }
}

fn cell(columns: &[String], row: &resonator_protocol::Row, name: &str) -> Option<String> {
    let i = columns.iter().position(|c| c == name)?;
    row.cells
        .iter()
        .find(|(n, _)| n == &columns[i])
        .and_then(|(_, v)| match v {
            Value::Text(t) => Some(t.clone()),
            Value::Integer(n) => Some(n.to_string()),
            _ => None,
        })
}

/// This node's serving addresses (its daemon's blob provider), so the
/// target fetches the wasm from the right endpoint. Empty when not
/// serving.
fn own_serving_addrs(dir: &Path) -> Vec<String> {
    let Ok(conn) = crate::store::open_db(dir) else {
        return Vec::new();
    };
    resonator_node::get_rsntr(&conn, "serving_addrs")
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .unwrap_or_default()
}

/// Builds the `rsntr:ModInstall` Turtle signal.
#[allow(clippy::too_many_arguments)]
fn install_turtle(
    name: &str,
    hash: &str,
    bytes: u64,
    caps: &[String],
    addrs: &[String],
    timeout_ms: Option<u64>,
    memory_mb: Option<u64>,
    note: Option<&str>,
) -> String {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut t = format!(
        "[] a rsntr:ModInstall ;\n  rsntr:name \"{}\" ;\n  rsntr:hash \"{}\" ;\n  rsntr:bytes {}",
        esc(name),
        esc(hash),
        bytes
    );
    for c in caps {
        t.push_str(&format!(" ;\n  rsntr:caps \"{}\"", esc(c)));
    }
    for a in addrs {
        t.push_str(&format!(" ;\n  rsntr:addr \"{}\"", esc(a)));
    }
    if let Some(ms) = timeout_ms {
        t.push_str(&format!(" ;\n  rsntr:timeoutMs {ms}"));
    }
    if let Some(mb) = memory_mb {
        t.push_str(&format!(" ;\n  rsntr:memoryMb {mb}"));
    }
    if let Some(n) = note {
        t.push_str(&format!(" ;\n  rsntr:note \"{}\"", esc(n)));
    }
    t.push_str(" .\n");
    t
}

/// `rsntr mod push <peer> <name> <file>`: import the wasm locally, then
/// send the `mod-install` Execute to `peer`.
#[allow(clippy::too_many_arguments)]
pub async fn mod_push(
    dir: &Path,
    peer: &str,
    name: &str,
    wasm_path: &Path,
    caps: &[String],
    timeout_ms: Option<u64>,
    memory_mb: Option<u64>,
    note: Option<&str>,
    offline: bool,
) -> Result<crate::client::QueryReport> {
    check_name(name)?;
    let (hash, bytes) = import_own_wasm(dir, wasm_path).await?;
    let addrs = own_serving_addrs(dir);
    let turtle = install_turtle(
        name, &hash, bytes, caps, &addrs, timeout_ms, memory_mb, note,
    );
    crate::client::run_statement(
        dir,
        peer,
        resonator_protocol::RequestKind::Execute,
        "mod-install",
        &turtle,
        vec![],
        offline,
        None,
    )
    .await
}

/// `rsntr mod rm --peer <peer> <name>`: send the `mod-install` remove
/// verb to `peer`.
pub async fn mod_remove_remote(
    dir: &Path,
    peer: &str,
    name: &str,
    offline: bool,
) -> Result<crate::client::QueryReport> {
    check_name(name)?;
    let esc = name.replace('\\', "\\\\").replace('"', "\\\"");
    let turtle = format!("[] a rsntr:ModRemove ;\n  rsntr:name \"{esc}\" .\n");
    crate::client::run_statement(
        dir,
        peer,
        resonator_protocol::RequestKind::Execute,
        "mod-install",
        &turtle,
        vec![],
        offline,
        None,
    )
    .await
}
