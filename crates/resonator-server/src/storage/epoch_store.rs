//! Host epochs of wavelets and the deltas a rewind orphaned
//! (`wavelet_epochs` and `orphan_deltas`), and the rewind itself: one
//! write transaction that cuts a wavelet's history back to the start of a
//! newer epoch.
//!
//! Wavelets are named by their `wave://` URI here, as in the federation
//! tables.

use resonator_model::id::{IdUriCodec, WaveletName};
use resonator_model::version::HashedVersion;
use rusqlite::{Connection, params};

use super::delta_store::{DeltaStore, wavelet_row_id};
use super::{StorageError, WriteHandle};

/// A row of `wavelet_epochs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEpoch {
    /// The wavelet's `wave://` URI.
    pub wavelet: String,
    pub epoch: u64,
    pub start: HashedVersion,
    /// The holder's public key.
    pub holder: Vec<u8>,
    pub holder_domain: String,
    /// The serialised signed statement.
    pub statement: Vec<u8>,
}

/// A row of `orphan_deltas`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanDelta {
    pub wavelet: String,
    /// SHA-256 of the serialised signed original delta.
    pub sha256: Vec<u8>,
    pub fork: HashedVersion,
    pub seq: u64,
    /// The serialised `ProtocolAppliedWaveletDelta`, exactly as hashed.
    pub applied: Vec<u8>,
}

/// What a rewind changes besides the history itself.
#[derive(Debug, Clone)]
pub struct Rewind {
    /// The epoch that causes it, recorded in the same transaction.
    pub epoch: Option<StoredEpoch>,
    /// The deltas taken out, to be resubmitted.
    pub orphans: Vec<OrphanDelta>,
    /// The remote domain now hosting the wavelet, `None` if this server
    /// does.
    pub hosted_by: Option<String>,
    /// Milliseconds since the epoch.
    pub now: i64,
}

fn sql(version: u64) -> Result<i64, StorageError> {
    i64::try_from(version)
        .map_err(|_| StorageError::Invalid(format!("version {version} out of range")))
}

fn uri(name: &WaveletName) -> String {
    IdUriCodec::wavelet_name_to_uri(name)
}

/// Deletes the deltas applied at `version` or later and the snapshots at
/// `version` or later. Returns how many deltas went.
pub(crate) fn truncate(
    conn: &Connection,
    name: &WaveletName,
    version: u64,
) -> Result<usize, StorageError> {
    let Some(id) = wavelet_row_id(conn, name)? else {
        return Ok(0);
    };
    let version = sql(version)?;
    let deleted = conn
        .prepare_cached("DELETE FROM deltas WHERE wavelet = ?1 AND applied_at_version >= ?2")?
        .execute(params![id, version])?;
    conn.prepare_cached("DELETE FROM snapshots WHERE wavelet = ?1 AND version >= ?2")?
        .execute(params![id, version])?;
    Ok(deleted)
}

fn insert_epoch(conn: &Connection, epoch: &StoredEpoch) -> Result<(), StorageError> {
    conn.prepare_cached(
        "INSERT OR REPLACE INTO wavelet_epochs (wavelet, epoch, start_version, start_hash, \
         holder, holder_domain, statement) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?
    .execute(params![
        epoch.wavelet,
        sql(epoch.epoch)?,
        sql(epoch.start.version())?,
        epoch.start.history_hash(),
        epoch.holder,
        epoch.holder_domain,
        epoch.statement
    ])?;
    Ok(())
}

fn set_hosted_by(
    conn: &Connection,
    name: &WaveletName,
    hosted_by: Option<&str>,
) -> Result<(), StorageError> {
    if let Some(id) = wavelet_row_id(conn, name)? {
        conn.prepare_cached("UPDATE wavelets SET hosted_by = ?2 WHERE id = ?1")?
            .execute(params![id, hosted_by])?;
    }
    Ok(())
}

impl DeltaStore {
    /// Deletes the deltas of a wavelet applied at `version` or later, and
    /// its snapshots at `version` or later, in one write transaction.
    /// `version` must be a delta boundary; the caller sees to it that no
    /// container of the wavelet is loaded or that it is reloaded after.
    pub async fn truncate_from(
        &self,
        wavelet_name: &WaveletName,
        version: u64,
    ) -> Result<usize, StorageError> {
        let name = wavelet_name.clone();
        self.database()
            .write(move |conn| truncate(conn, &name, version))
            .await
    }

    /// Rewinds a wavelet to `fork` in one write transaction: truncates the
    /// history, records the epoch and the orphans, lowers the version of
    /// outbox rows and of the search index row so they follow the shorter
    /// history, and notes the new host.
    pub(crate) async fn rewind(
        &self,
        wavelet_name: &WaveletName,
        fork: &HashedVersion,
        rewind: Rewind,
    ) -> Result<WriteHandle<()>, StorageError> {
        let name = wavelet_name.clone();
        let fork = fork.clone();
        self.database()
            .enqueue_write(move |conn| {
                let at = sql(fork.version())?;
                truncate(conn, &name, fork.version())?;
                if let Some(epoch) = &rewind.epoch {
                    insert_epoch(conn, epoch)?;
                }
                let wavelet = uri(&name);
                for orphan in &rewind.orphans {
                    conn.prepare_cached(
                        "INSERT OR IGNORE INTO orphan_deltas (wavelet, sha256, fork_version, \
                         fork_hash, seq, applied, created_at, status) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending')",
                    )?
                    .execute(params![
                        orphan.wavelet,
                        orphan.sha256,
                        sql(orphan.fork.version())?,
                        orphan.fork.history_hash(),
                        sql(orphan.seq)?,
                        orphan.applied,
                        rewind.now
                    ])?;
                }
                conn.prepare_cached(
                    "UPDATE federation_outbox SET from_version = ?2 \
                     WHERE wavelet = ?1 AND from_version > ?2",
                )?
                .execute(params![wavelet, at])?;
                // The index never goes back to an older version on its own;
                // let the reindex of the rewound wavelet through.
                let [wave_domain, wave_id, wavelet_domain, wavelet_id] = [
                    name.wave_id.domain(),
                    name.wave_id.id(),
                    name.wavelet_id.domain(),
                    name.wavelet_id.id(),
                ];
                conn.prepare_cached(
                    "UPDATE search_wavelets SET version = ?5, \
                     text_version = min(text_version, ?5) \
                     WHERE wave_domain = ?1 AND wave_id = ?2 AND wavelet_domain = ?3 \
                     AND wavelet_id = ?4 AND version > ?5",
                )?
                .execute(params![
                    wave_domain,
                    wave_id,
                    wavelet_domain,
                    wavelet_id,
                    at
                ])?;
                set_hosted_by(conn, &name, rewind.hosted_by.as_deref())?;
                Ok(())
            })
            .await
    }

    /// Records an epoch of a wavelet without changing its history, and
    /// notes its host (`None`: this server).
    pub async fn put_epoch(
        &self,
        wavelet_name: &WaveletName,
        epoch: StoredEpoch,
        hosted_by: Option<String>,
    ) -> Result<(), StorageError> {
        let name = wavelet_name.clone();
        self.database()
            .write(move |conn| {
                insert_epoch(conn, &epoch)?;
                set_hosted_by(conn, &name, hosted_by.as_deref())
            })
            .await
    }

    /// Every recorded epoch, by wavelet and epoch.
    pub async fn epochs(&self) -> Result<Vec<StoredEpoch>, StorageError> {
        self.database()
            .read(|conn| {
                let mut statement = conn.prepare_cached(
                    "SELECT wavelet, epoch, start_version, start_hash, holder, holder_domain, \
                     statement FROM wavelet_epochs ORDER BY wavelet, epoch",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                        row.get::<_, Vec<u8>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Vec<u8>>(6)?,
                    ))
                })?;
                let mut epochs = Vec::new();
                for row in rows {
                    let (wavelet, epoch, start, hash, holder, holder_domain, statement) = row?;
                    epochs.push(StoredEpoch {
                        wavelet,
                        epoch: epoch.max(0) as u64,
                        start: HashedVersion::of(start.max(0) as u64, hash),
                        holder,
                        holder_domain,
                        statement,
                    });
                }
                Ok(epochs)
            })
            .await
    }

    /// The orphans still to be resubmitted, by wavelet, fork and order.
    pub async fn pending_orphans(&self) -> Result<Vec<OrphanDelta>, StorageError> {
        self.database()
            .read(|conn| {
                let mut statement = conn.prepare_cached(
                    "SELECT wavelet, sha256, fork_version, fork_hash, seq, applied \
                     FROM orphan_deltas WHERE status = 'pending' \
                     ORDER BY wavelet, fork_version, seq",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Vec<u8>>(5)?,
                    ))
                })?;
                let mut orphans = Vec::new();
                for row in rows {
                    let (wavelet, sha256, fork, fork_hash, seq, applied) = row?;
                    orphans.push(OrphanDelta {
                        wavelet,
                        sha256,
                        fork: HashedVersion::of(fork.max(0) as u64, fork_hash),
                        seq: seq.max(0) as u64,
                        applied,
                    });
                }
                Ok(orphans)
            })
            .await
    }

    /// Marks orphans as delivered to the host.
    pub async fn orphans_done(
        &self,
        wavelet: &str,
        sha256: Vec<Vec<u8>>,
    ) -> Result<(), StorageError> {
        let wavelet = wavelet.to_owned();
        self.database()
            .write(move |conn| {
                let mut statement = conn.prepare_cached(
                    "UPDATE orphan_deltas SET status = 'done' WHERE wavelet = ?1 AND sha256 = ?2",
                )?;
                for sha in &sha256 {
                    statement.execute(params![wavelet, sha])?;
                }
                Ok(())
            })
            .await
    }
}
