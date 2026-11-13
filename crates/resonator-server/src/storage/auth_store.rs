//! Sessions and personal access tokens in SQLite. Only hashes of the
//! secrets are stored; the authentication layer ([`crate::auth`]) owns
//! their format and meaning. Times are milliseconds since the epoch.

use resonator_model::id::ParticipantId;
use rusqlite::{OptionalExtension, Row, params};

use super::{Database, StorageError};

/// A stored browser session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    /// SHA-256 of the session id in the cookie.
    pub id_hash: [u8; 32],
    pub participant: ParticipantId,
    /// SHA-256 of the session's WebSocket token.
    pub socket_token_hash: [u8; 32],
    pub created_at: i64,
    pub last_seen_at: i64,
    pub expires_at: i64,
    pub absolute_expires_at: i64,
}

/// A stored personal access token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessTokenRecord {
    pub id: i64,
    pub participant: ParticipantId,
    pub name: String,
    /// Space-separated scope names.
    pub scopes: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub last_used_at: Option<i64>,
}

/// Stores sessions and access tokens.
#[derive(Debug, Clone)]
pub struct AuthStore {
    db: Database,
}

const SESSION_COLUMNS: &str = "id_hash, address, socket_token_hash, created_at, last_seen_at, \
                               expires_at, absolute_expires_at";
const TOKEN_COLUMNS: &str = "id, address, name, scopes, created_at, expires_at, last_used_at";

fn session_from_row(row: &Row<'_>) -> rusqlite::Result<SessionRecord> {
    Ok(SessionRecord {
        id_hash: row.get(0)?,
        participant: ParticipantId::new_unchecked(&row.get::<_, String>(1)?),
        socket_token_hash: row.get(2)?,
        created_at: row.get(3)?,
        last_seen_at: row.get(4)?,
        expires_at: row.get(5)?,
        absolute_expires_at: row.get(6)?,
    })
}

fn token_from_row(row: &Row<'_>) -> rusqlite::Result<AccessTokenRecord> {
    Ok(AccessTokenRecord {
        id: row.get(0)?,
        participant: ParticipantId::new_unchecked(&row.get::<_, String>(1)?),
        name: row.get(2)?,
        scopes: row.get(3)?,
        created_at: row.get(4)?,
        expires_at: row.get(5)?,
        last_used_at: row.get(6)?,
    })
}

impl AuthStore {
    pub(crate) fn new(db: Database) -> Self {
        AuthStore { db }
    }

    pub async fn create_session(&self, session: SessionRecord) -> Result<(), StorageError> {
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "INSERT INTO sessions (id_hash, address, socket_token_hash, created_at, \
                     last_seen_at, expires_at, absolute_expires_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )?
                .execute(params![
                    session.id_hash,
                    session.participant.address(),
                    session.socket_token_hash,
                    session.created_at,
                    session.last_seen_at,
                    session.expires_at,
                    session.absolute_expires_at
                ])?;
                Ok(())
            })
            .await
    }

    /// The session with this id hash, expired or not.
    pub async fn session(&self, id_hash: [u8; 32]) -> Result<Option<SessionRecord>, StorageError> {
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(&format!(
                        "SELECT {SESSION_COLUMNS} FROM sessions WHERE id_hash = ?1"
                    ))?
                    .query_row([id_hash], session_from_row)
                    .optional()?)
            })
            .await
    }

    /// The session whose WebSocket token has this hash.
    pub async fn session_by_socket_token(
        &self,
        socket_token_hash: [u8; 32],
    ) -> Result<Option<SessionRecord>, StorageError> {
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(&format!(
                        "SELECT {SESSION_COLUMNS} FROM sessions WHERE socket_token_hash = ?1"
                    ))?
                    .query_row([socket_token_hash], session_from_row)
                    .optional()?)
            })
            .await
    }

    /// Records a use of a session and its new idle expiry.
    pub async fn touch_session(
        &self,
        id_hash: [u8; 32],
        last_seen_at: i64,
        expires_at: i64,
    ) -> Result<(), StorageError> {
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "UPDATE sessions SET last_seen_at = ?2, expires_at = ?3 WHERE id_hash = ?1",
                )?
                .execute(params![id_hash, last_seen_at, expires_at])?;
                Ok(())
            })
            .await
    }

    /// Deletes a session; returns whether it existed.
    pub async fn delete_session(&self, id_hash: [u8; 32]) -> Result<bool, StorageError> {
        self.db
            .write(move |conn| {
                Ok(conn
                    .prepare_cached("DELETE FROM sessions WHERE id_hash = ?1")?
                    .execute([id_hash])?
                    > 0)
            })
            .await
    }

    /// Deletes every session of a participant; returns how many.
    pub async fn delete_sessions_of(
        &self,
        participant: &ParticipantId,
    ) -> Result<usize, StorageError> {
        let address = participant.address().to_owned();
        self.db
            .write(move |conn| {
                Ok(conn
                    .prepare_cached("DELETE FROM sessions WHERE address = ?1")?
                    .execute([address])?)
            })
            .await
    }

    /// Deletes the sessions that expired before `now`; returns how many.
    pub async fn delete_expired_sessions(&self, now: i64) -> Result<usize, StorageError> {
        self.db
            .write(move |conn| {
                Ok(conn
                    .prepare_cached(
                        "DELETE FROM sessions WHERE expires_at <= ?1 OR absolute_expires_at <= ?1",
                    )?
                    .execute([now])?)
            })
            .await
    }

    pub async fn session_count(&self) -> Result<u64, StorageError> {
        self.db
            .read(|conn| {
                let count: i64 =
                    conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
                Ok(count.max(0) as u64)
            })
            .await
    }

    /// Stores a new access token and returns its record.
    pub async fn create_token(
        &self,
        token_hash: [u8; 32],
        participant: &ParticipantId,
        name: &str,
        scopes: &str,
        created_at: i64,
        expires_at: Option<i64>,
    ) -> Result<AccessTokenRecord, StorageError> {
        let participant = participant.clone();
        let (name, scopes) = (name.to_owned(), scopes.to_owned());
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "INSERT INTO access_tokens (token_hash, address, name, scopes, created_at, \
                     expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )?
                .execute(params![
                    token_hash,
                    participant.address(),
                    name,
                    scopes,
                    created_at,
                    expires_at
                ])?;
                Ok(AccessTokenRecord {
                    id: conn.last_insert_rowid(),
                    participant,
                    name,
                    scopes,
                    created_at,
                    expires_at,
                    last_used_at: None,
                })
            })
            .await
    }

    /// The access token with this hash, expired or not.
    pub async fn token_by_hash(
        &self,
        token_hash: [u8; 32],
    ) -> Result<Option<AccessTokenRecord>, StorageError> {
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(&format!(
                        "SELECT {TOKEN_COLUMNS} FROM access_tokens WHERE token_hash = ?1"
                    ))?
                    .query_row([token_hash], token_from_row)
                    .optional()?)
            })
            .await
    }

    pub async fn token(&self, id: i64) -> Result<Option<AccessTokenRecord>, StorageError> {
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(&format!(
                        "SELECT {TOKEN_COLUMNS} FROM access_tokens WHERE id = ?1"
                    ))?
                    .query_row([id], token_from_row)
                    .optional()?)
            })
            .await
    }

    /// A participant's access tokens, oldest first.
    pub async fn tokens_of(
        &self,
        participant: &ParticipantId,
    ) -> Result<Vec<AccessTokenRecord>, StorageError> {
        let address = participant.address().to_owned();
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(&format!(
                    "SELECT {TOKEN_COLUMNS} FROM access_tokens WHERE address = ?1 ORDER BY id"
                ))?;
                let rows = statement.query_map([address], token_from_row)?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    /// Deletes a participant's token; returns whether it existed.
    pub async fn delete_token(
        &self,
        participant: &ParticipantId,
        id: i64,
    ) -> Result<bool, StorageError> {
        let address = participant.address().to_owned();
        self.db
            .write(move |conn| {
                Ok(conn
                    .prepare_cached("DELETE FROM access_tokens WHERE id = ?1 AND address = ?2")?
                    .execute(params![id, address])?
                    > 0)
            })
            .await
    }

    pub async fn touch_token(&self, id: i64, last_used_at: i64) -> Result<(), StorageError> {
        self.db
            .write(move |conn| {
                conn.prepare_cached("UPDATE access_tokens SET last_used_at = ?2 WHERE id = ?1")?
                    .execute(params![id, last_used_at])?;
                Ok(())
            })
            .await
    }

    /// The robot or agent whose account holds this access token hash
    /// (lower-case hex).
    pub async fn robot_by_token_hash(
        &self,
        token_hash_hex: String,
    ) -> Result<Option<ParticipantId>, StorageError> {
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(
                        "SELECT address FROM accounts WHERE access_token_hash = ?1 \
                         AND kind IN ('robot', 'agent')",
                    )?
                    .query_row([token_hash_hex], |row| row.get::<_, String>(0))
                    .optional()?
                    .map(|address| ParticipantId::new_unchecked(&address)))
            })
            .await
    }
}
