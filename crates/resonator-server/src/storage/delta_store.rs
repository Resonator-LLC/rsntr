//! The delta store: the applied and transformed delta history of every
//! wavelet, plus periodic wavelet snapshots.
//!
//! Derived from `DeltaStore`, `WaveletStore`, `WaveletDeltaRecordReader`
//! and `WaveletDeltaRecord` in `org.waveprotocol.box.server.waveserver` and
//! the file and memory stores in `org.waveprotocol.box.server.persistence`
//! of Apache Wave, licensed under the Apache License, Version 2.0. The
//! snapshot rows are an addition.

use std::collections::BTreeSet;

use resonator_model::id::{ParticipantId, WaveId, WaveletId, WaveletName};
use resonator_model::operation::TransformedWaveletDelta;
use resonator_model::proto::delta_store::ProtoTransformedWaveletDelta;
use resonator_model::proto::federation::ProtocolAppliedWaveletDelta;
use resonator_model::proto::{
    ByteStringMessage, decode, deserialize_stored_delta, encode, get_hashed_version_applied_at,
    serialize_stored_delta,
};
use resonator_model::version::HashedVersion;
use rusqlite::{Connection, OptionalExtension, params};

use super::federation_store::{AppliedRequest, insert_applied};
use super::{Database, StorageError, WriteHandle};

/// An applied delta with the exact bytes its history hash was computed
/// over.
pub type AppliedDelta = ByteStringMessage<ProtocolAppliedWaveletDelta>;

/// A delta as applied to a wavelet: the applied delta (the signed original
/// with the version it was applied at) and its transformed form
/// (`WaveletDeltaRecord`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaveletDeltaRecord {
    pub applied_at_version: HashedVersion,
    /// Absent only for the empty record returned when a submitted delta
    /// transformed away; stored records always have it.
    pub applied: Option<AppliedDelta>,
    pub transformed: TransformedWaveletDelta,
}

impl WaveletDeltaRecord {
    pub fn new(
        applied_at_version: HashedVersion,
        applied: Option<AppliedDelta>,
        transformed: TransformedWaveletDelta,
    ) -> Self {
        WaveletDeltaRecord {
            applied_at_version,
            applied,
            transformed,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.transformed.is_empty()
    }

    pub fn author(&self) -> &ParticipantId {
        self.transformed.author()
    }

    pub fn resulting_version(&self) -> &HashedVersion {
        self.transformed.resulting_version()
    }

    pub fn application_timestamp(&self) -> i64 {
        self.transformed.application_timestamp()
    }
}

/// A stored wavelet snapshot: a serialised `WaveletSnapshot` message and
/// the hashed version it was taken at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSnapshot {
    pub version: HashedVersion,
    pub snapshot: Vec<u8>,
}

fn corrupt(name: &WaveletName, message: impl std::fmt::Display) -> StorageError {
    StorageError::Corrupt {
        wavelet: name.to_string(),
        message: message.to_string(),
    }
}

fn to_sql_version(version: u64) -> Result<i64, StorageError> {
    i64::try_from(version)
        .map_err(|_| StorageError::Invalid(format!("version {version} exceeds the range of int64")))
}

fn from_sql_version(name: &WaveletName, version: i64) -> Result<u64, StorageError> {
    u64::try_from(version).map_err(|_| corrupt(name, format!("negative version {version}")))
}

/// The row id of a wavelet, if it has any deltas.
pub(crate) fn wavelet_row_id(
    conn: &Connection,
    name: &WaveletName,
) -> Result<Option<i64>, StorageError> {
    Ok(conn
        .prepare_cached(
            "SELECT id FROM wavelets WHERE wave_domain = ?1 AND wave_id = ?2 \
             AND wavelet_domain = ?3 AND wavelet_id = ?4",
        )?
        .query_row(
            params![
                name.wave_id.domain(),
                name.wave_id.id(),
                name.wavelet_id.domain(),
                name.wavelet_id.id()
            ],
            |row| row.get(0),
        )
        .optional()?)
}

struct DeltaRow {
    applied_at_version: i64,
    resulting_version: i64,
    resulting_hash: Vec<u8>,
    applied: Vec<u8>,
    transformed: Vec<u8>,
}

const DELTA_COLUMNS: &str =
    "applied_at_version, resulting_version, resulting_hash, applied, transformed";

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeltaRow> {
    Ok(DeltaRow {
        applied_at_version: row.get(0)?,
        resulting_version: row.get(1)?,
        resulting_hash: row.get(2)?,
        applied: row.get(3)?,
        transformed: row.get(4)?,
    })
}

fn decode_row(name: &WaveletName, row: DeltaRow) -> Result<WaveletDeltaRecord, StorageError> {
    let at = row.applied_at_version;
    let applied: AppliedDelta = ByteStringMessage::parse_from(row.applied)
        .map_err(|e| corrupt(name, format!("applied delta at version {at}: {e}")))?;
    let stored: ProtoTransformedWaveletDelta = decode(&row.transformed)
        .map_err(|e| corrupt(name, format!("transformed delta at version {at}: {e}")))?;
    let transformed = deserialize_stored_delta(&stored)
        .map_err(|e| corrupt(name, format!("transformed delta at version {at}: {e}")))?;
    let applied_at_version = get_hashed_version_applied_at(&applied)
        .map_err(|e| corrupt(name, format!("applied delta at version {at}: {e}")))?;
    if to_sql_version(applied_at_version.version()).ok() != Some(at)
        || to_sql_version(transformed.applied_at_version()).ok() != Some(at)
    {
        return Err(corrupt(
            name,
            format!("delta stored at version {at} claims another applied-at version"),
        ));
    }
    let resulting = transformed.resulting_version();
    if to_sql_version(resulting.version()).ok() != Some(row.resulting_version)
        || resulting.history_hash() != row.resulting_hash.as_slice()
    {
        return Err(corrupt(
            name,
            format!("delta stored at version {at} disagrees with its resulting version columns"),
        ));
    }
    Ok(WaveletDeltaRecord {
        applied_at_version,
        applied: Some(applied),
        transformed,
    })
}

/// A delta encoded for insertion.
struct EncodedDelta {
    applied_at_version: i64,
    resulting_version: i64,
    resulting_hash: Vec<u8>,
    applied: Vec<u8>,
    transformed: Vec<u8>,
    applied_timestamp: i64,
    author: String,
}

fn encode_record(record: &WaveletDeltaRecord) -> Result<EncodedDelta, StorageError> {
    let applied = record.applied.as_ref().ok_or_else(|| {
        StorageError::Invalid("cannot store a delta record without an applied delta".to_owned())
    })?;
    if record.transformed.is_empty() {
        return Err(StorageError::Invalid(
            "cannot store an empty delta".to_owned(),
        ));
    }
    let stored = serialize_stored_delta(&record.transformed)
        .and_then(|message| encode(&message))
        .map_err(|e| StorageError::Invalid(format!("cannot encode transformed delta: {e}")))?;
    if record.transformed.applied_at_version() != record.applied_at_version.version() {
        return Err(StorageError::Invalid(format!(
            "delta record applied at {} holds a transformed delta applied at {}",
            record.applied_at_version.version(),
            record.transformed.applied_at_version()
        )));
    }
    Ok(EncodedDelta {
        applied_at_version: to_sql_version(record.applied_at_version.version())?,
        resulting_version: to_sql_version(record.resulting_version().version())?,
        resulting_hash: record.resulting_version().history_hash().to_vec(),
        applied: applied.bytes().to_vec(),
        transformed: stored,
        applied_timestamp: record.application_timestamp(),
        author: record.author().address().to_owned(),
    })
}

/// The resulting version of the last stored delta.
fn end_version(
    conn: &Connection,
    name: &WaveletName,
    id: i64,
) -> Result<Option<HashedVersion>, StorageError> {
    let row: Option<(i64, Vec<u8>)> = conn
        .prepare_cached(
            "SELECT resulting_version, resulting_hash FROM deltas WHERE wavelet = ?1 \
             ORDER BY applied_at_version DESC LIMIT 1",
        )?
        .query_row([id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    row.map(|(version, hash)| Ok(HashedVersion::of(from_sql_version(name, version)?, hash)))
        .transpose()
}

/// The hashed version at `version` if a stored delta ends there.
fn hashed_version_ending_at(
    conn: &Connection,
    name: &WaveletName,
    id: i64,
    version: u64,
) -> Result<Option<HashedVersion>, StorageError> {
    let row: Option<(i64, Vec<u8>)> = conn
        .prepare_cached(
            "SELECT resulting_version, resulting_hash FROM deltas \
             WHERE wavelet = ?1 AND applied_at_version < ?2 \
             ORDER BY applied_at_version DESC LIMIT 1",
        )?
        .query_row(params![id, to_sql_version(version)?], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    match row {
        Some((end, hash)) if from_sql_version(name, end)? == version => {
            Ok(Some(HashedVersion::of(version, hash)))
        }
        _ => Ok(None),
    }
}

fn insert_deltas(
    conn: &Connection,
    name: &WaveletName,
    hosted_by: Option<&str>,
    deltas: &[EncodedDelta],
    snapshot: Option<&StoredSnapshot>,
    keep_snapshots: usize,
) -> Result<(), StorageError> {
    let id = match wavelet_row_id(conn, name)? {
        Some(id) => id,
        None => {
            conn.prepare_cached(
                "INSERT INTO wavelets (wave_domain, wave_id, wavelet_domain, wavelet_id, \
                 hosted_by) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![
                name.wave_id.domain(),
                name.wave_id.id(),
                name.wavelet_id.domain(),
                name.wavelet_id.id(),
                hosted_by
            ])?;
            conn.last_insert_rowid()
        }
    };
    let mut end = end_version(conn, name, id)?
        .map(|version| to_sql_version(version.version()))
        .transpose()?
        .unwrap_or(0);
    let mut insert = conn.prepare_cached(
        "INSERT INTO deltas (wavelet, applied_at_version, resulting_version, resulting_hash, \
         applied, transformed, applied_timestamp, author) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for delta in deltas {
        if delta.applied_at_version != end {
            return Err(StorageError::Invalid(format!(
                "delta applied at version {} does not continue the stored history of {name} \
                 ending at version {end}",
                delta.applied_at_version
            )));
        }
        insert.execute(params![
            id,
            delta.applied_at_version,
            delta.resulting_version,
            delta.resulting_hash,
            delta.applied,
            delta.transformed,
            delta.applied_timestamp,
            delta.author
        ])?;
        end = delta.resulting_version;
    }
    if let Some(snapshot) = snapshot {
        insert_snapshot(conn, name, id, snapshot, keep_snapshots)?;
    }
    Ok(())
}

fn insert_snapshot(
    conn: &Connection,
    name: &WaveletName,
    id: i64,
    snapshot: &StoredSnapshot,
    keep_snapshots: usize,
) -> Result<(), StorageError> {
    let version = snapshot.version.version();
    if hashed_version_ending_at(conn, name, id, version)?.as_ref() != Some(&snapshot.version) {
        return Err(StorageError::Invalid(format!(
            "snapshot of {name} at version {version} is not at a stored delta boundary"
        )));
    }
    conn.prepare_cached(
        "INSERT OR REPLACE INTO snapshots (wavelet, version, hash, snapshot) \
         VALUES (?1, ?2, ?3, ?4)",
    )?
    .execute(params![
        id,
        to_sql_version(version)?,
        snapshot.version.history_hash(),
        snapshot.snapshot
    ])?;
    conn.prepare_cached(
        "DELETE FROM snapshots WHERE wavelet = ?1 AND version NOT IN \
         (SELECT version FROM snapshots WHERE wavelet = ?1 ORDER BY version DESC LIMIT ?2)",
    )?
    .execute(params![id, keep_snapshots.max(1) as i64])?;
    Ok(())
}

fn parse_wave_id(domain: &str, id: &str) -> Option<WaveId> {
    WaveId::of(domain, id)
        .or_else(|_| WaveId::of_legacy(domain, id))
        .ok()
}

fn parse_wavelet_id(domain: &str, id: &str) -> Option<WaveletId> {
    WaveletId::of(domain, id)
        .or_else(|_| WaveletId::of_legacy(domain, id))
        .ok()
}

/// The delta histories of all wavelets (`DeltaStore`).
#[derive(Debug, Clone)]
pub struct DeltaStore {
    db: Database,
}

impl DeltaStore {
    pub(crate) fn new(db: Database) -> Self {
        DeltaStore { db }
    }

    pub(crate) fn database(&self) -> &Database {
        &self.db
    }

    /// Access to one wavelet's history. Does no i/o; a wavelet without
    /// deltas is simply empty and comes into being with its first delta.
    pub fn open(&self, wavelet_name: WaveletName) -> DeltasAccess {
        DeltasAccess {
            db: self.db.clone(),
            name: wavelet_name,
            hosted_by: None,
        }
    }

    /// [`DeltaStore::open`] for a replica of a wavelet hosted by the remote
    /// domain `host`: the wavelet row its first delta creates is marked
    /// `hosted_by` that domain.
    pub fn open_replica(&self, wavelet_name: WaveletName, host: &str) -> DeltasAccess {
        DeltasAccess {
            db: self.db.clone(),
            name: wavelet_name,
            hosted_by: Some(host.to_owned()),
        }
    }

    /// The remote domain hosting a stored wavelet, `None` for a local
    /// wavelet or one without deltas.
    pub async fn hosted_by(
        &self,
        wavelet_name: &WaveletName,
    ) -> Result<Option<String>, StorageError> {
        let name = wavelet_name.clone();
        self.db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(
                        "SELECT hosted_by FROM wavelets WHERE wave_domain = ?1 AND wave_id = ?2 \
                         AND wavelet_domain = ?3 AND wavelet_id = ?4",
                    )?
                    .query_row(
                        params![
                            name.wave_id.domain(),
                            name.wave_id.id(),
                            name.wavelet_id.domain(),
                            name.wavelet_id.id()
                        ],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .optional()?
                    .flatten())
            })
            .await
    }

    /// Deletes a wavelet with its deltas and snapshots. Fails with
    /// [`StorageError::WaveletNotFound`] if it has no deltas.
    pub async fn delete(&self, wavelet_name: &WaveletName) -> Result<(), StorageError> {
        let name = wavelet_name.clone();
        self.db
            .write(move |conn| {
                let id = wavelet_row_id(conn, &name)?
                    .ok_or_else(|| StorageError::WaveletNotFound(name.to_string()))?;
                // Deltas and snapshots go with it (ON DELETE CASCADE).
                conn.prepare_cached("DELETE FROM wavelets WHERE id = ?1")?
                    .execute([id])?;
                Ok(())
            })
            .await
    }

    /// The ids of the non-empty wavelets of a wave.
    pub async fn lookup(&self, wave_id: &WaveId) -> Result<BTreeSet<WaveletId>, StorageError> {
        let wave_id = wave_id.clone();
        self.db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(
                    "SELECT wavelet_domain, wavelet_id FROM wavelets \
                     WHERE wave_domain = ?1 AND wave_id = ?2",
                )?;
                let rows = statement.query_map([wave_id.domain(), wave_id.id()], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                let mut ids = BTreeSet::new();
                for row in rows {
                    let (domain, id) = row?;
                    ids.insert(parse_wavelet_id(&domain, &id).ok_or_else(|| {
                        StorageError::Corrupt {
                            wavelet: format!("{domain}/{id} of {wave_id}"),
                            message: "invalid stored wavelet id".to_owned(),
                        }
                    })?);
                }
                Ok(ids)
            })
            .await
    }

    /// The ids of all waves with at least one non-empty wavelet.
    pub async fn wave_ids(&self) -> Result<Vec<WaveId>, StorageError> {
        self.db
            .read(|conn| {
                let mut statement = conn.prepare_cached(
                    "SELECT DISTINCT wave_domain, wave_id FROM wavelets \
                     ORDER BY wave_domain, wave_id",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                let mut ids = Vec::new();
                for row in rows {
                    let (domain, id) = row?;
                    ids.push(
                        parse_wave_id(&domain, &id).ok_or_else(|| StorageError::Corrupt {
                            wavelet: format!("{domain}/{id}"),
                            message: "invalid stored wave id".to_owned(),
                        })?,
                    );
                }
                Ok(ids)
            })
            .await
    }

    /// Number of stored deltas across all wavelets.
    pub async fn delta_count(&self) -> Result<u64, StorageError> {
        self.db
            .read(|conn| {
                let count: i64 = conn.query_row("SELECT COUNT(*) FROM deltas", [], |r| r.get(0))?;
                Ok(count.max(0) as u64)
            })
            .await
    }
}

/// The delta history of one wavelet (`DeltaStore.DeltasAccess`). Versions
/// are looked up by number; callers compare hashes.
#[derive(Debug, Clone)]
pub struct DeltasAccess {
    db: Database,
    name: WaveletName,
    /// The remote host of a replica; `None` for a local wavelet.
    hosted_by: Option<String>,
}

impl DeltasAccess {
    pub fn wavelet_name(&self) -> &WaveletName {
        &self.name
    }

    async fn read_wavelet<T, F>(&self, work: F) -> Result<Option<T>, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection, &WaveletName, i64) -> Result<Option<T>, StorageError>
            + Send
            + 'static,
    {
        let name = self.name.clone();
        self.db
            .read(move |conn| match wavelet_row_id(conn, &name)? {
                Some(id) => work(conn, &name, id),
                None => Ok(None),
            })
            .await
    }

    /// Whether the wavelet has no deltas.
    pub async fn is_empty(&self) -> Result<bool, StorageError> {
        Ok(self.end_version().await?.is_none())
    }

    /// The resulting version of the last delta, if any.
    pub async fn end_version(&self) -> Result<Option<HashedVersion>, StorageError> {
        self.read_wavelet(end_version).await
    }

    /// The delta applied at `version`, if any.
    pub async fn delta(&self, version: u64) -> Result<Option<WaveletDeltaRecord>, StorageError> {
        self.read_wavelet(move |conn, name, id| {
            conn.prepare_cached(&format!(
                "SELECT {DELTA_COLUMNS} FROM deltas \
                 WHERE wavelet = ?1 AND applied_at_version = ?2"
            ))?
            .query_row(params![id, to_sql_version(version)?], read_row)
            .optional()?
            .map(|row| decode_row(name, row))
            .transpose()
        })
        .await
    }

    /// The delta leading to `version`, if any.
    pub async fn delta_by_end_version(
        &self,
        version: u64,
    ) -> Result<Option<WaveletDeltaRecord>, StorageError> {
        self.read_wavelet(move |conn, name, id| {
            let row = conn
                .prepare_cached(&format!(
                    "SELECT {DELTA_COLUMNS} FROM deltas \
                     WHERE wavelet = ?1 AND applied_at_version < ?2 \
                     ORDER BY applied_at_version DESC LIMIT 1"
                ))?
                .query_row(params![id, to_sql_version(version)?], read_row)
                .optional()?;
            match row {
                Some(row) if from_sql_version(name, row.resulting_version)? == version => {
                    decode_row(name, row).map(Some)
                }
                _ => Ok(None),
            }
        })
        .await
    }

    /// The last delta, if any.
    pub async fn last_delta(&self) -> Result<Option<WaveletDeltaRecord>, StorageError> {
        self.read_wavelet(|conn, name, id| {
            conn.prepare_cached(&format!(
                "SELECT {DELTA_COLUMNS} FROM deltas WHERE wavelet = ?1 \
                 ORDER BY applied_at_version DESC LIMIT 1"
            ))?
            .query_row([id], read_row)
            .optional()?
            .map(|row| decode_row(name, row))
            .transpose()
        })
        .await
    }

    /// The hashed version a delta was applied at, if a delta was applied
    /// at `version`.
    pub async fn applied_at_version(
        &self,
        version: u64,
    ) -> Result<Option<HashedVersion>, StorageError> {
        Ok(self
            .delta(version)
            .await?
            .map(|record| record.applied_at_version))
    }

    /// The resulting hashed version of the delta applied at `version`, if
    /// any.
    pub async fn resulting_version(
        &self,
        version: u64,
    ) -> Result<Option<HashedVersion>, StorageError> {
        self.read_wavelet(move |conn, name, id| {
            let row: Option<(i64, Vec<u8>)> = conn
                .prepare_cached(
                    "SELECT resulting_version, resulting_hash FROM deltas \
                     WHERE wavelet = ?1 AND applied_at_version = ?2",
                )?
                .query_row(params![id, to_sql_version(version)?], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional()?;
            row.map(|(version, hash)| Ok(HashedVersion::of(from_sql_version(name, version)?, hash)))
                .transpose()
        })
        .await
    }

    /// The hashed version at `version` if a stored delta ends exactly
    /// there. Reads no delta bodies.
    pub async fn hashed_version_ending_at(
        &self,
        version: u64,
    ) -> Result<Option<HashedVersion>, StorageError> {
        self.read_wavelet(move |conn, name, id| hashed_version_ending_at(conn, name, id, version))
            .await
    }

    /// The applied delta applied at `version`, if any.
    pub async fn applied_delta(&self, version: u64) -> Result<Option<AppliedDelta>, StorageError> {
        Ok(self.delta(version).await?.and_then(|record| record.applied))
    }

    /// The transformed delta applied at `version`, if any.
    pub async fn transformed_delta(
        &self,
        version: u64,
    ) -> Result<Option<TransformedWaveletDelta>, StorageError> {
        Ok(self.delta(version).await?.map(|record| record.transformed))
    }

    /// Deltas in history order, starting with the one applied at
    /// `start_version` (which must be a delta boundary for anything to be
    /// returned) and stopping before `end_version` or after `limit` deltas.
    pub async fn deltas_in_range(
        &self,
        start_version: u64,
        end_version: u64,
        limit: usize,
    ) -> Result<Vec<WaveletDeltaRecord>, StorageError> {
        let records = self
            .read_wavelet(move |conn, name, id| {
                let mut statement = conn.prepare_cached(&format!(
                    "SELECT {DELTA_COLUMNS} FROM deltas \
                     WHERE wavelet = ?1 AND applied_at_version >= ?2 AND applied_at_version < ?3 \
                     ORDER BY applied_at_version LIMIT ?4"
                ))?;
                let rows = statement.query_map(
                    params![
                        id,
                        to_sql_version(start_version)?,
                        to_sql_version(end_version.min(i64::MAX as u64))?,
                        i64::try_from(limit).unwrap_or(i64::MAX)
                    ],
                    read_row,
                )?;
                let mut records = Vec::new();
                let mut expected = start_version;
                for row in rows {
                    let record = decode_row(name, row?)?;
                    if record.applied_at_version.version() != expected {
                        if records.is_empty() {
                            // Not a delta boundary.
                            return Ok(Some(records));
                        }
                        return Err(corrupt(
                            name,
                            format!(
                                "gap in delta history: expected a delta at version {expected}, \
                                 found one at {}",
                                record.applied_at_version.version()
                            ),
                        ));
                    }
                    expected = record.resulting_version().version();
                    records.push(record);
                }
                Ok(Some(records))
            })
            .await?;
        Ok(records.unwrap_or_default())
    }

    /// Queues deltas, and optionally a snapshot at the resulting version of
    /// the last of them, for appending. The deltas must continue the stored
    /// history; hashes are the caller's responsibility. `applied`, the
    /// federation request that submitted the deltas, is recorded in the
    /// same transaction.
    pub(crate) async fn enqueue_append(
        &self,
        deltas: &[&WaveletDeltaRecord],
        snapshot: Option<StoredSnapshot>,
        keep_snapshots: usize,
        applied: Option<AppliedRequest>,
    ) -> Result<WriteHandle<()>, StorageError> {
        let encoded = deltas
            .iter()
            .map(|record| encode_record(record))
            .collect::<Result<Vec<_>, _>>()?;
        let name = self.name.clone();
        let hosted_by = self.hosted_by.clone();
        self.db
            .enqueue_write(move |conn| {
                insert_deltas(
                    conn,
                    &name,
                    hosted_by.as_deref(),
                    &encoded,
                    snapshot.as_ref(),
                    keep_snapshots,
                )?;
                match &applied {
                    Some(row) => insert_applied(conn, row),
                    None => Ok(()),
                }
            })
            .await
    }

    /// Appends contiguous deltas to the end of the history. They are
    /// committed when this returns.
    pub async fn append(&self, deltas: &[WaveletDeltaRecord]) -> Result<(), StorageError> {
        let deltas: Vec<&WaveletDeltaRecord> = deltas.iter().collect();
        self.enqueue_append(&deltas, None, 1, None)
            .await?
            .wait()
            .await
    }

    /// Stores a snapshot taken at a delta boundary and drops all but the
    /// `keep` newest snapshots of the wavelet.
    pub async fn put_snapshot(
        &self,
        snapshot: StoredSnapshot,
        keep: usize,
    ) -> Result<(), StorageError> {
        let name = self.name.clone();
        self.db
            .write(move |conn| {
                let id = wavelet_row_id(conn, &name)?
                    .ok_or_else(|| StorageError::WaveletNotFound(name.to_string()))?;
                insert_snapshot(conn, &name, id, &snapshot, keep)
            })
            .await
    }

    /// The stored snapshots, newest first.
    pub async fn snapshots(&self) -> Result<Vec<StoredSnapshot>, StorageError> {
        let snapshots = self
            .read_wavelet(|conn, name, id| {
                let mut statement = conn.prepare_cached(
                    "SELECT version, hash, snapshot FROM snapshots WHERE wavelet = ?1 \
                     ORDER BY version DESC",
                )?;
                let rows = statement.query_map([id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                })?;
                let mut snapshots = Vec::new();
                for row in rows {
                    let (version, hash, snapshot) = row?;
                    snapshots.push(StoredSnapshot {
                        version: HashedVersion::of(from_sql_version(name, version)?, hash),
                        snapshot,
                    });
                }
                Ok(Some(snapshots))
            })
            .await?;
        Ok(snapshots.unwrap_or_default())
    }
}
