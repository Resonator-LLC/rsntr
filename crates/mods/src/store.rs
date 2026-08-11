//! `_modulations` row management: the storage side of `rsntr mod
//! add/list/enable/disable/rm`. The table itself is owned by
//! `resonator-node` (its DDL rides in `ensure_node_tables`); these
//! functions only read and write rows.

use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::error::ModError;

/// One `_modulations` row as listed (without the wasm blob).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModRow {
    pub name: String,
    pub sha256: String,
    pub enabled: bool,
    pub caps: Vec<String>,
    pub note: Option<String>,
}

/// Lowercase hex sha256 of a wasm blob (the `_modulations.sha256` form).
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn check_name(name: &str) -> Result<(), ModError> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if ok {
        Ok(())
    } else {
        Err(ModError::reject(format!(
            "invalid mod name {name:?}: use ascii letters, digits, '-', '_', '.'"
        )))
    }
}

/// Registers (or replaces) a mod: stores the wasm blob, its sha256, and
/// the granted capabilities. New rows start disabled; replacing keeps the
/// row's enabled bit. Returns the stored sha256.
pub fn mod_add(
    conn: &Connection,
    name: &str,
    wasm: &[u8],
    caps: &[String],
    note: Option<&str>,
) -> Result<String, ModError> {
    check_name(name)?;
    resonator_node::ensure_modulations_table(conn)?;
    let sha = sha256_hex(wasm);
    let caps_json = serde_json::to_string(caps)?;
    conn.execute(
        "INSERT INTO _modulations (name, wasm, sha256, enabled, caps, note) \
         VALUES (?1, ?2, ?3, 0, ?4, ?5) \
         ON CONFLICT(name) DO UPDATE SET wasm = ?2, sha256 = ?3, caps = ?4, note = ?5",
        (name, wasm, &sha, &caps_json, note),
    )?;
    Ok(sha)
}

/// All `_modulations` rows, without the blobs.
pub fn mod_list(conn: &Connection) -> Result<Vec<ModRow>, ModError> {
    resonator_node::ensure_modulations_table(conn)?;
    let mut stmt =
        conn.prepare("SELECT name, sha256, enabled, caps, note FROM _modulations ORDER BY name")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .map(|(name, sha256, enabled, caps, note)| ModRow {
            name,
            sha256,
            enabled: enabled != 0,
            caps: serde_json::from_str(&caps).unwrap_or_default(),
            note,
        })
        .collect())
}

/// Flips a mod's enabled bit; false when no such row exists. Takes
/// effect on the next `rsntr serve` start (the registry loads once).
pub fn mod_set_enabled(conn: &Connection, name: &str, enabled: bool) -> Result<bool, ModError> {
    resonator_node::ensure_modulations_table(conn)?;
    let n = conn.execute(
        "UPDATE _modulations SET enabled = ?2 WHERE name = ?1",
        (name, i64::from(enabled)),
    )?;
    Ok(n > 0)
}

/// Deletes a mod row; false when no such row exists.
pub fn mod_rm(conn: &Connection, name: &str) -> Result<bool, ModError> {
    resonator_node::ensure_modulations_table(conn)?;
    let n = conn.execute("DELETE FROM _modulations WHERE name = ?1", [name])?;
    Ok(n > 0)
}

/// The stored wasm blob of one row, with its recorded sha256.
pub(crate) fn mod_wasm(
    conn: &Connection,
    name: &str,
) -> Result<Option<(Vec<u8>, String)>, ModError> {
    resonator_node::ensure_modulations_table(conn)?;
    Ok(conn
        .query_row(
            "SELECT wasm, sha256 FROM _modulations WHERE name = ?1",
            [name],
            |r| Ok((r.get::<_, Option<Vec<u8>>>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
        .and_then(|(wasm, sha)| wasm.map(|w| (w, sha))))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_list_toggle_rm_round_trip() {
        let conn = Connection::open_in_memory().unwrap();
        let sha = mod_add(&conn, "time", b"wasm-bytes", &["clock".into()], Some("n")).unwrap();
        assert_eq!(sha, sha256_hex(b"wasm-bytes"));
        let rows = mod_list(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].enabled);
        assert_eq!(rows[0].caps, vec!["clock".to_string()]);
        assert_eq!(rows[0].note.as_deref(), Some("n"));

        assert!(mod_set_enabled(&conn, "time", true).unwrap());
        assert!(mod_list(&conn).unwrap()[0].enabled);

        // Replacing updates blob+hash+caps and keeps the enabled bit.
        let sha2 = mod_add(&conn, "time", b"other", &[], None).unwrap();
        assert_ne!(sha, sha2);
        let rows = mod_list(&conn).unwrap();
        assert!(rows[0].enabled);
        assert!(rows[0].caps.is_empty());

        assert!(!mod_set_enabled(&conn, "ghost", true).unwrap());
        assert!(mod_rm(&conn, "time").unwrap());
        assert!(!mod_rm(&conn, "time").unwrap());
        assert!(mod_add(&conn, "bad name!", b"x", &[], None).is_err());
    }
}
