//! Submissions waiting for their host (`pending_submits`, migration 12):
//! a delta submitted here to a wavelet hosted by another server (or by
//! another device of this person) that could not be reached is kept,
//! signed, until the host applies it. See
//! [`crate::waveserver::WaveServer::drain_pending`].

use rusqlite::params;

use super::{Database, StorageError};

/// A row of `pending_submits`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSubmit {
    pub seq: i64,
    /// The wavelet's `wave://` URI.
    pub wavelet: String,
    pub author: String,
    /// The federation request id the delta is sent with, every time.
    pub request_id: String,
    /// The serialised `ProtocolSignedDelta`.
    pub signed_delta: Vec<u8>,
    pub target_version: u64,
    pub created_at: i64,
    pub attempts: u32,
    pub last_error: Option<String>,
}

/// The pending submissions; cheap to clone.
#[derive(Debug, Clone)]
pub struct PendingStore {
    db: Database,
}

const COLUMNS: &str = "seq, wavelet, author, request_id, signed_delta, target_version, \
                       created_at, attempts, last_error";

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingSubmit> {
    Ok(PendingSubmit {
        seq: row.get(0)?,
        wavelet: row.get(1)?,
        author: row.get(2)?,
        request_id: row.get(3)?,
        signed_delta: row.get(4)?,
        target_version: row.get::<_, i64>(5)?.max(0) as u64,
        created_at: row.get(6)?,
        attempts: row.get::<_, i64>(7)?.clamp(0, i64::from(u32::MAX)) as u32,
        last_error: row.get(8)?,
    })
}

impl PendingStore {
    pub(crate) fn new(db: Database) -> Self {
        PendingStore { db }
    }

    /// Queues a submission; returns its seq.
    pub async fn push(
        &self,
        wavelet: String,
        author: String,
        request_id: String,
        signed_delta: Vec<u8>,
        target_version: u64,
        created_at: i64,
    ) -> Result<i64, StorageError> {
        let target = i64::try_from(target_version)
            .map_err(|_| StorageError::Invalid(format!("version {target_version} too large")))?;
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "INSERT INTO pending_submits (wavelet, author, request_id, signed_delta, \
                     target_version, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )?
                .execute(params![
                    wavelet,
                    author,
                    request_id,
                    signed_delta,
                    target,
                    created_at
                ])?;
                Ok(conn.last_insert_rowid())
            })
            .await
    }

    /// Every pending submission, oldest first.
    pub async fn all(&self) -> Result<Vec<PendingSubmit>, StorageError> {
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(&format!(
                    "SELECT {COLUMNS} FROM pending_submits ORDER BY seq"
                ))?;
                let rows = statement.query_map([], row)?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
    }

    /// Whether a wavelet has pending submissions.
    pub async fn has_wavelet(&self, wavelet: &str) -> Result<bool, StorageError> {
        let wavelet = wavelet.to_owned();
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached("SELECT 1 FROM pending_submits WHERE wavelet = ?1 LIMIT 1")?
                    .exists(params![wavelet])?)
            })
            .await
    }

    /// Removes a submission the host has answered.
    pub async fn remove(&self, seq: i64) -> Result<(), StorageError> {
        self.db
            .write(move |conn| {
                conn.prepare_cached("DELETE FROM pending_submits WHERE seq = ?1")?
                    .execute(params![seq])?;
                Ok(())
            })
            .await
    }

    /// Notes a failed attempt.
    pub async fn failed(&self, seq: i64, error: String) -> Result<(), StorageError> {
        self.db
            .write(move |conn| {
                conn.prepare_cached(
                    "UPDATE pending_submits SET attempts = attempts + 1, last_error = ?2 \
                     WHERE seq = ?1",
                )?
                .execute(params![seq, error])?;
                Ok(())
            })
            .await
    }
}
