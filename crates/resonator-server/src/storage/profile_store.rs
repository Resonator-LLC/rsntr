//! Names and avatars (`profiles`, migration 20): those of this domain's
//! accounts (`own`) and copies fetched from other domains. A version
//! replaces the stored one only if it is newer (`updated_at`), so the
//! devices of a person and the copies elsewhere settle on the same one.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};

use super::{Database, StorageError};

/// A profile without its avatar bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredProfile {
    pub address: String,
    /// Empty: the name derived from the address.
    pub name: String,
    /// SHA-256 of the avatar; `None` without one.
    pub avatar_sha256: Option<Vec<u8>>,
    pub updated_at: i64,
    pub own: bool,
    pub fetched_at: i64,
}

/// A whole version of a profile, as set or as sent between nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRecord {
    pub address: String,
    pub name: String,
    /// A 256x256 PNG; `None` without one.
    pub avatar: Option<Vec<u8>>,
    pub updated_at: i64,
}

/// The `profiles` table; cheap to clone.
#[derive(Debug, Clone)]
pub struct ProfileStore {
    db: Database,
}

fn stored(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredProfile> {
    Ok(StoredProfile {
        address: row.get(0)?,
        name: row.get(1)?,
        avatar_sha256: row.get(2)?,
        updated_at: row.get(3)?,
        own: row.get::<_, i64>(4)? != 0,
        fetched_at: row.get(5)?,
    })
}

const COLUMNS: &str = "address, name, avatar_sha256, updated_at, own, fetched_at";

impl ProfileStore {
    pub(crate) fn new(db: Database) -> Self {
        ProfileStore { db }
    }

    pub async fn get(&self, address: &str) -> Result<Option<StoredProfile>, StorageError> {
        let address = address.to_owned();
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(&format!(
                        "SELECT {COLUMNS} FROM profiles WHERE address = ?1"
                    ))?
                    .query_row(params![address], stored)
                    .optional()?)
            })
            .await
    }

    /// The stored profiles among `addresses`, by address.
    pub async fn get_many(
        &self,
        addresses: &[String],
    ) -> Result<HashMap<String, StoredProfile>, StorageError> {
        let addresses = addresses.to_vec();
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(&format!(
                    "SELECT {COLUMNS} FROM profiles WHERE address = ?1"
                ))?;
                let mut found = HashMap::new();
                for address in addresses {
                    if let Some(profile) =
                        statement.query_row(params![address], stored).optional()?
                    {
                        found.insert(address, profile);
                    }
                }
                Ok(found)
            })
            .await
    }

    /// The whole version of a profile, avatar included.
    pub async fn record(&self, address: &str) -> Result<Option<ProfileRecord>, StorageError> {
        let address = address.to_owned();
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(
                        "SELECT address, name, avatar, updated_at FROM profiles WHERE address = ?1",
                    )?
                    .query_row(params![address], |row| {
                        Ok(ProfileRecord {
                            address: row.get(0)?,
                            name: row.get(1)?,
                            avatar: row.get(2)?,
                            updated_at: row.get(3)?,
                        })
                    })
                    .optional()?)
            })
            .await
    }

    /// The avatar PNG and its hash.
    pub async fn avatar(&self, address: &str) -> Result<Option<(Vec<u8>, Vec<u8>)>, StorageError> {
        let address = address.to_owned();
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(
                        "SELECT avatar, avatar_sha256 FROM profiles \
                         WHERE address = ?1 AND avatar IS NOT NULL",
                    )?
                    .query_row(params![address], |row| Ok((row.get(0)?, row.get(1)?)))
                    .optional()?)
            })
            .await
    }

    /// The own profiles, without avatars (for comparing between devices).
    pub async fn own(&self) -> Result<Vec<StoredProfile>, StorageError> {
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(&format!(
                    "SELECT {COLUMNS} FROM profiles WHERE own = 1 ORDER BY address"
                ))?;
                let rows = statement
                    .query_map([], stored)?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await
    }

    /// Keeps `record` if it is newer than the stored version (or there is
    /// none); returns whether it was kept. `fetched_at` is noted either way.
    pub async fn put(
        &self,
        record: ProfileRecord,
        own: bool,
        fetched_at: i64,
    ) -> Result<bool, StorageError> {
        let hash = record
            .avatar
            .as_deref()
            .map(|png| <sha2::Sha256 as sha2::Digest>::digest(png).to_vec());
        self.db
            .write(move |conn| {
                // Writes are serialised, so the two statements see no
                // other write in between.
                let kept =
                    conn.prepare_cached(
                        "INSERT INTO profiles \
                         (address, name, avatar, avatar_sha256, updated_at, own, fetched_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                         ON CONFLICT (address) DO UPDATE SET name = excluded.name, \
                         avatar = excluded.avatar, avatar_sha256 = excluded.avatar_sha256, \
                         updated_at = excluded.updated_at, own = excluded.own, \
                         fetched_at = excluded.fetched_at \
                         WHERE excluded.updated_at > profiles.updated_at",
                    )?
                    .execute(params![
                        record.address,
                        record.name,
                        record.avatar,
                        hash,
                        record.updated_at,
                        i64::from(own),
                        fetched_at
                    ])? > 0;
                if !kept {
                    conn.prepare_cached("UPDATE profiles SET fetched_at = ?2 WHERE address = ?1")?
                        .execute(params![record.address, fetched_at])?;
                }
                Ok(kept)
            })
            .await
    }
}
