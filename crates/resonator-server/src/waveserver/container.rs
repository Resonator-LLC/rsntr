//! A wavelet hosted by this server: its state, the submit pipeline and
//! read access.
//!
//! Derived from `WaveletContainer`, `WaveletContainerImpl`,
//! `LocalWaveletContainer` and `LocalWaveletContainerImpl` in
//! `org.waveprotocol.box.server.waveserver` of Apache Wave, licensed under
//! the Apache License, Version 2.0.
//!
//! Submissions to one wavelet are serialised by its lock. A delta is
//! applied in memory and announced on the wave bus first; its write is
//! queued in the same critical section, and when the storage layer reports
//! the commit, the commit is announced separately.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use resonator_model::data::WaveletData;
use resonator_model::id::{ParticipantId, WaveletName};
use resonator_model::operation::{
    SubmitTransformError, TransformedWaveletDelta, WaveletDelta, transform_submitted_delta,
};
use resonator_model::proto::federation::{ProtocolSignedDelta, ProtocolWaveletDelta};
use resonator_model::proto::{
    ProtoError, applied_delta_epoch, build_applied_delta_in_epoch, build_transformed_delta, decode,
    deserialize_delta, encode, get_hashed_version_applied_at, serialize_delta,
};
use resonator_model::version::HashedVersion;
use tokio::sync::{RwLock, mpsc, watch};

use super::bus::WaveBus;
use super::epoch::{EpochBook, EpochRecord};
use super::error::{SubmitError, WaveServerError, WaveletStateError};
use super::state::{AppendError, KEEP_SNAPSHOTS, LoadReport, StateSettings, WaveletState};
use super::{SubmitReceipt, SubmitResult};
use crate::clock::Clock;
use crate::storage::{
    AppliedDelta, AppliedRequest, DeltaStore, DeltasAccess, OrphanDelta, Rewind, StorageError,
    WaveletDeltaRecord, WriteHandle,
};

/// Tuning of a loaded wavelet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaveletSettings {
    /// A snapshot is stored every this many deltas; 0 for never.
    pub snapshot_interval: u64,
    /// Persisted deltas kept in memory for transforming stale submissions.
    pub delta_cache_size: usize,
}

impl Default for WaveletSettings {
    fn default() -> Self {
        WaveletSettings {
            snapshot_interval: 100,
            delta_cache_size: 256,
        }
    }
}

/// A wavelet's state together with the version known to be durably stored
/// (`CommittedWaveletSnapshot`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedWaveletSnapshot {
    pub snapshot: WaveletData,
    pub committed_version: HashedVersion,
}

#[derive(Debug, Clone)]
struct CommitState {
    version: HashedVersion,
    failed: bool,
}

/// What the container shares with its persister task and the wave map.
struct Shared {
    name: WaveletName,
    bus: Arc<WaveBus>,
    commit: watch::Sender<CommitState>,
    /// Writes queued and not yet reported back.
    pending: AtomicUsize,
    /// The current version number, readable without the lock.
    version: AtomicU64,
    corrupted: AtomicBool,
    /// Replaced after a rewind; rejects everything like a corrupted one.
    retired: AtomicBool,
}

type PersistTicket = (HashedVersion, WriteHandle<()>);

/// Announces commits in the order the writes were queued.
async fn persister(shared: Arc<Shared>, mut tickets: mpsc::UnboundedReceiver<PersistTicket>) {
    while let Some((version, handle)) = tickets.recv().await {
        let result = handle.wait().await;
        // Before the announcement, so that whoever hears of the commit
        // already finds the wavelet quiescent.
        shared.pending.fetch_sub(1, Ordering::SeqCst);
        match result {
            Ok(()) => {
                shared
                    .commit
                    .send_modify(|state| state.version = version.clone());
                shared.bus.publish_committed(&shared.name, &version);
            }
            Err(error) => {
                // Memory is now ahead of storage. Later writes of this
                // wavelet fail too, since they no longer continue the
                // stored history.
                tracing::error!(
                    wavelet = %shared.name,
                    version = version.version(),
                    %error,
                    "failed to persist delta; wavelet will be reloaded from storage"
                );
                shared.corrupted.store(true, Ordering::SeqCst);
                shared.commit.send_modify(|state| state.failed = true);
            }
        }
    }
}

/// What became of an applied delta offered to a replica
/// ([`LocalWaveletContainer::apply_replica_delta`]).
#[derive(Debug, Clone)]
pub enum ReplicaOutcome {
    /// Applied; announced on the wave bus and queued for persisting.
    Applied(Arc<WaveletDeltaRecord>),
    /// The replica already has this very delta.
    AlreadyApplied,
    /// The delta applies at a later version than the replica's current
    /// one: the deltas in between are missing.
    Ahead {
        /// The replica's current version.
        current: HashedVersion,
        /// The version the delta applies at.
        applied_at: HashedVersion,
    },
}

/// What [`LocalWaveletContainer::change_epoch`] did.
#[derive(Debug, Clone)]
pub(crate) enum EpochChange {
    /// The epoch was known already.
    Known,
    /// A newer or conflicting epoch is known: this one.
    Stale(EpochRecord),
    /// Recorded; the history did not reach past the epoch's start.
    Recorded,
    /// Recorded, and the history rewound to `fork`; `rewound` are the
    /// deltas taken out, in order. The container is retired.
    Rewound {
        fork: HashedVersion,
        rewound: Vec<Arc<WaveletDeltaRecord>>,
    },
}

/// What [`LocalWaveletContainer::resubmit_orphans`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResubmitOutcome {
    /// Deltas applied now.
    pub applied: usize,
    /// Deltas already applied or transformed away.
    pub skipped: usize,
    /// The version afterwards.
    pub version: HashedVersion,
}

/// The SHA-256 of an applied delta's serialised signed original: what
/// identifies an orphan wherever it is applied.
pub fn original_sha256(applied: &AppliedDelta) -> Result<Vec<u8>, ProtoError> {
    use sha2::Digest;
    let signed =
        applied
            .message()
            .signed_original_delta
            .as_ref()
            .ok_or(ProtoError::MissingField(
                "ProtocolAppliedWaveletDelta.signed_original_delta",
            ))?;
    Ok(sha2::Sha256::digest(encode(signed)?).to_vec())
}

fn original_delta(applied: &AppliedDelta) -> Result<WaveletDelta, SubmitError> {
    let signed = applied
        .message()
        .signed_original_delta
        .as_ref()
        .and_then(|signed| signed.delta.as_deref())
        .ok_or(ProtoError::MissingField("ProtocolSignedDelta.delta"))
        .map_err(invalid_delta)?;
    let protocol_delta: ProtocolWaveletDelta = decode(signed).map_err(invalid_delta)?;
    deserialize_delta(&protocol_delta).map_err(invalid_delta)
}

/// A wavelet of a local domain (`LocalWaveletContainerImpl`), or the
/// replica of a wavelet hosted elsewhere (`RemoteWaveletContainerImpl`),
/// which only changes through [`LocalWaveletContainer::apply_replica_delta`].
pub struct LocalWaveletContainer {
    shared: Arc<Shared>,
    shared_domain_participant: Option<ParticipantId>,
    clock: Arc<dyn Clock>,
    state: RwLock<WaveletState>,
    tickets: mpsc::UnboundedSender<PersistTicket>,
    load_report: LoadReport,
    /// The host epochs, shared with the wave server; none in tests that
    /// load containers on their own.
    epochs: Option<Arc<EpochBook>>,
}

impl std::fmt::Debug for LocalWaveletContainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalWaveletContainer")
            .field("name", &self.shared.name)
            .field("version", &self.shared.version.load(Ordering::Relaxed))
            .finish()
    }
}

fn invalid_delta(error: ProtoError) -> SubmitError {
    SubmitError::InvalidDelta(error)
}

impl LocalWaveletContainer {
    /// Loads a wavelet from the delta store; a wavelet without deltas
    /// loads as empty. `wave_domain` enables the shared domain participant
    /// `@wave_domain` for access checks. Must be called within a tokio
    /// runtime.
    ///
    /// At most one container may exist per wavelet and database; the wave
    /// server's wave map sees to that.
    pub async fn load(
        store: &DeltaStore,
        wavelet_name: WaveletName,
        bus: Arc<WaveBus>,
        clock: Arc<dyn Clock>,
        wave_domain: Option<&str>,
        settings: WaveletSettings,
    ) -> Result<Self, WaveletStateError> {
        let access = store.open(wavelet_name.clone());
        Self::load_from(access, bus, clock, wave_domain, settings).await
    }

    /// [`LocalWaveletContainer::load`] from the given history, e.g. that
    /// of a replica ([`DeltaStore::open_replica`]).
    pub async fn load_from(
        access: DeltasAccess,
        bus: Arc<WaveBus>,
        clock: Arc<dyn Clock>,
        wave_domain: Option<&str>,
        settings: WaveletSettings,
    ) -> Result<Self, WaveletStateError> {
        let wavelet_name = access.wavelet_name().clone();
        let settings = StateSettings {
            snapshot_interval: settings.snapshot_interval,
            delta_cache_size: settings.delta_cache_size,
        };
        let (state, load_report) =
            WaveletState::load(access, settings)
                .await
                .map_err(|source| WaveletStateError::Load {
                    wavelet: wavelet_name.to_string(),
                    source,
                })?;
        let current = state.current_version().clone();
        let (commit, _) = watch::channel(CommitState {
            version: current.clone(),
            failed: false,
        });
        let shared = Arc::new(Shared {
            name: wavelet_name,
            bus,
            commit,
            pending: AtomicUsize::new(0),
            version: AtomicU64::new(current.version()),
            corrupted: AtomicBool::new(false),
            retired: AtomicBool::new(false),
        });
        let (tickets, receiver) = mpsc::unbounded_channel();
        tokio::spawn(persister(Arc::clone(&shared), receiver));
        Ok(LocalWaveletContainer {
            shared,
            shared_domain_participant: wave_domain
                .and_then(|domain| ParticipantId::shared_domain_participant(domain).ok()),
            clock,
            state: RwLock::new(state),
            tickets,
            load_report,
            epochs: None,
        })
    }

    /// Uses the wave server's host epochs.
    pub(crate) fn with_epochs(mut self, epochs: Arc<EpochBook>) -> Self {
        self.epochs = Some(epochs);
        self
    }

    /// The host epoch active at `version`.
    fn epoch_at(&self, version: u64) -> u64 {
        self.epochs
            .as_ref()
            .map_or(0, |book| book.active_at(&self.shared.name, version))
    }

    pub fn wavelet_name(&self) -> &WaveletName {
        &self.shared.name
    }

    /// The participant standing for everyone of the wave domain, if any.
    pub fn shared_domain_participant(&self) -> Option<&ParticipantId> {
        self.shared_domain_participant.as_ref()
    }

    /// How the wavelet was loaded from storage.
    pub fn load_report(&self) -> LoadReport {
        self.load_report
    }

    /// Whether memory and storage have diverged; such a container rejects
    /// everything and is replaced on the next access through the server.
    pub fn is_corrupted(&self) -> bool {
        self.shared.corrupted.load(Ordering::SeqCst) || self.is_retired()
    }

    /// Whether a rewind replaced this container.
    pub(crate) fn is_retired(&self) -> bool {
        self.shared.retired.load(Ordering::SeqCst)
    }

    /// Whether no write of this wavelet is in flight.
    pub(crate) fn is_quiescent(&self) -> bool {
        self.shared.pending.load(Ordering::SeqCst) == 0
    }

    /// The current version number without taking the lock.
    pub(crate) fn version_hint(&self) -> u64 {
        self.shared.version.load(Ordering::SeqCst)
    }

    fn check_state_ok(&self) -> Result<(), WaveletStateError> {
        if self.is_corrupted() {
            return Err(WaveletStateError::Corrupted);
        }
        Ok(())
    }

    fn storage_error(&self, source: StorageError) -> WaveletStateError {
        WaveletStateError::Storage {
            wavelet: self.shared.name.to_string(),
            source,
        }
    }

    /// Transforms a delta to the current version, applies it, announces it
    /// on the wave bus and queues it for persisting. Returns once the delta
    /// is applied in memory; see [`LocalWaveletContainer::wait_committed`].
    ///
    /// A delta that duplicates one already in the history (same author and
    /// operations, as when a client resubmits after losing the response) is
    /// not applied again; the existing record is returned. If the delta
    /// transforms away, the result is an empty record without an applied
    /// delta.
    ///
    /// The caller is responsible for access control; see
    /// [`LocalWaveletContainer::submit_request_as`].
    pub async fn submit_request(
        &self,
        signed_delta: ProtocolSignedDelta,
    ) -> Result<Arc<WaveletDeltaRecord>, SubmitError> {
        let mut state = self.state.write().await;
        self.check_state_ok()?;
        self.transform_and_apply(&mut state, signed_delta, None)
            .await
            .map(|(record, _)| record)
    }

    /// [`LocalWaveletContainer::submit_request`] after checking that
    /// `author` may write the wavelet. Check and application happen under
    /// one lock, so a participant removed in between cannot slip a delta
    /// in (the original checks first and locks later).
    pub async fn submit_request_as(
        &self,
        author: &ParticipantId,
        signed_delta: ProtocolSignedDelta,
    ) -> Result<Arc<WaveletDeltaRecord>, SubmitError> {
        self.submit_request_with_receipt(author, signed_delta, None)
            .await
            .map(|(record, _)| record)
    }

    /// [`LocalWaveletContainer::submit_request_as`] for a federation
    /// request: if the delta is applied, `receipt` is recorded in the same
    /// transaction as the delta. Returns whether it was.
    pub async fn submit_request_with_receipt(
        &self,
        author: &ParticipantId,
        signed_delta: ProtocolSignedDelta,
        receipt: Option<SubmitReceipt>,
    ) -> Result<(Arc<WaveletDeltaRecord>, bool), SubmitError> {
        let mut state = self.state.write().await;
        self.check_state_ok()?;
        let permitted = state.snapshot().is_none_or(|wavelet| {
            wavelet.check_access_permission(author, self.shared_domain_participant.as_ref())
        });
        if !permitted {
            return Err(SubmitError::NotParticipant {
                author: author.address().to_owned(),
                wavelet: self.shared.name.clone(),
            });
        }
        self.transform_and_apply(&mut state, signed_delta, receipt)
            .await
    }

    async fn transform_and_apply(
        &self,
        state: &mut WaveletState,
        signed_delta: ProtocolSignedDelta,
        receipt: Option<SubmitReceipt>,
    ) -> Result<(Arc<WaveletDeltaRecord>, bool), SubmitError> {
        let delta_bytes = signed_delta
            .delta
            .as_deref()
            .ok_or(ProtoError::MissingField("ProtocolSignedDelta.delta"))
            .map_err(invalid_delta)?;
        let protocol_delta: ProtocolWaveletDelta = decode(delta_bytes).map_err(invalid_delta)?;
        if protocol_delta.operation.is_empty() {
            let version = protocol_delta
                .hashed_version
                .as_ref()
                .and_then(|version| version.version)
                .and_then(|version| u64::try_from(version).ok())
                .unwrap_or(0);
            return Err(SubmitError::EmptyDelta(version));
        }
        let delta = deserialize_delta(&protocol_delta).map_err(invalid_delta)?;
        let current = state.current_version().clone();
        let transformed = self.maybe_transform(state, &delta, &current).await?;
        let timestamp = self.clock.now_millis();

        if transformed.is_empty() {
            // Transformed away: nothing to apply or store.
            let target = transformed.target_version().clone();
            let empty = TransformedWaveletDelta::new(
                transformed.author().clone(),
                target.clone(),
                timestamp,
                Vec::new(),
            )
            .map_err(|e| SubmitError::Internal(e.to_string()))?;
            return Ok((
                Arc::new(WaveletDeltaRecord::new(target, None, empty)),
                false,
            ));
        }

        if transformed.target_version() != &current {
            // A duplicate of a delta already applied: answer with that one,
            // which makes resubmission idempotent.
            let existing = state
                .delta_applied_at(transformed.target_version())
                .await
                .map_err(|e| self.storage_error(e))?
                .ok_or_else(|| {
                    SubmitError::Internal(format!(
                        "duplicate delta detected at version {} but no delta was applied there",
                        transformed.target_version().version()
                    ))
                })?;
            if existing.author() != transformed.author()
                || existing.transformed.ops() != transformed.ops()
            {
                return Err(SubmitError::Internal(
                    "Duplicate delta detected but mismatched author or ops".to_owned(),
                ));
            }
            tracing::info!(
                wavelet = %self.shared.name,
                version = existing.applied_at_version.version(),
                "duplicate delta"
            );
            return Ok((existing, false));
        }

        let operations = u32::try_from(transformed.len())
            .map_err(|_| SubmitError::Internal("too many operations".to_owned()))?;
        let epoch = self.epoch_at(current.version());
        let applied = build_applied_delta_in_epoch(
            signed_delta,
            &current,
            operations,
            timestamp,
            epoch,
            None,
        )
        .map_err(invalid_delta)?;
        let transformed_delta =
            build_transformed_delta(&applied, &transformed).map_err(invalid_delta)?;
        let record = Arc::new(WaveletDeltaRecord::new(
            current,
            Some(applied),
            transformed_delta,
        ));
        let applied = receipt.map(|receipt| receipt.into_row(&SubmitResult::of(&record)));
        let recorded = applied.is_some();
        self.apply_and_persist(state, &record, applied).await?;
        Ok((record, recorded))
    }

    /// Transforms a submitted delta against the history since its target
    /// version (`maybeTransformSubmittedDelta`).
    async fn maybe_transform(
        &self,
        state: &WaveletState,
        delta: &WaveletDelta,
        current: &HashedVersion,
    ) -> Result<WaveletDelta, SubmitError> {
        let target = delta.target_version();
        let mut server_deltas = Vec::new();
        if target != current {
            if target.version() == current.version() {
                return Err(SubmitError::InvalidHash {
                    expected: current.clone(),
                    target: target.clone(),
                });
            }
            // The transform trusts that the history starts at the target
            // version, so that must be a real point of it.
            let known = state
                .hashed_version(target.version())
                .await
                .map_err(|e| self.storage_error(e))?;
            if known.as_ref() != Some(target) {
                return Err(SubmitError::UnknownTargetVersion(target.version()));
            }
            let history = state
                .history(target.version(), current.version(), usize::MAX)
                .await
                .map_err(|e| self.storage_error(e))?;
            server_deltas = history
                .iter()
                .map(|record| record.transformed.clone())
                .collect();
        }
        transform_submitted_delta(delta, current, &server_deltas).map_err(|error| match error {
            SubmitTransformError::InvalidHash { expected, target } => {
                SubmitError::InvalidHash { expected, target }
            }
            SubmitTransformError::Transform(error) => SubmitError::Transform(error),
            error @ SubmitTransformError::History { .. } => {
                SubmitError::Internal(error.to_string())
            }
        })
    }

    async fn apply_and_persist(
        &self,
        state: &mut WaveletState,
        record: &Arc<WaveletDeltaRecord>,
        applied: Option<AppliedRequest>,
    ) -> Result<(), SubmitError> {
        let persisted = self.shared.commit.borrow().version.version();
        match state.append_delta(Arc::clone(record), persisted) {
            Ok(()) => {}
            Err(AppendError::Rejected(error)) => return Err(SubmitError::Operation(error)),
            Err(AppendError::Corrupted(message)) => {
                self.shared.corrupted.store(true, Ordering::SeqCst);
                tracing::error!(wavelet = %self.shared.name, %message, "wavelet state corrupted");
                return Err(SubmitError::Internal(message));
            }
        }
        let resulting = record.resulting_version().clone();
        self.shared
            .version
            .store(resulting.version(), Ordering::SeqCst);
        let snapshot = state.snapshot_if_due();
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        let handle = match state
            .access()
            .enqueue_append(&[record.as_ref()], snapshot, KEEP_SNAPSHOTS, applied)
            .await
        {
            Ok(handle) => handle,
            Err(error) => {
                // Applied but never announced and not going to be stored.
                self.shared.pending.fetch_sub(1, Ordering::SeqCst);
                self.shared.corrupted.store(true, Ordering::SeqCst);
                self.shared.commit.send_modify(|state| state.failed = true);
                return Err(self.storage_error(error).into());
            }
        };
        if let Some(wavelet) = state.snapshot() {
            self.shared
                .bus
                .publish_update(wavelet, std::slice::from_ref(record));
        }
        // The persister announces the commit; it only sees the ticket after
        // the update went out.
        if self.tickets.send((resulting, handle)).is_err() {
            self.shared.pending.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(())
    }

    /// Offers a delta applied by the wavelet's remote host to this replica
    /// (`RemoteWaveletContainerImpl.update`). The delta must continue the
    /// hash chain at the current version: its original is transformed
    /// against the history since its target version, as the host did, and
    /// must yield as many operations as the host applied. The caller has
    /// checked its signatures; access was checked by the host.
    pub async fn apply_replica_delta(
        &self,
        applied: AppliedDelta,
    ) -> Result<ReplicaOutcome, SubmitError> {
        let mut state = self.state.write().await;
        self.check_state_ok()?;
        let applied_at = get_hashed_version_applied_at(&applied).map_err(invalid_delta)?;
        let current = state.current_version().clone();
        if applied_at.version() > current.version() {
            return Ok(ReplicaOutcome::Ahead {
                current,
                applied_at,
            });
        }
        if applied_at.version() < current.version() {
            let existing = state
                .delta_applied_at(&applied_at)
                .await
                .map_err(|e| self.storage_error(e))?;
            return match existing {
                Some(record) if record.applied.as_ref() == Some(&applied) => {
                    Ok(ReplicaOutcome::AlreadyApplied)
                }
                _ => Err(SubmitError::InvalidHash {
                    expected: state
                        .hashed_version(applied_at.version())
                        .await
                        .map_err(|e| self.storage_error(e))?
                        .unwrap_or_else(|| applied_at.clone()),
                    target: applied_at,
                }),
            };
        }
        if applied_at != current {
            return Err(SubmitError::InvalidHash {
                expected: current,
                target: applied_at,
            });
        }
        let expected = self.epoch_at(current.version());
        let epoch = applied_delta_epoch(&applied);
        if epoch != expected {
            return Err(SubmitError::Internal(format!(
                "delta at version {} of host epoch {epoch}, but epoch {expected} is active there",
                current.version()
            )));
        }
        let transformed = if let Some(rebased) = &applied.message().rebased_operations {
            // A resubmitted orphan, rebased by the host: its original
            // targets a version outside this history.
            deserialize_delta(rebased).map_err(invalid_delta)?
        } else {
            let signed = applied
                .message()
                .signed_original_delta
                .as_ref()
                .and_then(|signed| signed.delta.as_deref())
                .ok_or(ProtoError::MissingField("ProtocolSignedDelta.delta"))
                .map_err(invalid_delta)?;
            let protocol_delta: ProtocolWaveletDelta = decode(signed).map_err(invalid_delta)?;
            let delta = deserialize_delta(&protocol_delta).map_err(invalid_delta)?;
            self.maybe_transform(&state, &delta, &current).await?
        };
        if transformed.target_version() != &current {
            return Err(SubmitError::Internal(format!(
                "remote delta transformed to version {} instead of {}",
                transformed.target_version().version(),
                current.version()
            )));
        }
        let transformed_delta =
            build_transformed_delta(&applied, &transformed).map_err(invalid_delta)?;
        let record = Arc::new(WaveletDeltaRecord::new(
            current,
            Some(applied),
            transformed_delta,
        ));
        self.apply_and_persist(&mut state, &record, None).await?;
        Ok(ReplicaOutcome::Applied(record))
    }

    /// Moves the wavelet into a newer host epoch (see [`super::epoch`]).
    /// If the history goes beyond the epoch's start, it is rewound there in
    /// one write transaction, the deltas taken out are kept as orphans and
    /// the container retires (the wave server loads a new one). The epoch
    /// is recorded either way.
    pub(crate) async fn change_epoch(
        &self,
        record: EpochRecord,
        hosted_by: Option<String>,
        store: &DeltaStore,
        now: i64,
    ) -> Result<EpochChange, SubmitError> {
        let state = self.state.write().await;
        self.check_state_ok()?;
        let Some(book) = self.epochs.as_ref() else {
            return Err(SubmitError::Internal("no epoch book".to_owned()));
        };
        let name = &self.shared.name;
        if let Some(latest) = book.latest(name)
            && latest.epoch >= record.epoch
        {
            return Ok(
                if latest.epoch == record.epoch && latest.start == record.start {
                    EpochChange::Known
                } else {
                    EpochChange::Stale(latest)
                },
            );
        }
        let current = state.current_version().clone();
        let start = record.start.clone();
        if current.version() > start.version() || current == start {
            let known = state
                .hashed_version(start.version())
                .await
                .map_err(|e| self.storage_error(e))?;
            if known.as_ref() != Some(&start) {
                return Err(SubmitError::InvalidHash {
                    expected: known.unwrap_or_else(|| current.clone()),
                    target: start,
                });
            }
        } else if current.version() == start.version() {
            return Err(SubmitError::InvalidHash {
                expected: current,
                target: start,
            });
        }
        if current.version() <= start.version() {
            store
                .put_epoch(name, record.stored(name), hosted_by)
                .await
                .map_err(|e| self.storage_error(e))?;
            book.insert(name, record);
            return Ok(EpochChange::Recorded);
        }
        // Everything applied must be stored before the cut; nothing new
        // comes in while the lock is held.
        let mut commit = self.shared.commit.subscribe();
        let failed = commit
            .wait_for(|state| state.failed || state.version.version() >= current.version())
            .await
            .map_err(|_| WaveletStateError::Corrupted)?
            .failed;
        if failed {
            return Err(WaveletStateError::Corrupted.into());
        }
        let rewound = state
            .history(start.version(), current.version(), usize::MAX)
            .await
            .map_err(|e| self.storage_error(e))?;
        let uri = resonator_model::id::IdUriCodec::wavelet_name_to_uri(name);
        let mut orphans = Vec::with_capacity(rewound.len());
        for (seq, delta) in rewound.iter().enumerate() {
            let Some(applied) = &delta.applied else {
                continue;
            };
            orphans.push(OrphanDelta {
                wavelet: uri.clone(),
                sha256: original_sha256(applied).map_err(invalid_delta)?,
                fork: start.clone(),
                seq: seq as u64,
                applied: applied.bytes().to_vec(),
            });
        }
        let rewind = Rewind {
            epoch: Some(record.stored(name)),
            orphans,
            hosted_by,
            now,
        };
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        let written = match store.rewind(name, &start, rewind).await {
            Ok(handle) => handle.wait().await,
            Err(error) => Err(error),
        };
        self.shared.pending.fetch_sub(1, Ordering::SeqCst);
        if let Err(error) = written {
            // Storage may or may not have the cut; start over from it.
            self.shared.corrupted.store(true, Ordering::SeqCst);
            return Err(self.storage_error(error).into());
        }
        book.insert(name, record);
        self.shared.retired.store(true, Ordering::SeqCst);
        tracing::info!(
            wavelet = %name,
            from = current.version(),
            to = start.version(),
            orphans = rewound.len(),
            "rewound to the start of a newer host epoch"
        );
        Ok(EpochChange::Rewound {
            fork: start,
            rewound,
        })
    }

    /// Applies orphaned deltas resubmitted after a rewind elsewhere: a
    /// chain of applied deltas from `fork`, a version of this history, as
    /// some former host applied them. Each is rebased onto the history
    /// since the fork, the way a client's concurrency control rebases its
    /// pending deltas: a delta of the history whose signed original is the
    /// next of the chain is that delta, already applied (by an earlier
    /// resubmission); any other is transformed through the rest of the
    /// chain. What is left is applied with its signed original unchanged
    /// and the rebased operations recorded beside it. `permitted` decides
    /// whether an author may write; the first one who may not stops the
    /// rest.
    pub(crate) async fn resubmit_orphans(
        &self,
        fork: &HashedVersion,
        chain: Vec<AppliedDelta>,
        permitted: impl Fn(&WaveletData, &ParticipantId) -> bool,
    ) -> Result<ResubmitOutcome, SubmitError> {
        use resonator_model::operation::transform_op_lists;
        let mut state = self.state.write().await;
        self.check_state_ok()?;
        let current = state.current_version().clone();
        let known = state
            .hashed_version(fork.version())
            .await
            .map_err(|e| self.storage_error(e))?;
        if known.as_ref() != Some(fork) {
            return Err(SubmitError::UnknownTargetVersion(fork.version()));
        }
        // The chain's operations as applied on the fork, checking that it
        // continues the hash chain from the fork.
        let mut fork_deltas: Vec<TransformedWaveletDelta> = Vec::new();
        let mut pending: Vec<(Vec<u8>, AppliedDelta, WaveletDelta)> = Vec::new();
        let mut at = fork.clone();
        for applied in chain {
            let applied_at = get_hashed_version_applied_at(&applied).map_err(invalid_delta)?;
            if applied_at != at {
                return Err(SubmitError::InvalidHash {
                    expected: at,
                    target: applied_at,
                });
            }
            let transformed = if let Some(rebased) = &applied.message().rebased_operations {
                deserialize_delta(rebased).map_err(invalid_delta)?
            } else {
                let original = original_delta(&applied)?;
                self.transform_on_fork(&state, &original, fork, &fork_deltas, &at)
                    .await?
            };
            if transformed.target_version() != &at {
                return Err(SubmitError::Internal(format!(
                    "orphan transformed to version {} instead of {}",
                    transformed.target_version().version(),
                    at.version()
                )));
            }
            let delta = build_transformed_delta(&applied, &transformed).map_err(invalid_delta)?;
            at = delta.resulting_version().clone();
            fork_deltas.push(delta);
            let sha = original_sha256(&applied).map_err(invalid_delta)?;
            pending.push((sha, applied, transformed));
        }
        let total = pending.len();
        // Rebase onto the history since the fork.
        let since_fork = state
            .history(fork.version(), current.version(), usize::MAX)
            .await
            .map_err(|e| self.storage_error(e))?;
        let mut skipped = 0usize;
        for server in &since_fork {
            let Some(applied) = &server.applied else {
                continue;
            };
            let sha = original_sha256(applied).map_err(invalid_delta)?;
            if let Some(index) = pending
                .iter()
                .position(|(pending_sha, ..)| *pending_sha == sha)
            {
                // Applied before; whatever precedes it in the chain must
                // have transformed away then.
                if pending[..index]
                    .iter()
                    .any(|(_, _, delta)| !delta.is_empty())
                {
                    return Err(SubmitError::Internal(
                        "orphaned deltas were applied out of order".to_owned(),
                    ));
                }
                skipped += index + 1;
                pending.drain(..=index);
                continue;
            }
            let mut server_ops = server.transformed.ops().to_vec();
            for (_, _, delta) in pending.iter_mut() {
                let (client, carried) =
                    transform_op_lists(delta.ops(), &server_ops).map_err(SubmitError::Transform)?;
                server_ops = carried;
                *delta = WaveletDelta::new(
                    delta.author().clone(),
                    server.resulting_version().clone(),
                    client,
                );
            }
        }
        let mut applied_count = 0usize;
        for (_, applied, delta) in pending {
            if delta.is_empty() {
                skipped += 1;
                continue;
            }
            let author = delta.author().clone();
            if let Some(wavelet) = state.snapshot()
                && !permitted(wavelet, &author)
            {
                return Err(SubmitError::NotParticipant {
                    author: author.address().to_owned(),
                    wavelet: self.shared.name.clone(),
                });
            }
            let current = state.current_version().clone();
            let rebased = WaveletDelta::new(author, current.clone(), delta.ops().to_vec());
            let operations = u32::try_from(rebased.len())
                .map_err(|_| SubmitError::Internal("too many operations".to_owned()))?;
            let signed = applied
                .message()
                .signed_original_delta
                .clone()
                .ok_or(ProtoError::MissingField("ProtocolSignedDelta.delta"))
                .map_err(invalid_delta)?;
            let new_applied = build_applied_delta_in_epoch(
                signed,
                &current,
                operations,
                self.clock.now_millis(),
                self.epoch_at(current.version()),
                Some(serialize_delta(&rebased).map_err(invalid_delta)?),
            )
            .map_err(invalid_delta)?;
            let transformed =
                build_transformed_delta(&new_applied, &rebased).map_err(invalid_delta)?;
            let record = Arc::new(WaveletDeltaRecord::new(
                current,
                Some(new_applied),
                transformed,
            ));
            self.apply_and_persist(&mut state, &record, None).await?;
            applied_count += 1;
        }
        tracing::info!(
            wavelet = %self.shared.name,
            fork = fork.version(),
            total,
            applied = applied_count,
            skipped,
            "orphaned deltas resubmitted"
        );
        Ok(ResubmitOutcome {
            applied: applied_count,
            skipped,
            version: state.current_version().clone(),
        })
    }

    /// Transforms an orphan's original to apply at `at` on a fork: against
    /// this history from its target up to `fork`, then the fork's deltas.
    async fn transform_on_fork(
        &self,
        state: &WaveletState,
        original: &WaveletDelta,
        fork: &HashedVersion,
        fork_deltas: &[TransformedWaveletDelta],
        at: &HashedVersion,
    ) -> Result<WaveletDelta, SubmitError> {
        let target = original.target_version();
        let mut history: Vec<TransformedWaveletDelta> = Vec::new();
        if target.version() < fork.version() {
            let known = state
                .hashed_version(target.version())
                .await
                .map_err(|e| self.storage_error(e))?;
            if known.as_ref() != Some(target) {
                return Err(SubmitError::UnknownTargetVersion(target.version()));
            }
            history.extend(
                state
                    .history(target.version(), fork.version(), usize::MAX)
                    .await
                    .map_err(|e| self.storage_error(e))?
                    .iter()
                    .map(|record| record.transformed.clone()),
            );
            history.extend(fork_deltas.iter().cloned());
        } else if target == fork {
            history.extend(fork_deltas.iter().cloned());
        } else {
            let index = fork_deltas
                .iter()
                .position(|delta| delta.resulting_version() == target)
                .ok_or(SubmitError::UnknownTargetVersion(target.version()))?;
            history.extend(fork_deltas[index + 1..].iter().cloned());
        }
        transform_submitted_delta(original, at, &history).map_err(|error| match error {
            SubmitTransformError::InvalidHash { expected, target } => {
                SubmitError::InvalidHash { expected, target }
            }
            SubmitTransformError::Transform(error) => SubmitError::Transform(error),
            error @ SubmitTransformError::History { .. } => {
                SubmitError::Internal(error.to_string())
            }
        })
    }

    /// The hashed version at `version` if that is a delta boundary of the
    /// history.
    pub async fn hashed_version_at(
        &self,
        version: u64,
    ) -> Result<Option<HashedVersion>, WaveletStateError> {
        let state = self.state.read().await;
        self.check_state_ok()?;
        state
            .hashed_version(version)
            .await
            .map_err(|e| self.storage_error(e))
    }

    /// Waits until the wavelet's history up to `version` is durably stored
    /// and returns the committed version reached.
    pub async fn wait_committed(&self, version: u64) -> Result<HashedVersion, WaveletStateError> {
        let mut commit = self.shared.commit.subscribe();
        let state = commit
            .wait_for(|state| state.failed || state.version.version() >= version)
            .await
            .map_err(|_| WaveletStateError::Corrupted)?;
        if state.version.version() >= version {
            Ok(state.version.clone())
        } else {
            Err(WaveletStateError::Corrupted)
        }
    }

    /// Whether `participant` may read and write the wavelet: anyone logged
    /// in if it is empty (to write the first delta), otherwise its
    /// participants, or anyone if the shared domain participant is one.
    /// `None` stands for a user who is not logged in.
    pub async fn check_access_permission(
        &self,
        participant: Option<&ParticipantId>,
    ) -> Result<bool, WaveletStateError> {
        let state = self.state.read().await;
        self.check_state_ok()?;
        let Some(participant) = participant else {
            return Ok(false);
        };
        Ok(match state.snapshot() {
            None => true,
            Some(wavelet) => wavelet
                .check_access_permission(participant, self.shared_domain_participant.as_ref()),
        })
    }

    /// The version up to which the history is durably stored.
    pub async fn last_committed_version(&self) -> Result<HashedVersion, WaveletStateError> {
        let _state = self.state.read().await;
        self.check_state_ok()?;
        Ok(self.shared.commit.borrow().version.clone())
    }

    pub async fn current_version(&self) -> Result<HashedVersion, WaveletStateError> {
        let state = self.state.read().await;
        self.check_state_ok()?;
        Ok(state.current_version().clone())
    }

    /// The hashed version before any delta.
    pub async fn version_zero(&self) -> HashedVersion {
        self.state.read().await.version_zero().clone()
    }

    /// A copy of the wavelet's data; `None` if it has no deltas.
    pub async fn copy_wavelet_data(&self) -> Result<Option<WaveletData>, WaveletStateError> {
        self.apply_function(|wavelet| wavelet.cloned()).await
    }

    /// A copy of the wavelet's data with the committed version; `None` if
    /// it has no deltas.
    pub async fn get_snapshot(
        &self,
    ) -> Result<Option<CommittedWaveletSnapshot>, WaveletStateError> {
        let state = self.state.read().await;
        self.check_state_ok()?;
        Ok(state.snapshot().map(|wavelet| CommittedWaveletSnapshot {
            snapshot: wavelet.clone(),
            committed_version: self.shared.commit.borrow().version.clone(),
        }))
    }

    /// Runs `function` on the wavelet's data (`None` if it has no deltas)
    /// under the read lock, without copying it. `function` must be quick.
    pub async fn apply_function<T>(
        &self,
        function: impl FnOnce(Option<&WaveletData>) -> T,
    ) -> Result<T, WaveletStateError> {
        let state = self.state.read().await;
        self.check_state_ok()?;
        Ok(function(state.snapshot()))
    }

    pub async fn has_participant(
        &self,
        participant: &ParticipantId,
    ) -> Result<bool, WaveletStateError> {
        self.apply_function(|wavelet| {
            wavelet.is_some_and(|wavelet| wavelet.participants().contains(participant))
        })
        .await
    }

    pub async fn creator(&self) -> Result<Option<ParticipantId>, WaveletStateError> {
        self.apply_function(|wavelet| wavelet.map(|wavelet| wavelet.creator().clone()))
            .await
    }

    /// Whether the wavelet has no deltas.
    pub async fn is_empty(&self) -> Result<bool, WaveletStateError> {
        self.apply_function(|wavelet| wavelet.is_none()).await
    }

    async fn check_version_is_delta_boundary(
        &self,
        state: &WaveletState,
        version: &HashedVersion,
        what: &str,
    ) -> Result<(), WaveServerError> {
        let actual = state
            .hashed_version(version.version())
            .await
            .map_err(|e| self.storage_error(e))?;
        if actual.as_ref() != Some(version) {
            // The hash is left out of the message so as not to leak it.
            return Err(WaveServerError::AccessControl(format!(
                "Unrecognized {what} at version {}",
                version.version()
            )));
        }
        Ok(())
    }

    /// The delta records from `start_version` to `end_version`, both of
    /// which must be delta boundaries of the history with matching hashes.
    /// With a `limit`, at most that many deltas are returned and the caller
    /// continues from the resulting version of the last one.
    pub async fn request_history_records(
        &self,
        start_version: &HashedVersion,
        end_version: &HashedVersion,
        limit: Option<usize>,
    ) -> Result<Vec<Arc<WaveletDeltaRecord>>, WaveServerError> {
        let state = self.state.read().await;
        self.check_state_ok()?;
        self.check_version_is_delta_boundary(&state, start_version, "start version")
            .await?;
        self.check_version_is_delta_boundary(&state, end_version, "end version")
            .await?;
        if start_version.version() > end_version.version() {
            return Err(WaveServerError::AccessControl(format!(
                "Start version {} is after end version {}",
                start_version.version(),
                end_version.version()
            )));
        }
        Ok(state
            .history(
                start_version.version(),
                end_version.version(),
                limit.unwrap_or(usize::MAX),
            )
            .await
            .map_err(|e| self.storage_error(e))?)
    }

    /// The applied deltas between two versions (`requestHistory`).
    pub async fn request_history(
        &self,
        start_version: &HashedVersion,
        end_version: &HashedVersion,
        limit: Option<usize>,
    ) -> Result<Vec<AppliedDelta>, WaveServerError> {
        let records = self
            .request_history_records(start_version, end_version, limit)
            .await?;
        Ok(records
            .iter()
            .filter_map(|record| record.applied.clone())
            .collect())
    }

    /// The transformed deltas between two versions
    /// (`requestTransformedHistory`).
    pub async fn request_transformed_history(
        &self,
        start_version: &HashedVersion,
        end_version: &HashedVersion,
        limit: Option<usize>,
    ) -> Result<Vec<TransformedWaveletDelta>, WaveServerError> {
        let records = self
            .request_history_records(start_version, end_version, limit)
            .await?;
        Ok(records
            .iter()
            .map(|record| record.transformed.clone())
            .collect())
    }

    /// Whether the delta leading to `version` carries a signature by
    /// `signer_id` (`isDeltaSigner`).
    pub async fn is_delta_signer(
        &self,
        version: &HashedVersion,
        signer_id: &[u8],
    ) -> Result<bool, WaveletStateError> {
        let state = self.state.read().await;
        self.check_state_ok()?;
        let record = state
            .delta_by_end_version(version)
            .await
            .map_err(|e| self.storage_error(e))?;
        Ok(record
            .as_ref()
            .and_then(|record| record.applied.as_ref())
            .and_then(|applied| applied.message().signed_original_delta.as_ref())
            .is_some_and(|signed| {
                signed
                    .signature
                    .iter()
                    .any(|signature| signature.signer_id.as_deref() == Some(signer_id))
            }))
    }
}
