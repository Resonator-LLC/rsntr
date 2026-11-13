//! The in-memory state of a loaded wavelet backed by the delta store.
//!
//! Derived from `WaveletState` and `DeltaStoreBasedWaveletState` in
//! `org.waveprotocol.box.server.waveserver` of Apache Wave, licensed under
//! the Apache License, Version 2.0.
//!
//! Differences from the original: a wavelet is loaded from the newest
//! usable stored snapshot plus the deltas after it instead of always
//! replaying its whole history; the hash chain of every delta read while
//! loading is verified; a window of recent deltas is kept in memory (the
//! original keeps only the ones not yet persisted); and applying a delta is
//! atomic.

use std::collections::VecDeque;
use std::sync::Arc;

use resonator_model::data::WaveletData;
use resonator_model::id::WaveletName;
use resonator_model::operation::{
    TransformedWaveletDelta, WaveletOperationError, WaveletOperationKind, apply_wavelet_delta,
    build_wavelet_from_first_delta,
};
use resonator_model::proto::waveclient_rpc::WaveletSnapshot;
use resonator_model::proto::{
    calculate_resulting_hashed_version, decode, deserialize_wavelet, encode, serialize_wavelet,
};
use resonator_model::version::{HashedVersion, HashedVersionFactory};

use crate::storage::{DeltasAccess, StorageError, StoredSnapshot, WaveletDeltaRecord};

/// Deltas read per query while replaying history.
const REPLAY_CHUNK: usize = 512;

/// Snapshots kept per wavelet; the older one is the fallback if the newest
/// turns out unusable.
pub(crate) const KEEP_SNAPSHOTS: usize = 2;

#[derive(Debug, Clone, Copy)]
pub(crate) struct StateSettings {
    /// A snapshot is stored every this many deltas; 0 for never.
    pub snapshot_interval: u64,
    /// Persisted deltas kept in memory.
    pub delta_cache_size: usize,
}

/// Why a delta could not be appended to the in-memory state.
#[derive(Debug)]
pub(crate) enum AppendError {
    /// The delta does not apply; the state is unchanged.
    Rejected(WaveletOperationError),
    /// The delta failed after validating; the state can no longer be
    /// trusted.
    Corrupted(String),
}

fn corrupt(name: &WaveletName, message: impl std::fmt::Display) -> StorageError {
    StorageError::Corrupt {
        wavelet: name.to_string(),
        message: message.to_string(),
    }
}

pub(crate) struct WaveletState {
    access: DeltasAccess,
    version_zero: HashedVersion,
    /// Absent while the wavelet has no deltas.
    snapshot: Option<WaveletData>,
    /// The newest part of the history, contiguous and ending at the current
    /// version. Holds at least every delta not yet persisted.
    recent: VecDeque<Arc<WaveletDeltaRecord>>,
    /// Deltas applied since the last stored snapshot.
    deltas_since_snapshot: u64,
    settings: StateSettings,
}

/// How a wavelet was loaded; for logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadReport {
    /// The version of the stored snapshot loading started from, if any.
    pub snapshot_version: Option<u64>,
    /// Stored snapshots that had to be skipped as unusable.
    pub snapshots_rejected: usize,
    /// Deltas replayed on top of the snapshot (or from version zero).
    pub deltas_replayed: u64,
}

struct Replay {
    wavelet: Option<WaveletData>,
    tail: VecDeque<Arc<WaveletDeltaRecord>>,
    count: u64,
}

/// Applies the stored deltas from `at` to the end of the history, checking
/// that each continues the hash chain.
async fn replay(
    access: &DeltasAccess,
    mut wavelet: Option<WaveletData>,
    mut at: HashedVersion,
    end: &HashedVersion,
    tail_size: usize,
) -> Result<Replay, StorageError> {
    let name = access.wavelet_name();
    let mut tail = VecDeque::new();
    let mut count = 0u64;
    while at.version() < end.version() {
        let chunk = access
            .deltas_in_range(at.version(), u64::MAX, REPLAY_CHUNK)
            .await?;
        if chunk.is_empty() {
            return Err(corrupt(
                name,
                format!("no delta applies at version {}", at.version()),
            ));
        }
        for record in chunk {
            if record.applied_at_version != at {
                return Err(corrupt(
                    name,
                    format!(
                        "delta at version {} does not continue the history hash",
                        at.version()
                    ),
                ));
            }
            let hashed = record
                .applied
                .as_ref()
                .map(calculate_resulting_hashed_version)
                .transpose()
                .map_err(|e| corrupt(name, e))?;
            if hashed.as_ref() != Some(record.resulting_version()) {
                return Err(corrupt(
                    name,
                    format!(
                        "delta at version {} does not hash to its resulting version",
                        at.version()
                    ),
                ));
            }
            match &mut wavelet {
                None => {
                    wavelet = Some(
                        build_wavelet_from_first_delta(name, &record.transformed)
                            .map_err(|e| corrupt(name, format!("first delta: {e}")))?,
                    );
                }
                Some(wavelet) => {
                    apply_wavelet_delta(&record.transformed, wavelet).map_err(|e| {
                        corrupt(name, format!("delta at version {}: {e}", at.version()))
                    })?;
                }
            }
            at = record.resulting_version().clone();
            count += 1;
            if tail_size > 0 {
                if tail.len() == tail_size {
                    tail.pop_front();
                }
                tail.push_back(Arc::new(record));
            }
        }
    }
    if &at != end || wavelet.as_ref().map(WaveletData::hashed_version) != Some(end) {
        return Err(corrupt(
            name,
            format!(
                "replayed history ends at version {} instead of {}",
                at.version(),
                end.version()
            ),
        ));
    }
    Ok(Replay {
        wavelet,
        tail,
        count,
    })
}

/// Decodes a stored snapshot and checks it against the delta history.
async fn open_snapshot(
    access: &DeltasAccess,
    stored: &StoredSnapshot,
    end: &HashedVersion,
) -> Result<WaveletData, StorageError> {
    let name = access.wavelet_name();
    let version = stored.version.version();
    if version > end.version() {
        return Err(corrupt(name, "snapshot is ahead of the delta history"));
    }
    let boundary = access.hashed_version_ending_at(version).await?;
    if boundary.as_ref() != Some(&stored.version) {
        return Err(corrupt(name, "snapshot is not at a delta boundary"));
    }
    let message: WaveletSnapshot = decode(&stored.snapshot).map_err(|e| corrupt(name, e))?;
    let wavelet = deserialize_wavelet(&message, &name.wave_id).map_err(|e| corrupt(name, e))?;
    if wavelet.wavelet_id() != &name.wavelet_id || wavelet.hashed_version() != &stored.version {
        return Err(corrupt(name, "snapshot contents do not match its row"));
    }
    Ok(wavelet)
}

impl WaveletState {
    /// Loads a wavelet: from the newest usable snapshot plus the deltas
    /// after it, or by replaying the whole history if there is none.
    pub(crate) async fn load(
        access: DeltasAccess,
        settings: StateSettings,
    ) -> Result<(Self, LoadReport), StorageError> {
        let name = access.wavelet_name().clone();
        let version_zero = HashedVersionFactory::create_version_zero(&name);
        let mut report = LoadReport {
            snapshot_version: None,
            snapshots_rejected: 0,
            deltas_replayed: 0,
        };
        let Some(end) = access.end_version().await? else {
            let state = WaveletState {
                access,
                version_zero,
                snapshot: None,
                recent: VecDeque::new(),
                deltas_since_snapshot: 0,
                settings,
            };
            return Ok((state, report));
        };
        let mut replayed = None;
        for stored in access.snapshots().await? {
            let attempt = async {
                let wavelet = open_snapshot(&access, &stored, &end).await?;
                let start = stored.version.clone();
                replay(
                    &access,
                    Some(wavelet),
                    start,
                    &end,
                    settings.delta_cache_size,
                )
                .await
            };
            match attempt.await {
                Ok(replay) => {
                    report.snapshot_version = Some(stored.version.version());
                    replayed = Some(replay);
                    break;
                }
                Err(error) => {
                    report.snapshots_rejected += 1;
                    tracing::warn!(
                        wavelet = %name,
                        snapshot_version = stored.version.version(),
                        %error,
                        "ignoring unusable wavelet snapshot"
                    );
                }
            }
        }
        let replayed = match replayed {
            Some(replayed) => replayed,
            None => {
                let start = version_zero.clone();
                replay(&access, None, start, &end, settings.delta_cache_size).await?
            }
        };
        report.deltas_replayed = replayed.count;
        let state = WaveletState {
            access,
            version_zero,
            snapshot: replayed.wavelet,
            recent: replayed.tail,
            deltas_since_snapshot: replayed.count,
            settings,
        };
        Ok((state, report))
    }

    pub(crate) fn wavelet_name(&self) -> &WaveletName {
        self.access.wavelet_name()
    }

    pub(crate) fn access(&self) -> &DeltasAccess {
        &self.access
    }

    /// The wavelet's data; `None` while it has no deltas.
    pub(crate) fn snapshot(&self) -> Option<&WaveletData> {
        self.snapshot.as_ref()
    }

    pub(crate) fn current_version(&self) -> &HashedVersion {
        match &self.snapshot {
            Some(snapshot) => snapshot.hashed_version(),
            None => &self.version_zero,
        }
    }

    pub(crate) fn version_zero(&self) -> &HashedVersion {
        &self.version_zero
    }

    /// The cached delta applied at `version`.
    fn find_recent(&self, version: u64) -> Option<&Arc<WaveletDeltaRecord>> {
        self.recent
            .binary_search_by_key(&version, |record| record.applied_at_version.version())
            .ok()
            .and_then(|index| self.recent.get(index))
    }

    /// The first version covered by the in-memory window, if it is not
    /// empty.
    fn recent_start(&self) -> Option<u64> {
        self.recent
            .front()
            .map(|record| record.applied_at_version.version())
    }

    /// The hashed version at `version` if that is a delta boundary of the
    /// history.
    pub(crate) async fn hashed_version(
        &self,
        version: u64,
    ) -> Result<Option<HashedVersion>, StorageError> {
        if version == 0 {
            return Ok(Some(self.version_zero.clone()));
        }
        let Some(snapshot) = &self.snapshot else {
            return Ok(None);
        };
        if version == snapshot.version() {
            return Ok(Some(snapshot.hashed_version().clone()));
        }
        if version > snapshot.version() {
            return Ok(None);
        }
        if self.recent_start().is_some_and(|start| version >= start) {
            return Ok(self
                .find_recent(version)
                .map(|record| record.applied_at_version.clone()));
        }
        self.access.hashed_version_ending_at(version).await
    }

    /// The delta applied at `begin_version`; `None` if no delta was applied
    /// there or the hash differs.
    pub(crate) async fn delta_applied_at(
        &self,
        begin_version: &HashedVersion,
    ) -> Result<Option<Arc<WaveletDeltaRecord>>, StorageError> {
        let version = begin_version.version();
        let record = if self.recent_start().is_some_and(|start| version >= start) {
            self.find_recent(version).cloned()
        } else {
            self.access.delta(version).await?.map(Arc::new)
        };
        Ok(record.filter(|record| &record.applied_at_version == begin_version))
    }

    /// The delta leading to `end_version`; `None` if no delta ends there or
    /// the hash differs.
    pub(crate) async fn delta_by_end_version(
        &self,
        end_version: &HashedVersion,
    ) -> Result<Option<Arc<WaveletDeltaRecord>>, StorageError> {
        let version = end_version.version();
        let cached = self.recent.iter().rev().find_map(|record| {
            let end = record.resulting_version().version();
            (end <= version).then(|| (end == version).then(|| Arc::clone(record)))
        });
        let record = match cached {
            Some(found) => found,
            None if self.recent_start().is_some_and(|start| version > start) => None,
            None => self
                .access
                .delta_by_end_version(version)
                .await?
                .map(Arc::new),
        };
        Ok(record.filter(|record| record.resulting_version() == end_version))
    }

    /// The deltas from `start` to `end`, both of which the caller has
    /// checked to be delta boundaries with `start` before `end`; at most
    /// `limit` of them.
    pub(crate) async fn history(
        &self,
        start: u64,
        end: u64,
        limit: usize,
    ) -> Result<Vec<Arc<WaveletDeltaRecord>>, StorageError> {
        let mut deltas = Vec::new();
        if start >= end || limit == 0 {
            return Ok(deltas);
        }
        let memory_start = self.recent_start().unwrap_or(u64::MAX);
        let mut next = start;
        if start < memory_start {
            let stored = self
                .access
                .deltas_in_range(start, end.min(memory_start), limit)
                .await?;
            for record in stored {
                next = record.resulting_version().version();
                deltas.push(Arc::new(record));
            }
        }
        if deltas.len() < limit && next < end && next >= memory_start {
            let first = self
                .recent
                .binary_search_by_key(&next, |record| record.applied_at_version.version())
                .map_err(|_| {
                    corrupt(
                        self.wavelet_name(),
                        format!("no delta applies at version {next}"),
                    )
                })?;
            for record in self.recent.iter().skip(first) {
                if record.applied_at_version.version() >= end || deltas.len() >= limit {
                    break;
                }
                next = record.resulting_version().version();
                deltas.push(Arc::clone(record));
            }
        }
        if deltas.len() < limit && next != end {
            return Err(corrupt(
                self.wavelet_name(),
                format!("delta history from version {start} stops at {next}, short of {end}"),
            ));
        }
        Ok(deltas)
    }

    /// Applies a delta to the wavelet and remembers it. On
    /// [`AppendError::Rejected`] nothing has changed.
    ///
    /// `persisted_version` is the version known durable; cached deltas up
    /// to it may be dropped from memory.
    pub(crate) fn append_delta(
        &mut self,
        record: Arc<WaveletDeltaRecord>,
        persisted_version: u64,
    ) -> Result<(), AppendError> {
        if self.current_version() != &record.applied_at_version {
            return Err(AppendError::Corrupted(format!(
                "applied version {} doesn't match current version {}",
                record.applied_at_version.version(),
                self.current_version().version()
            )));
        }
        let delta = &record.transformed;
        match &mut self.snapshot {
            None => {
                // A failed first delta leaves no wavelet behind.
                let wavelet = build_wavelet_from_first_delta(self.access.wavelet_name(), delta)
                    .map_err(AppendError::Rejected)?;
                self.snapshot = Some(wavelet);
            }
            Some(wavelet) => {
                // Dry run on a copy of the parts the delta touches, so that
                // a delta that fails half way leaves no trace.
                let mut scratch = scratch_copy(wavelet, delta).map_err(AppendError::Rejected)?;
                apply_wavelet_delta(delta, &mut scratch).map_err(AppendError::Rejected)?;
                apply_wavelet_delta(delta, wavelet).map_err(|error| {
                    AppendError::Corrupted(format!("validated delta failed to apply: {error}"))
                })?;
            }
        }
        self.recent.push_back(record);
        self.deltas_since_snapshot += 1;
        while self.recent.len() > self.settings.delta_cache_size.max(1)
            && self
                .recent
                .front()
                .is_some_and(|front| front.resulting_version().version() <= persisted_version)
        {
            self.recent.pop_front();
        }
        Ok(())
    }

    /// Serialises the wavelet for storing if a snapshot is due.
    pub(crate) fn snapshot_if_due(&mut self) -> Option<StoredSnapshot> {
        let interval = self.settings.snapshot_interval;
        if interval == 0 || self.deltas_since_snapshot < interval {
            return None;
        }
        let wavelet = self.snapshot.as_ref()?;
        let version = wavelet.hashed_version().clone();
        match serialize_wavelet(wavelet, &version).and_then(|message| encode(&message)) {
            Ok(snapshot) => {
                self.deltas_since_snapshot = 0;
                Some(StoredSnapshot { version, snapshot })
            }
            Err(error) => {
                // Not fatal: the wavelet still loads by replay. Retry after
                // another interval rather than on every delta.
                tracing::warn!(wavelet = %self.wavelet_name(), %error, "cannot snapshot wavelet");
                self.deltas_since_snapshot = 0;
                None
            }
        }
    }
}

/// A copy of `wavelet` holding only the documents `delta` touches. A delta
/// applies to the copy exactly if it applies to the wavelet: operations
/// read nothing but the version, the participants and their own document.
fn scratch_copy(
    wavelet: &WaveletData,
    delta: &TransformedWaveletDelta,
) -> Result<WaveletData, WaveletOperationError> {
    let mut scratch = WaveletData::new(
        wavelet.wavelet_id().clone(),
        wavelet.creator().clone(),
        wavelet.creation_time(),
        wavelet.version(),
        wavelet.hashed_version().clone(),
        wavelet.last_modified_time(),
        wavelet.wave_id().clone(),
    );
    for participant in wavelet.participants() {
        scratch.add_participant(participant.clone());
    }
    for op in delta.ops() {
        let doc_id = match op.kind() {
            WaveletOperationKind::Blip { blip_id, .. } => blip_id.as_str(),
            WaveletOperationKind::VersionUpdate {
                doc_id: Some(doc_id),
                ..
            } => doc_id.as_str(),
            _ => continue,
        };
        if scratch.document(doc_id).is_some() {
            continue;
        }
        if let Some(document) = wavelet.document(doc_id) {
            scratch.create_document(
                doc_id,
                document.author().clone(),
                document.contributors().iter().cloned(),
                &document.content().as_operation(),
                document.last_modified_time(),
                document.last_modified_version(),
            )?;
        }
    }
    Ok(scratch)
}
