//! The mod registry: enabled `_modulations` rows verified, compiled, and
//! described.
//!
//! Loading a row: verify the stored sha256 against the blob, compile via
//! extism (`CompiledPlugin`, cached by hash + limits so identical blobs
//! compile once), instantiate once to call `describe()`, then refuse the
//! row unless the descriptor's ABI is 1, its name matches the row, and
//! the granted caps cover its declared needs. Per-request `Plugin`
//! instances come fresh from the cached compile (no state bleed between
//! requests or peers).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use extism::{CompiledPlugin, Manifest, Plugin, Wasm, convert::Json};
use rusqlite::Connection;
use tracing::warn;

use resonator_mod_pdk::Descriptor;
use resonator_protocol::mod_matches;

use crate::error::ModError;
use crate::host;
use crate::store::sha256_hex;

/// Default wasm memory ceiling when the row's limits JSON does not set
/// `memory_mb`.
pub const DEFAULT_MEMORY_MB: u64 = 64;

const WASM_PAGE_BYTES: u64 = 64 * 1024;

/// Runtime budgets for one mod, from the row's `limits` JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModLimits {
    /// Wall clock per `handle()` call, enforced by the extism runtime.
    pub timeout_ms: u64,
    /// Wasm linear memory ceiling in pages (64 KiB each).
    pub memory_max_pages: u32,
}

/// One enabled `_modulations` row as read from the database (raw JSON
/// columns still unparsed).
#[derive(Debug, Clone)]
pub struct EnabledRow {
    pub name: String,
    pub wasm: Option<Vec<u8>>,
    pub sha256: String,
    pub caps: String,
    pub config: String,
    pub limits: String,
}

/// Reads the enabled rows (blobs included) for [`ModRegistry::from_rows`].
pub fn read_enabled_rows(conn: &Connection) -> Result<Vec<EnabledRow>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT name, wasm, sha256, caps, config, limits \
         FROM _modulations WHERE enabled = 1 ORDER BY name",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(EnabledRow {
                name: r.get(0)?,
                wasm: r.get(1)?,
                sha256: r.get(2)?,
                caps: r.get(3)?,
                config: r.get(4)?,
                limits: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// One loaded mod: descriptor, grants, config, and the cached compile.
pub struct ModEntry {
    pub name: String,
    pub descriptor: Descriptor,
    pub caps: BTreeSet<String>,
    pub config: serde_json::Map<String, serde_json::Value>,
    pub limits: ModLimits,
    compiled: CompiledPlugin,
}

impl ModEntry {
    /// A fresh plugin instance for one request.
    pub fn instantiate(&self) -> Result<Plugin, ModError> {
        Plugin::new_from_compiled(&self.compiled).map_err(ModError::wasm)
    }
}

/// The loaded mods, by name.
#[derive(Default)]
pub struct ModRegistry {
    entries: BTreeMap<String, Arc<ModEntry>>,
}

impl ModRegistry {
    /// Loads the enabled rows of `conn` (compiles on the calling thread;
    /// prefer [`from_rows`](Self::from_rows) off the db thread). Returns
    /// the registry plus the rows that were refused, as (name, reason).
    pub fn load(
        conn: &Connection,
        default_timeout_ms: u64,
    ) -> Result<(Self, Vec<(String, String)>), ModError> {
        Ok(Self::from_rows(
            read_enabled_rows(conn)?,
            default_timeout_ms,
        ))
    }

    /// Verifies, compiles, and describes each row. A row that fails is
    /// refused (collected into the second return), never a hard error:
    /// one bad mod must not stop the node from serving.
    pub fn from_rows(
        rows: Vec<EnabledRow>,
        default_timeout_ms: u64,
    ) -> (Self, Vec<(String, String)>) {
        let mut entries = BTreeMap::new();
        let mut refused = Vec::new();
        let mut cache: HashMap<String, CompiledPlugin> = HashMap::new();
        for row in rows {
            let name = row.name.clone();
            match load_entry(row, default_timeout_ms, &mut cache) {
                Ok(entry) => {
                    entries.insert(name, Arc::new(entry));
                }
                Err(e) => {
                    warn!(mod_name = %name, error = %e, "refusing to load mod");
                    refused.push((name, e.to_string()));
                }
            }
        }
        (Self { entries }, refused)
    }

    /// The entry whose name matches a requested modulation tag (exact,
    /// or version-suffix per `mod_matches`).
    pub fn find(&self, requested: &str) -> Option<Arc<ModEntry>> {
        self.entries
            .values()
            .find(|e| mod_matches(requested, &e.name))
            .cloned()
    }

    /// The loaded mod names, sorted.
    pub fn names(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Parses a row's `limits` JSON (`{"timeout_ms": N, "memory_mb": M}`).
fn parse_limits(raw: &str, default_timeout_ms: u64) -> Result<ModLimits, ModError> {
    let v: serde_json::Value = if raw.trim().is_empty() {
        serde_json::Value::Object(Default::default())
    } else {
        serde_json::from_str(raw)
            .map_err(|e| ModError::reject(format!("limits column is not JSON: {e}")))?
    };
    let timeout_ms = v
        .get("timeout_ms")
        .and_then(|t| t.as_u64())
        .filter(|t| *t > 0)
        .unwrap_or(default_timeout_ms);
    let memory_mb = v
        .get("memory_mb")
        .and_then(|m| m.as_u64())
        .filter(|m| *m > 0)
        .unwrap_or(DEFAULT_MEMORY_MB);
    let pages = (memory_mb * 1024 * 1024).div_ceil(WASM_PAGE_BYTES);
    Ok(ModLimits {
        timeout_ms,
        memory_max_pages: u32::try_from(pages)
            .map_err(|_| ModError::reject("memory_mb limit is out of range"))?,
    })
}

/// Verify + compile + describe one row; see the module docs for the
/// refusal rules.
fn load_entry(
    row: EnabledRow,
    default_timeout_ms: u64,
    cache: &mut HashMap<String, CompiledPlugin>,
) -> Result<ModEntry, ModError> {
    let wasm = row
        .wasm
        .ok_or_else(|| ModError::reject("row stores no wasm blob"))?;
    let sha = sha256_hex(&wasm);
    if !sha.eq_ignore_ascii_case(row.sha256.trim()) {
        return Err(ModError::reject(format!(
            "sha256 mismatch: row says {}, blob is {sha}",
            row.sha256
        )));
    }
    let caps: BTreeSet<String> = serde_json::from_str::<Vec<String>>(&row.caps)
        .map_err(|e| ModError::reject(format!("caps column is not a JSON string array: {e}")))?
        .into_iter()
        .collect();
    let config: serde_json::Map<String, serde_json::Value> = if row.config.trim().is_empty() {
        Default::default()
    } else {
        serde_json::from_str(&row.config)
            .map_err(|e| ModError::reject(format!("config column is not a JSON object: {e}")))?
    };
    let limits = parse_limits(&row.limits, default_timeout_ms)?;

    let compiled = compile(&wasm, &sha, limits, cache)?;
    let descriptor = describe_compiled(&compiled)?;
    if descriptor.abi != 1 {
        return Err(ModError::reject(format!(
            "unsupported ABI version {} (this host speaks 1)",
            descriptor.abi
        )));
    }
    if descriptor.name != row.name {
        return Err(ModError::reject(format!(
            "descriptor name {:?} does not match the row name {:?}",
            descriptor.name, row.name
        )));
    }
    let missing: Vec<&str> = descriptor
        .needs
        .iter()
        .filter(|n| !caps.contains(n.as_str()))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        return Err(ModError::reject(format!(
            "granted caps do not cover declared needs: missing [{}] \
             (grant with `rsntr mod add ... --cap <name>`)",
            missing.join(", ")
        )));
    }

    Ok(ModEntry {
        name: row.name,
        descriptor,
        caps,
        config,
        limits,
        compiled,
    })
}

/// Compiles a blob under its limits, reusing the cache across rows.
fn compile(
    wasm: &[u8],
    sha: &str,
    limits: ModLimits,
    cache: &mut HashMap<String, CompiledPlugin>,
) -> Result<CompiledPlugin, ModError> {
    let key = format!("{sha}:{}:{}", limits.timeout_ms, limits.memory_max_pages);
    if let Some(hit) = cache.get(&key) {
        return Ok(hit.clone());
    }
    let manifest = Manifest::new([Wasm::data(wasm.to_vec())])
        .with_timeout(Duration::from_millis(limits.timeout_ms))
        .with_memory_max(limits.memory_max_pages);
    let compiled = CompiledPlugin::new(host::plugin_builder(manifest)).map_err(ModError::wasm)?;
    cache.insert(key, compiled.clone());
    Ok(compiled)
}

/// Calls the `describe()` export on a fresh instance.
fn describe_compiled(compiled: &CompiledPlugin) -> Result<Descriptor, ModError> {
    let mut plugin = Plugin::new_from_compiled(compiled).map_err(ModError::wasm)?;
    let Json(descriptor) = plugin
        .call::<(), Json<Descriptor>>("describe", ())
        .map_err(|e| ModError::Wasm(format!("describe() failed: {e}")))?;
    Ok(descriptor)
}

/// `rsntr mod describe <name>`: verifies and compiles the stored blob
/// (enabled or not) and returns its descriptor. Capability coverage is
/// deliberately not checked: describing is how the owner learns the
/// mod's needs before granting them.
pub fn describe_stored(
    conn: &Connection,
    name: &str,
    default_timeout_ms: u64,
) -> Result<Descriptor, ModError> {
    let (wasm, recorded) = crate::store::mod_wasm(conn, name)?
        .ok_or_else(|| ModError::reject(format!("no mod named {name:?} (or no wasm stored)")))?;
    let sha = sha256_hex(&wasm);
    if !sha.eq_ignore_ascii_case(recorded.trim()) {
        return Err(ModError::reject(format!(
            "sha256 mismatch: row says {recorded}, blob is {sha}"
        )));
    }
    let limits = parse_limits("{}", default_timeout_ms)?;
    let compiled = compile(&wasm, &sha, limits, &mut HashMap::new())?;
    describe_compiled(&compiled)
}

/// Describes a wasm blob handed in directly: the owner-channel `rsntr
/// mod describe`, where the registry row was read over an envelope
/// rather than a database connection.
pub fn describe_wasm(wasm: &[u8], default_timeout_ms: u64) -> Result<Descriptor, ModError> {
    let sha = sha256_hex(wasm);
    let limits = parse_limits("{}", default_timeout_ms)?;
    let compiled = compile(wasm, &sha, limits, &mut HashMap::new())?;
    describe_compiled(&compiled)
}
