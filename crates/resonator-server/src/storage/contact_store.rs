//! Contacts (`contacts`, migration 13), for device mode: the people an
//! account added by hand, together with everyone it shares a
//! conversation with, read from the search index (`search_participants`).

use rusqlite::params;

use super::{Database, StorageError};

/// A contact as listed: its address, its account kind here (`human` if it
/// has no account here, as for people of other domains) and whether it
/// was added by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub address: String,
    pub kind: String,
    pub added: bool,
}

/// The contacts of the accounts here; cheap to clone.
#[derive(Debug, Clone)]
pub struct ContactStore {
    db: Database,
}

/// A `LIKE` pattern matching addresses that contain `query`, lower-cased
/// (`\` escapes).
fn contains_pattern(query: &str) -> String {
    let mut pattern = String::with_capacity(query.len() + 2);
    pattern.push('%');
    for c in query.to_lowercase().chars() {
        if matches!(c, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    pattern
}

impl ContactStore {
    pub(crate) fn new(db: Database) -> Self {
        ContactStore { db }
    }

    /// Adds `address` to the contacts of `owner`; returns whether it was
    /// not there yet.
    pub async fn add(&self, owner: &str, address: &str, at: i64) -> Result<bool, StorageError> {
        let (owner, address) = (owner.to_owned(), address.to_owned());
        self.db
            .write(move |conn| {
                let changed = conn
                    .prepare_cached(
                        "INSERT OR IGNORE INTO contacts (owner, address, added_at) \
                         VALUES (?1, ?2, ?3)",
                    )?
                    .execute(params![owner, address, at])?;
                Ok(changed > 0)
            })
            .await
    }

    /// Removes `address` from the contacts `owner` added by hand; returns
    /// whether it was there. (People `owner` shares waves with stay.)
    pub async fn remove(&self, owner: &str, address: &str) -> Result<bool, StorageError> {
        let (owner, address) = (owner.to_owned(), address.to_owned());
        self.db
            .write(move |conn| {
                let changed = conn
                    .prepare_cached("DELETE FROM contacts WHERE owner = ?1 AND address = ?2")?
                    .execute(params![owner, address])?;
                Ok(changed > 0)
            })
            .await
    }

    /// The contacts of `owner` whose address contains `query`
    /// (case-insensitive; empty matches all): the addresses added by hand
    /// and the participants of the conversations `owner` takes part in,
    /// without `owner` and shared domain participants (`@domain`); people
    /// first, then by address.
    pub async fn search(
        &self,
        owner: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Contact>, StorageError> {
        let owner = owner.to_owned();
        let pattern = contains_pattern(query);
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(
                    "WITH c(address, added) AS ( \
                         SELECT address, 1 FROM contacts WHERE owner = ?1 \
                         UNION ALL \
                         SELECT DISTINCT p2.participant, 0 \
                         FROM search_participants p1 \
                         JOIN search_wavelets w ON w.id = p1.wavelet AND w.kind = 1 \
                         JOIN search_participants p2 ON p2.wavelet = p1.wavelet \
                         WHERE p1.participant = ?1 \
                     ) \
                     SELECT c.address, COALESCE(a.kind, 'human') AS kind, MAX(c.added) \
                     FROM c LEFT JOIN accounts a ON a.address = c.address \
                     WHERE c.address <> ?1 AND c.address NOT LIKE '@%' \
                       AND lower(c.address) LIKE ?2 ESCAPE '\\' \
                     GROUP BY c.address \
                     ORDER BY kind <> 'human', c.address LIMIT ?3",
                )?;
                let rows = statement
                    .query_map(params![owner, pattern, limit], |row| {
                        Ok(Contact {
                            address: row.get(0)?,
                            kind: row.get(1)?,
                            added: row.get::<_, i64>(2)? != 0,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
    }
}
