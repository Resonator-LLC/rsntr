//! The wave server: accepts delta submissions, serves snapshots and
//! history, and announces updates on the wave bus.
//!
//! Derived from `WaveServerImpl`, `WaveletProvider` and their companions in
//! `org.waveprotocol.box.server.waveserver` of Apache Wave, licensed under
//! the Apache License, Version 2.0.
//!
//! Wavelets of the wave domain are hosted here. Deltas are wrapped in a
//! `ProtocolSignedDelta` without signatures (as `NonSigningSignatureHandler`
//! does), because history hashes are computed over that envelope, unless a
//! [`DeltaSigner`] is configured (federation; see [`signer`]).
//!
//! With federation, wavelets of other domains are held as read-only
//! replicas fed by their hosts ([`WaveServer::apply_replica_delta`]), and
//! submissions to them are forwarded to the host through a [`RemoteHost`].
//! Replicas live in the same tables and go through the same wave bus as
//! local wavelets, so readers do not tell them apart.

mod bus;
mod container;
mod epoch;
mod error;
pub mod fork;
pub mod signer;
mod state;
#[cfg(test)]
mod tests;
mod wave_map;

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures_util::future::BoxFuture;

use resonator_model::data::WaveletData;
use resonator_model::id::{ParticipantId, WaveId, WaveletId, WaveletName};
use resonator_model::operation::{TransformedWaveletDelta, WaveletDelta};
use resonator_model::proto::federation::{ProtocolSignedDelta, ProtocolWaveletDelta};
use resonator_model::proto::{ByteStringMessage, serialize_delta};
use resonator_model::version::{HashedVersion, HashedVersionFactory};

pub use bus::{SubscriptionId, WaveBus, WaveBusEvent, WaveBusSubscriber};
pub use container::{
    CommittedWaveletSnapshot, LocalWaveletContainer, ReplicaOutcome, ResubmitOutcome,
    WaveletSettings, original_sha256,
};
pub use epoch::EpochRecord;
pub use error::{SubmitError, SubmitErrorKind, WaveServerError, WaveletStateError};
pub use signer::{DeltaSigner, SharedDeltaSigner};
pub use state::LoadReport;

use crate::clock::{Clock, SystemClock};
use crate::config::Config;
use crate::storage::{AppliedDelta, Database, DeltaStore, StorageError, WaveletDeltaRecord};
use container::EpochChange;
use epoch::EpochBook;
use wave_map::WaveMap;

/// The outcome of a successful submission
/// (`SubmitRequestListener.onSuccess`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitResult {
    /// Number of operations applied; zero if the delta transformed away.
    pub operations_applied: usize,
    /// The wavelet version after the delta. For a resubmitted duplicate,
    /// the version after the delta's first application.
    pub hashed_version_after_application: HashedVersion,
    /// When the delta was applied, in milliseconds since the epoch.
    pub application_timestamp: i64,
    /// Set when the host of the wavelet could not be reached and the
    /// delta waits here for it (the `pending_submits` row): nothing is
    /// applied yet, `hashed_version_after_application` is the version the
    /// delta was made against. [`WaveServer::wait_pending`] waits for the
    /// host's answer.
    pub pending: Option<i64>,
}

/// Encodes the answer to a submission for [`SubmitReceipt::response`].
pub type EncodeResponse = Box<dyn FnOnce(&SubmitResult) -> Option<Vec<u8>> + Send>;

/// A federation request that submits a delta, to be recorded as applied
/// in the same transaction as the delta, so that a retry finds it even
/// after a crash.
pub struct SubmitReceipt {
    pub request_id: String,
    /// The domain the request came from.
    pub domain: String,
    /// Milliseconds since the epoch.
    pub applied_at: i64,
    /// The response to keep for retries, made from the result.
    pub response: EncodeResponse,
}

impl std::fmt::Debug for SubmitReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubmitReceipt")
            .field("request_id", &self.request_id)
            .field("domain", &self.domain)
            .finish_non_exhaustive()
    }
}

impl SubmitReceipt {
    pub(crate) fn into_row(self, result: &SubmitResult) -> crate::storage::AppliedRequest {
        crate::storage::AppliedRequest {
            response: (self.response)(result),
            request_id: self.request_id,
            domain: self.domain,
            applied_at: self.applied_at,
        }
    }
}

impl SubmitResult {
    pub(crate) fn of(record: &WaveletDeltaRecord) -> Self {
        SubmitResult {
            operations_applied: record.transformed.len(),
            hashed_version_after_application: record.resulting_version().clone(),
            application_timestamp: record.application_timestamp(),
            pending: None,
        }
    }
}

/// Provides wavelet snapshots and history and accepts delta submissions
/// (`WaveletProvider`). Implemented by [`WaveServer`]; front ends can be
/// written against the trait.
///
/// Access control is the caller's business except where stated: use
/// [`WaveletProvider::check_access_permission`] before serving reads.
pub trait WaveletProvider: Send + Sync {
    /// Submits a delta to a wavelet. The delta's author must be allowed to
    /// write the wavelet: a participant, anyone if the shared domain
    /// participant is one, or anyone if the wavelet is still empty.
    /// Resolves when the delta is applied in memory; durability is
    /// announced on the wave bus.
    fn submit_request(
        &self,
        wavelet_name: &WaveletName,
        delta: ProtocolWaveletDelta,
    ) -> impl Future<Output = Result<SubmitResult, SubmitError>> + Send;

    /// The transformed deltas from `version_start` to `version_end`, which
    /// must both be delta boundaries of the wavelet's history.
    fn get_history(
        &self,
        wavelet_name: &WaveletName,
        version_start: &HashedVersion,
        version_end: &HashedVersion,
    ) -> impl Future<Output = Result<Vec<TransformedWaveletDelta>, WaveServerError>> + Send;

    /// Whether the wavelet exists and `participant` may access it. `None`
    /// stands for a user who is not logged in.
    fn check_access_permission(
        &self,
        wavelet_name: &WaveletName,
        participant: Option<&ParticipantId>,
    ) -> impl Future<Output = Result<bool, WaveServerError>> + Send;

    /// The ids of all waves with a non-empty wavelet.
    fn get_wave_ids(&self) -> impl Future<Output = Result<Vec<WaveId>, WaveServerError>> + Send;

    /// The ids of the non-empty wavelets of a wave.
    fn get_wavelet_ids(
        &self,
        wave_id: &WaveId,
    ) -> impl Future<Output = Result<BTreeSet<WaveletId>, WaveServerError>> + Send;

    /// The current state of a wavelet with its committed version; `None`
    /// if the wavelet does not exist.
    fn get_snapshot(
        &self,
        wavelet_name: &WaveletName,
    ) -> impl Future<Output = Result<Option<CommittedWaveletSnapshot>, WaveServerError>> + Send;
}

/// Settings of a [`WaveServer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaveServerOptions {
    /// The wave domain; wavelets whose id has this domain are local.
    pub domain: String,
    /// Waves kept in memory. The bound is soft: waves in use stay.
    pub wave_cache_size: usize,
    /// How long an unused wave stays in memory.
    pub wave_cache_expire: Duration,
    pub wavelet: WaveletSettings,
    /// Signs the deltas submitted through this server; `None` (the
    /// default) leaves them unsigned.
    pub delta_signer: Option<SharedDeltaSigner>,
}

impl WaveServerOptions {
    pub fn new(domain: impl Into<String>) -> Self {
        WaveServerOptions {
            domain: domain.into(),
            wave_cache_size: 1000,
            wave_cache_expire: Duration::from_secs(60 * 60),
            wavelet: WaveletSettings::default(),
            delta_signer: None,
        }
    }

    pub fn from_config(config: &Config) -> Self {
        WaveServerOptions {
            domain: config.domain.clone(),
            wave_cache_size: config.wave_cache_size,
            wave_cache_expire: config.wave_cache_expire(),
            wavelet: WaveletSettings {
                snapshot_interval: config.snapshot_interval,
                delta_cache_size: config.delta_cache_size,
            },
            delta_signer: None,
        }
    }
}

/// Where a wavelet lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaveletHost {
    /// Hosted by this server.
    Local,
    /// A wavelet of another domain. Its container is a replica (marked
    /// `hosted_by` in storage), fed by the host's signed delta updates
    /// through [`WaveServer::apply_replica_delta`]; submissions to it go to
    /// the host through the [`RemoteHost`] (`RemoteWaveletContainerImpl`
    /// and `WaveletFederationProvider` in the original). Without a remote
    /// host (federation disabled) they are rejected as
    /// [`SubmitError::NotLocal`].
    Remote { domain: String },
    /// Person mode: a wavelet of this node's own (person) domain that
    /// another device of the same person hosts, the device with key
    /// `holder`. Handled like [`WaveletHost::Remote`]: a replica here,
    /// submissions forwarded to the holder. Who the holder is comes from
    /// the newest host epoch of the wavelet, else from
    /// [`PersonHosting::holder`] (the configuration now, the host lease
    /// later).
    Delegated {
        /// The person domain (this server's domain).
        domain: String,
        /// The raw 32-byte device key of the holder.
        holder: Vec<u8>,
    },
}

impl WaveletHost {
    /// Whether this server hosts the wavelet.
    pub fn is_local(&self) -> bool {
        matches!(self, WaveletHost::Local)
    }

    /// The domain of the host: this server's for local wavelets.
    pub fn domain(&self) -> Option<&str> {
        match self {
            WaveletHost::Local => None,
            WaveletHost::Remote { domain } | WaveletHost::Delegated { domain, .. } => Some(domain),
        }
    }
}

/// Person mode: this server is one device of a person, whose domain is
/// the wave domain. Several devices share that domain; only one hosts its
/// wavelets at a time, so wavelets of the domain are local only on that
/// device and [`WaveletHost::Delegated`] on the others.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonHosting {
    /// This device's raw 32-byte key (the key its host epochs carry).
    pub device: Vec<u8>,
    /// The device hosting the person's wavelets that have no host epoch;
    /// `None` for this device.
    pub holder: Option<Vec<u8>>,
}

impl PersonHosting {
    /// Person mode before any host lease is known: no device is known to
    /// host the person's wavelets, so submissions to them wait
    /// ([`WaveServer::drain_pending`]).
    pub fn unknown(device: Vec<u8>) -> Self {
        PersonHosting {
            device,
            holder: Some(Vec::new()),
        }
    }
}

/// How long a submission the host applied waits for the host to send the
/// applied delta back to the replica (so that the submitter reads its own
/// write); after that it returns anyway.
const REMOTE_ECHO_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Forwards submissions to wavelets hosted by other servers: signs nothing
/// itself (the delta arrives signed) and resolves with the host's answer.
/// Implemented by the federation crate.
pub trait RemoteHost: Send + Sync {
    /// Sends the delta to the host. `request_id` names the submission for
    /// the host's retry detection: the same id for every attempt of one
    /// pending submission. [`SubmitError::Unreachable`] means the host was
    /// not reached (the delta may be sent again).
    fn submit(
        &self,
        wavelet_name: WaveletName,
        signed_delta: ProtocolSignedDelta,
        request_id: Option<String>,
    ) -> BoxFuture<'static, Result<SubmitResult, SubmitError>>;
}

/// Answers of pending submissions, by `pending_submits` seq.
type PendingAnswers =
    std::sync::Mutex<std::collections::HashMap<i64, tokio::sync::watch::Sender<PendingAnswer>>>;

/// What a pending submission came to; `None` while it waits.
type PendingAnswer = Option<Result<SubmitResult, String>>;

struct Inner {
    options: WaveServerOptions,
    db: Database,
    store: DeltaStore,
    bus: Arc<WaveBus>,
    wave_map: WaveMap,
    remote: OnceLock<Arc<dyn RemoteHost>>,
    epochs: Arc<EpochBook>,
    clock: Arc<dyn Clock>,
    person: std::sync::RwLock<Option<PersonHosting>>,
    pending: crate::storage::PendingStore,
    pending_answers: PendingAnswers,
    /// One drain of the pending submissions at a time.
    draining: tokio::sync::Mutex<()>,
}

/// What [`WaveServer::adopt_epoch`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EpochAdoption {
    /// The epoch was known already.
    Known,
    /// A newer epoch, or another statement of the same one, is known: the
    /// newest one.
    Stale(EpochRecord),
    /// Recorded; nothing to rewind.
    Recorded,
    /// Recorded, and the wavelet rewound to `fork`; `orphans` deltas were
    /// taken out and kept for resubmission.
    Rewound { fork: HashedVersion, orphans: usize },
}

/// A submission's outcome inside the server.
enum Submitted {
    Local(Arc<LocalWaveletContainer>, Arc<WaveletDeltaRecord>),
    Remote(SubmitResult),
}

impl Submitted {
    fn result(&self) -> SubmitResult {
        match self {
            Submitted::Local(_, record) => SubmitResult::of(record),
            Submitted::Remote(result) => result.clone(),
        }
    }
}

/// The wave server (`WaveServerImpl`); cheap to clone.
#[derive(Clone)]
pub struct WaveServer {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for WaveServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaveServer")
            .field("domain", &self.inner.options.domain)
            .finish()
    }
}

impl WaveServer {
    /// Creates a wave server on an open database, using the system clock.
    /// Wavelets are loaded lazily.
    pub fn new(db: Database, options: WaveServerOptions) -> Self {
        Self::with_clock(db, options, Arc::new(SystemClock))
    }

    /// [`WaveServer::new`] with an injected clock, which supplies delta
    /// timestamps and the time for cache expiry.
    pub fn with_clock(db: Database, options: WaveServerOptions, clock: Arc<dyn Clock>) -> Self {
        let bus = Arc::new(WaveBus::new());
        let store = db.delta_store();
        let db_pending = db.pending_store();
        let epochs = Arc::new(EpochBook::default());
        let expire_millis =
            i64::try_from(options.wave_cache_expire.as_millis()).unwrap_or(i64::MAX);
        let wave_map = WaveMap::new(
            store.clone(),
            Arc::clone(&bus),
            Arc::clone(&clock),
            options.domain.clone(),
            options.wavelet,
            options.wave_cache_size,
            expire_millis,
            Arc::clone(&epochs),
        );
        WaveServer {
            inner: Arc::new(Inner {
                options,
                db,
                store,
                bus,
                wave_map,
                remote: OnceLock::new(),
                epochs,
                clock,
                person: std::sync::RwLock::new(None),
                pending: db_pending,
                pending_answers: std::sync::Mutex::new(std::collections::HashMap::new()),
                draining: tokio::sync::Mutex::new(()),
            }),
        }
    }

    /// Installs what forwards submissions to remote hosts (federation).
    /// Returns false if one was installed already.
    pub fn set_remote_host(&self, remote: Arc<dyn RemoteHost>) -> bool {
        self.inner.remote.set(remote).is_ok()
    }

    /// Puts this server in person mode (or changes the holder of the
    /// person's wavelets): see [`PersonHosting`]. Set it before clients
    /// can submit. The host lease of a later phase calls it again when
    /// the holder changes.
    pub fn set_person_hosting(&self, hosting: Option<PersonHosting>) {
        *self
            .inner
            .person
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = hosting;
    }

    /// The person hosting set with [`WaveServer::set_person_hosting`].
    pub fn person_hosting(&self) -> Option<PersonHosting> {
        self.inner
            .person
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn options(&self) -> &WaveServerOptions {
        &self.inner.options
    }

    /// The wave domain.
    pub fn domain(&self) -> &str {
        &self.inner.options.domain
    }

    /// The bus on which wavelet updates and commits are announced.
    pub fn bus(&self) -> &Arc<WaveBus> {
        &self.inner.bus
    }

    pub fn database(&self) -> &Database {
        &self.inner.db
    }

    /// Whether this server hosts the wavelet, i.e. the wavelet id's domain
    /// is the wave domain.
    pub fn is_local_wavelet(&self, wavelet_name: &WaveletName) -> bool {
        matches!(self.host_of(wavelet_name), WaveletHost::Local)
    }

    /// Where a wavelet is hosted: here, by another server, or (person
    /// mode) by another device of this server's person.
    pub fn wavelet_host(&self, wavelet_name: &WaveletName) -> WaveletHost {
        self.host_of(wavelet_name)
    }

    /// Person mode: the key of the device of this server's person that
    /// hosts the wavelet, when that is another device
    /// ([`WaveletHost::Delegated`]).
    pub fn host_key(&self, wavelet_name: &WaveletName) -> Option<Vec<u8>> {
        match self.host_of(wavelet_name) {
            WaveletHost::Delegated { holder, .. } => Some(holder),
            _ => None,
        }
    }

    /// The domain hosting a wavelet: that of the holder of its newest
    /// host epoch, or the wavelet's own domain.
    pub fn host_domain(&self, wavelet_name: &WaveletName) -> String {
        match self.inner.epochs.latest(wavelet_name) {
            Some(epoch) => epoch.holder_domain,
            None => wavelet_name.wavelet_id.domain().to_owned(),
        }
    }

    /// Reads the host epochs from storage. Called once at start, before
    /// federation runs; a server that never federated has none.
    pub async fn load_epochs(&self) -> Result<(), StorageError> {
        let mut all: std::collections::HashMap<WaveletName, Vec<EpochRecord>> =
            std::collections::HashMap::new();
        for stored in self.inner.store.epochs().await? {
            if let Some((name, record)) = EpochRecord::from_stored(stored) {
                all.entry(name).or_default().push(record);
            }
        }
        self.inner.epochs.replace_all(all);
        Ok(())
    }

    /// The host epochs of a wavelet after epoch 0, oldest first.
    pub fn epochs(&self, wavelet_name: &WaveletName) -> Vec<EpochRecord> {
        self.inner.epochs.epochs(wavelet_name)
    }

    /// The host epoch active at `version` of a wavelet.
    pub fn epoch_at(&self, wavelet_name: &WaveletName, version: u64) -> u64 {
        self.inner.epochs.active_at(wavelet_name, version)
    }

    /// Moves a wavelet into a newer host epoch, whose statement the caller
    /// has verified. If this server's history goes beyond the epoch's
    /// start, it is rewound there: the deltas taken out are kept as
    /// orphans (`orphan_deltas`) for resubmission to the new holder, the
    /// container is reloaded, the search index follows, and subscribers
    /// hear of it through [`WaveBusSubscriber::wavelet_reset`]. Fails if
    /// the history differs at the epoch's start (a fork of a fork; not
    /// handled).
    pub async fn adopt_epoch(
        &self,
        wavelet_name: &WaveletName,
        record: EpochRecord,
    ) -> Result<EpochAdoption, SubmitError> {
        if record.epoch == 0 {
            return Err(SubmitError::Internal("epoch 0 is implicit".to_owned()));
        }
        let hosted_by = (record.holder_domain != self.inner.options.domain)
            .then(|| record.holder_domain.clone());
        let epoch = record.epoch;
        let holder = record.holder_domain.clone();
        let wavelet = self.inner.wave_map.get_or_create(wavelet_name).await?;
        let change = wavelet
            .change_epoch(
                record,
                hosted_by,
                &self.inner.store,
                self.inner.clock.now_millis(),
            )
            .await?;
        Ok(match change {
            EpochChange::Known => EpochAdoption::Known,
            EpochChange::Stale(latest) => EpochAdoption::Stale(latest),
            EpochChange::Recorded => {
                tracing::info!(wavelet = %wavelet_name, epoch, %holder, "host epoch adopted");
                EpochAdoption::Recorded
            }
            EpochChange::Rewound { fork, rewound } => {
                drop(wavelet);
                self.inner.wave_map.invalidate(wavelet_name);
                self.inner.bus.publish_reset(wavelet_name, &fork, &rewound);
                tracing::info!(
                    wavelet = %wavelet_name,
                    epoch,
                    %holder,
                    fork = fork.version(),
                    "host epoch adopted after a rewind"
                );
                EpochAdoption::Rewound {
                    fork,
                    orphans: rewound.len(),
                }
            }
        })
    }

    /// Applies orphaned deltas resubmitted by `domain` to a wavelet hosted
    /// here; see [`LocalWaveletContainer::resubmit_orphans`]. The caller
    /// has checked their signatures.
    pub async fn resubmit_orphans(
        &self,
        wavelet_name: &WaveletName,
        fork: &HashedVersion,
        chain: Vec<AppliedDelta>,
    ) -> Result<ResubmitOutcome, SubmitError> {
        if let Some(domain) = self.host_of(wavelet_name).domain() {
            return Err(SubmitError::NotLocal {
                wavelet: wavelet_name.clone(),
                domain: domain.to_owned(),
            });
        }
        let wavelet = self
            .inner
            .wave_map
            .get(wavelet_name)
            .await?
            .ok_or(SubmitError::UnknownTargetVersion(fork.version()))?;
        let shared = wavelet.shared_domain_participant().cloned();
        wavelet
            .resubmit_orphans(fork, chain, |data, author| {
                data.check_access_permission(author, shared.as_ref())
            })
            .await
    }

    /// Rewinds a wavelet hosted here to `version` under a new epoch held
    /// by this server, then applies the deltas taken out again, rebased
    /// as orphans: the history keeps its content but every subscriber
    /// goes through a rewind. Exercises on one server what a change of
    /// host does across several; the epoch statement is left empty, so
    /// this is for servers that do not federate (end-to-end tests).
    pub async fn rewind_and_replay(
        &self,
        wavelet_name: &WaveletName,
        version: u64,
    ) -> Result<ResubmitOutcome, SubmitError> {
        let internal = |e: &dyn std::fmt::Display| SubmitError::Internal(e.to_string());
        if !matches!(self.host_of(wavelet_name), WaveletHost::Local) {
            return Err(SubmitError::Internal("not hosted here".to_owned()));
        }
        let start = self
            .hashed_version_at(wavelet_name, version)
            .await
            .map_err(|e| internal(&e))?
            .ok_or(SubmitError::UnknownTargetVersion(version))?;
        let epoch = self
            .epochs(wavelet_name)
            .last()
            .map_or(1, |latest| latest.epoch + 1);
        let record = EpochRecord {
            epoch,
            start: start.clone(),
            holder: Vec::new(),
            holder_domain: self.inner.options.domain.clone(),
            statement: Vec::new(),
        };
        self.adopt_epoch(wavelet_name, record).await?;
        let uri = resonator_model::id::IdUriCodec::wavelet_name_to_uri(wavelet_name);
        let orphans: Vec<_> = self
            .inner
            .store
            .pending_orphans()
            .await
            .map_err(|e| internal(&e))?
            .into_iter()
            .filter(|orphan| orphan.wavelet == uri && orphan.fork == start)
            .collect();
        let mut chain = Vec::with_capacity(orphans.len());
        for orphan in &orphans {
            chain.push(AppliedDelta::parse_from(orphan.applied.clone()).map_err(|e| internal(&e))?);
        }
        let outcome = self.resubmit_orphans(wavelet_name, &start, chain).await?;
        self.inner
            .store
            .orphans_done(
                &uri,
                orphans.into_iter().map(|orphan| orphan.sha256).collect(),
            )
            .await
            .map_err(|e| internal(&e))?;
        Ok(outcome)
    }

    fn host_of(&self, wavelet_name: &WaveletName) -> WaveletHost {
        let own = &self.inner.options.domain;
        let person = self.person_hosting();
        if let Some(epoch) = self.inner.epochs.latest(wavelet_name) {
            if epoch.holder_domain != *own {
                return WaveletHost::Remote {
                    domain: epoch.holder_domain,
                };
            }
            // Person mode: the epoch may be held by another device of the
            // person (same domain, other key).
            return match person {
                Some(person) if !epoch.holder.is_empty() && epoch.holder != person.device => {
                    WaveletHost::Delegated {
                        domain: own.clone(),
                        holder: epoch.holder,
                    }
                }
                _ => WaveletHost::Local,
            };
        }
        let domain = wavelet_name.wavelet_id.domain();
        if domain != own {
            return WaveletHost::Remote {
                domain: domain.to_owned(),
            };
        }
        match person.and_then(|person| person.holder.filter(|holder| *holder != person.device)) {
            Some(holder) => WaveletHost::Delegated {
                domain: own.clone(),
                holder,
            },
            None => WaveletHost::Local,
        }
    }

    /// The version of a wavelet before its first delta, which the first
    /// delta must target.
    pub fn version_zero(&self, wavelet_name: &WaveletName) -> HashedVersion {
        HashedVersionFactory::create_version_zero(wavelet_name)
    }

    /// The container of an existing wavelet: a local one, or the replica
    /// of a remote one.
    async fn wavelet(
        &self,
        wavelet_name: &WaveletName,
    ) -> Result<Option<Arc<LocalWaveletContainer>>, WaveletStateError> {
        self.inner.wave_map.get(wavelet_name).await
    }

    async fn submit(
        &self,
        wavelet_name: &WaveletName,
        delta: ProtocolWaveletDelta,
    ) -> Result<Submitted, SubmitError> {
        let version = delta
            .hashed_version
            .as_ref()
            .and_then(|version| version.version)
            .unwrap_or(0);
        if delta.operation.is_empty() {
            return Err(SubmitError::EmptyDelta(u64::try_from(version).unwrap_or(0)));
        }
        let remote = match self.host_of(wavelet_name) {
            WaveletHost::Local => None,
            WaveletHost::Remote { domain } | WaveletHost::Delegated { domain, .. } => {
                match self.inner.remote.get() {
                    Some(remote) => Some(Arc::clone(remote)),
                    None => {
                        return Err(SubmitError::NotLocal {
                            wavelet: wavelet_name.clone(),
                            domain,
                        });
                    }
                }
            }
        };
        let author_address = delta.author.clone().unwrap_or_default();
        tracing::debug!(
            wavelet = %wavelet_name,
            author = %author_address,
            version,
            ops = delta.operation.len(),
            "submit"
        );
        // The envelope around the serialised delta: unsigned unless a
        // signer is configured. This is the one place local deltas get
        // their signatures.
        let serialized =
            ByteStringMessage::serialize_message(delta).map_err(SubmitError::InvalidDelta)?;
        let signature = match &self.inner.options.delta_signer {
            Some(signer) => signer.0.sign(&serialized),
            None => Vec::new(),
        };
        let signed_delta = ProtocolSignedDelta {
            delta: Some(serialized.into_bytes()),
            signature,
        };
        let author = ParticipantId::of(&author_address).map_err(SubmitError::InvalidAuthor)?;
        if let Some(remote) = remote {
            return self
                .submit_remote(remote, wavelet_name, &author, signed_delta, version)
                .await;
        }
        let wavelet = self.inner.wave_map.get_or_create(wavelet_name).await?;
        let record = wavelet.submit_request_as(&author, signed_delta).await?;
        tracing::debug!(
            wavelet = %wavelet_name,
            author = %record.author(),
            ops = record.transformed.len(),
            applied_at = record.applied_at_version.version(),
            timestamp = record.application_timestamp(),
            "submit result"
        );
        Ok(Submitted::Local(wavelet, record))
    }

    /// The remote part of [`WaveServer::submit`]: sends the signed delta
    /// to the host, or queues it if the host cannot be reached (or older
    /// submissions to the wavelet still wait, which keeps them in order).
    async fn submit_remote(
        &self,
        remote: Arc<dyn RemoteHost>,
        wavelet_name: &WaveletName,
        author: &ParticipantId,
        signed_delta: ProtocolSignedDelta,
        version: i64,
    ) -> Result<Submitted, SubmitError> {
        let internal = |e: &dyn std::fmt::Display| SubmitError::Internal(e.to_string());
        let uri = resonator_model::id::IdUriCodec::wavelet_name_to_uri(wavelet_name);
        let request_id = new_pending_id();
        let queued = self
            .inner
            .pending
            .has_wavelet(&uri)
            .await
            .map_err(|e| internal(&e))?;
        let unreachable = if queued {
            None
        } else {
            match remote
                .submit(
                    wavelet_name.clone(),
                    signed_delta.clone(),
                    Some(request_id.clone()),
                )
                .await
            {
                Ok(result) => {
                    tracing::debug!(
                        wavelet = %wavelet_name,
                        author = %author,
                        ops = result.operations_applied,
                        version = result.hashed_version_after_application.version(),
                        "remote submit result"
                    );
                    // The host transforms and applies it; the transformed
                    // delta comes back as an update of the replica.
                    // Callers read their own writes after a local
                    // submission, so this one returns once the replica
                    // has the delta too (or after a while).
                    let target = result.hashed_version_after_application.version();
                    if let Ok(replica) = self.inner.wave_map.get_or_create(wavelet_name).await {
                        let echoed =
                            tokio::time::timeout(REMOTE_ECHO_WAIT, replica.wait_committed(target))
                                .await;
                        if !matches!(echoed, Ok(Ok(_))) {
                            tracing::info!(
                                wavelet = %wavelet_name,
                                version = target,
                                "the host has not sent the submitted delta back yet"
                            );
                        }
                    }
                    return Ok(Submitted::Remote(result));
                }
                Err(SubmitError::Unreachable { domain, message }) => Some((domain, message)),
                Err(error) => return Err(error),
            }
        };
        let bytes = resonator_model::proto::encode(&signed_delta)
            .map_err(|e| SubmitError::Internal(e.to_string()))?;
        let target = u64::try_from(version).unwrap_or(0);
        let now = self.inner.clock.now_millis();
        let seq = {
            // The answer slot exists before the row, so a drain racing
            // with this cannot answer into nothing.
            let seq = self
                .inner
                .pending
                .push(
                    uri,
                    author.address().to_owned(),
                    request_id,
                    bytes,
                    target,
                    now,
                )
                .await
                .map_err(|e| internal(&e))?;
            let (answer, _) = tokio::sync::watch::channel(None);
            lock_answers(&self.inner.pending_answers).insert(seq, answer);
            seq
        };
        match unreachable {
            Some((domain, message)) => tracing::info!(
                wavelet = %wavelet_name,
                %domain,
                %message,
                seq,
                "host unreachable; the submission waits for it"
            ),
            None => {
                tracing::debug!(wavelet = %wavelet_name, seq, "queued behind pending submissions")
            }
        }
        let version = self.inner.wave_map.get(wavelet_name).await.ok().flatten();
        let at = match version {
            Some(wavelet) => wavelet.hashed_version_at(target).await.ok().flatten(),
            None => None,
        }
        .unwrap_or_else(|| self.version_zero(wavelet_name));
        Ok(Submitted::Remote(SubmitResult {
            operations_applied: 0,
            hashed_version_after_application: at,
            application_timestamp: now,
            pending: Some(seq),
        }))
    }

    /// Waits for the answer to a pending submission (see
    /// [`SubmitResult::pending`]): the host's result once the delta was
    /// applied, or why it was refused. Fails at once for a submission not
    /// queued by this run (after a restart, nobody waits for the old
    /// ones; they are still sent).
    pub async fn wait_pending(&self, seq: i64) -> Result<SubmitResult, String> {
        let receiver = lock_answers(&self.inner.pending_answers)
            .get(&seq)
            .map(tokio::sync::watch::Sender::subscribe);
        let Some(mut receiver) = receiver else {
            return Err(format!("no pending submission {seq}"));
        };
        let answer = receiver
            .wait_for(Option::is_some)
            .await
            .map(|answer| answer.clone());
        match answer {
            Ok(Some(answer)) => answer,
            _ => Err("the pending submission was dropped".to_owned()),
        }
    }

    /// How many submissions wait for their hosts.
    pub async fn pending_count(&self) -> usize {
        self.inner
            .pending
            .all()
            .await
            .map(|rows| rows.len())
            .unwrap_or(0)
    }

    /// Sends the submissions waiting for their hosts, oldest first, in
    /// order per wavelet: a wavelet whose host still cannot be reached
    /// keeps its submissions for the next drain. A wavelet now hosted here
    /// (this device took the host role over) applies them itself. Returns
    /// how many were answered. Federation calls this when peers connect
    /// and periodically; one drain runs at a time.
    pub async fn drain_pending(&self) -> usize {
        let _draining = self.inner.draining.lock().await;
        let rows = match self.inner.pending.all().await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "cannot read the pending submissions");
                return 0;
            }
        };
        let mut blocked = BTreeSet::new();
        let mut answered = 0;
        for row in rows {
            if blocked.contains(&row.wavelet) {
                continue;
            }
            match self.send_pending(&row).await {
                Some(answer) => {
                    if let Err(error) = self.inner.pending.remove(row.seq).await {
                        tracing::warn!(%error, "cannot remove a pending submission");
                        blocked.insert(row.wavelet.clone());
                        continue;
                    }
                    match &answer {
                        Ok(result) => tracing::info!(
                            wavelet = %row.wavelet,
                            seq = row.seq,
                            version = result.hashed_version_after_application.version(),
                            "pending submission applied by its host"
                        ),
                        Err(error) => tracing::info!(
                            wavelet = %row.wavelet,
                            seq = row.seq,
                            %error,
                            "pending submission refused by its host"
                        ),
                    }
                    if let Some(slot) = lock_answers(&self.inner.pending_answers).remove(&row.seq) {
                        let _ = slot.send(Some(answer));
                    }
                    answered += 1;
                }
                None => {
                    blocked.insert(row.wavelet.clone());
                }
            }
        }
        answered
    }

    /// One attempt at a pending submission: its answer, or `None` to try
    /// again later.
    async fn send_pending(
        &self,
        row: &crate::storage::PendingSubmit,
    ) -> Option<Result<SubmitResult, String>> {
        let refused = |message: String| Some(Err(message));
        let Ok(wavelet_name) = resonator_model::id::IdUriCodec::uri_to_wavelet_name(&row.wavelet)
        else {
            return refused(format!("bad wavelet {}", row.wavelet));
        };
        let Ok(signed_delta) =
            resonator_model::proto::decode::<ProtocolSignedDelta>(&row.signed_delta)
        else {
            return refused("bad stored delta".to_owned());
        };
        let Ok(author) = ParticipantId::of(&row.author) else {
            return refused(format!("bad author {}", row.author));
        };
        let outcome = match self.host_of(&wavelet_name) {
            WaveletHost::Local => {
                // This device hosts the wavelet now: apply it here.
                match self.inner.wave_map.get_or_create(&wavelet_name).await {
                    Ok(wavelet) => wavelet
                        .submit_request_as(&author, signed_delta)
                        .await
                        .map(|record| SubmitResult::of(&record)),
                    Err(error) => Err(SubmitError::from(error)),
                }
            }
            _ => {
                let remote = self.inner.remote.get().cloned()?;
                remote
                    .submit(
                        wavelet_name.clone(),
                        signed_delta,
                        Some(row.request_id.clone()),
                    )
                    .await
            }
        };
        match outcome {
            Ok(result) => Some(Ok(result)),
            Err(error @ SubmitError::Unreachable { .. }) => {
                tracing::debug!(wavelet = %wavelet_name, %error, "pending submission still waits");
                let _ = self.inner.pending.failed(row.seq, error.to_string()).await;
                None
            }
            Err(error) if error.kind() == SubmitErrorKind::BadRequest => {
                Some(Err(error.to_string()))
            }
            Err(error) => {
                tracing::info!(wavelet = %wavelet_name, %error, "pending submission failed; retrying later");
                let _ = self.inner.pending.failed(row.seq, error.to_string()).await;
                None
            }
        }
    }

    /// Submits a delta signed by a remote server on behalf of its user
    /// `author` to a wavelet hosted here (the host side of federation's
    /// `submit`). The wavelet must exist and `author` must be allowed to
    /// write it; the caller has checked the signatures and that the
    /// remote server speaks for the author's domain.
    pub async fn submit_remote_request(
        &self,
        author: &ParticipantId,
        wavelet_name: &WaveletName,
        signed_delta: ProtocolSignedDelta,
    ) -> Result<SubmitResult, SubmitError> {
        self.submit_remote_request_with_receipt(author, wavelet_name, signed_delta, None)
            .await
            .map(|(result, _)| result)
    }

    /// [`WaveServer::submit_remote_request`] that records `receipt` in the
    /// same transaction as the delta if one is applied; returns whether it
    /// was (not if the delta transformed away or duplicated one already
    /// applied).
    pub async fn submit_remote_request_with_receipt(
        &self,
        author: &ParticipantId,
        wavelet_name: &WaveletName,
        signed_delta: ProtocolSignedDelta,
        receipt: Option<SubmitReceipt>,
    ) -> Result<(SubmitResult, bool), SubmitError> {
        if let Some(domain) = self.host_of(wavelet_name).domain() {
            return Err(SubmitError::NotLocal {
                wavelet: wavelet_name.clone(),
                domain: domain.to_owned(),
            });
        }
        let not_participant = || SubmitError::NotParticipant {
            author: author.address().to_owned(),
            wavelet: wavelet_name.clone(),
        };
        // A delta at version 0 creates the wavelet: only another device of
        // this person sends one (the federation host checks that).
        let creates = signed_delta
            .delta
            .as_deref()
            .and_then(|bytes| resonator_model::proto::decode::<ProtocolWaveletDelta>(bytes).ok())
            .and_then(|delta| delta.hashed_version)
            .and_then(|version| version.version)
            .is_some_and(|version| version == 0);
        let wavelet = if creates {
            self.inner.wave_map.get_or_create(wavelet_name).await?
        } else {
            self.inner
                .wave_map
                .get(wavelet_name)
                .await?
                .ok_or_else(not_participant)?
        };
        let (record, recorded) = wavelet
            .submit_request_with_receipt(author, signed_delta, receipt)
            .await?;
        tracing::debug!(
            wavelet = %wavelet_name,
            author = %author,
            ops = record.transformed.len(),
            "remote submit applied"
        );
        Ok((SubmitResult::of(&record), recorded))
    }

    /// Offers a delta applied by the remote host of a wavelet to its
    /// replica here, creating the replica with its first delta. See
    /// [`LocalWaveletContainer::apply_replica_delta`]; the caller has
    /// checked that the delta comes from the host and its signatures.
    pub async fn apply_replica_delta(
        &self,
        wavelet_name: &WaveletName,
        applied: AppliedDelta,
    ) -> Result<ReplicaOutcome, SubmitError> {
        if let WaveletHost::Local = self.host_of(wavelet_name) {
            return Err(SubmitError::Internal(format!(
                "{wavelet_name} is hosted here, not a replica"
            )));
        }
        let wavelet = self.inner.wave_map.get_or_create(wavelet_name).await?;
        wavelet.apply_replica_delta(applied).await
    }

    /// Person mode: appends to a wavelet hosted here a delta another
    /// device of the person applied as its host earlier (under an earlier
    /// host lease) and this device lacks; it must continue the history
    /// here exactly. See [`LocalWaveletContainer::apply_replica_delta`];
    /// the caller has checked the delta's signatures and where it came
    /// from.
    pub async fn import_delta(
        &self,
        wavelet_name: &WaveletName,
        applied: AppliedDelta,
    ) -> Result<ReplicaOutcome, SubmitError> {
        let wavelet = self.inner.wave_map.get_or_create(wavelet_name).await?;
        wavelet.apply_replica_delta(applied).await
    }

    /// The current version of a wavelet, local or replica; version zero if
    /// it has no deltas.
    pub async fn current_version(
        &self,
        wavelet_name: &WaveletName,
    ) -> Result<HashedVersion, WaveServerError> {
        match self.wavelet(wavelet_name).await? {
            Some(wavelet) => Ok(wavelet.current_version().await?),
            None => Ok(self.version_zero(wavelet_name)),
        }
    }

    /// The hashed version of a wavelet at `version`, if that is one of its
    /// delta boundaries.
    pub async fn hashed_version_at(
        &self,
        wavelet_name: &WaveletName,
        version: u64,
    ) -> Result<Option<HashedVersion>, WaveServerError> {
        if version == 0 {
            return Ok(Some(self.version_zero(wavelet_name)));
        }
        match self.wavelet(wavelet_name).await? {
            Some(wavelet) => Ok(wavelet.hashed_version_at(version).await?),
            None => Ok(None),
        }
    }

    /// Whether the delta of a wavelet ending at `version` carries a
    /// signature by `signer_id` (`isDeltaSigner`).
    pub async fn is_delta_signer(
        &self,
        wavelet_name: &WaveletName,
        version: &HashedVersion,
        signer_id: &[u8],
    ) -> Result<bool, WaveServerError> {
        match self.wavelet(wavelet_name).await? {
            Some(wavelet) => Ok(wavelet.is_delta_signer(version, signer_id).await?),
            None => Ok(false),
        }
    }

    /// Submits a delta on behalf of an authenticated participant: the
    /// delta's author must be `participant`
    /// (`ClientFrontendImpl.submitRequest`). This is the call for front
    /// ends, robots and agents alike.
    pub async fn submit_request_as(
        &self,
        participant: &ParticipantId,
        wavelet_name: &WaveletName,
        delta: ProtocolWaveletDelta,
    ) -> Result<SubmitResult, SubmitError> {
        let author = delta.author.as_deref().unwrap_or_default();
        if ParticipantId::new_unchecked(author) != *participant {
            return Err(SubmitError::AuthorMismatch);
        }
        self.submit_request(wavelet_name, delta).await
    }

    /// Submits a delta given as a model value; its author is the
    /// submitting participant.
    pub async fn submit_delta(
        &self,
        wavelet_name: &WaveletName,
        delta: &WaveletDelta,
    ) -> Result<SubmitResult, SubmitError> {
        let delta = serialize_delta(delta).map_err(SubmitError::InvalidDelta)?;
        self.submit_request(wavelet_name, delta).await
    }

    /// Submits a delta and waits until it is durably stored, i.e. until the
    /// commit that the wave bus announces as `wavelet_committed`.
    ///
    /// For a wavelet hosted elsewhere this returns once the host applied
    /// the delta.
    pub async fn submit_request_durably(
        &self,
        wavelet_name: &WaveletName,
        delta: ProtocolWaveletDelta,
    ) -> Result<SubmitResult, SubmitError> {
        let submitted = self.submit(wavelet_name, delta).await?;
        if let Submitted::Local(wavelet, record) = &submitted
            && record.applied.is_some()
        {
            wavelet
                .wait_committed(record.resulting_version().version())
                .await?;
        }
        Ok(submitted.result())
    }

    /// Waits until a wavelet's history up to `version` is durably stored;
    /// returns the committed version reached.
    pub async fn wait_committed(
        &self,
        wavelet_name: &WaveletName,
        version: u64,
    ) -> Result<HashedVersion, WaveServerError> {
        let wavelet = self.existing(wavelet_name, "commit wait").await?;
        Ok(wavelet.wait_committed(version).await?)
    }

    async fn existing(
        &self,
        wavelet_name: &WaveletName,
        what: &str,
    ) -> Result<Arc<LocalWaveletContainer>, WaveServerError> {
        self.wavelet(wavelet_name).await?.ok_or_else(|| {
            WaveServerError::AccessControl(format!(
                "Client request for {what} made for non-existent wavelet: {wavelet_name}"
            ))
        })
    }

    /// [`WaveletProvider::get_history`] returning at most `limit` deltas;
    /// the caller continues from the resulting version of the last one.
    pub async fn get_history_limited(
        &self,
        wavelet_name: &WaveletName,
        version_start: &HashedVersion,
        version_end: &HashedVersion,
        limit: usize,
    ) -> Result<Vec<TransformedWaveletDelta>, WaveServerError> {
        self.existing(wavelet_name, "history")
            .await?
            .request_transformed_history(version_start, version_end, Some(limit))
            .await
    }

    /// The applied deltas (the bytes the history hashes cover) between two
    /// delta boundaries, at most `limit` of them if given.
    pub async fn get_applied_history(
        &self,
        wavelet_name: &WaveletName,
        version_start: &HashedVersion,
        version_end: &HashedVersion,
        limit: Option<usize>,
    ) -> Result<Vec<AppliedDelta>, WaveServerError> {
        self.existing(wavelet_name, "history")
            .await?
            .request_history(version_start, version_end, limit)
            .await
    }

    /// The full delta records between two delta boundaries.
    pub async fn get_history_records(
        &self,
        wavelet_name: &WaveletName,
        version_start: &HashedVersion,
        version_end: &HashedVersion,
        limit: Option<usize>,
    ) -> Result<Vec<Arc<WaveletDeltaRecord>>, WaveServerError> {
        self.existing(wavelet_name, "history")
            .await?
            .request_history_records(version_start, version_end, limit)
            .await
    }

    /// Runs `function` on a wavelet's current data without copying it;
    /// `None` if the wavelet does not exist (`ReadableWaveletDataProvider`).
    /// `function` runs under the wavelet's read lock and must be quick.
    pub async fn with_wavelet<T>(
        &self,
        wavelet_name: &WaveletName,
        function: impl FnOnce(&WaveletData) -> T,
    ) -> Result<Option<T>, WaveServerError> {
        let Some(wavelet) = self.wavelet(wavelet_name).await? else {
            return Ok(None);
        };
        Ok(wavelet.apply_function(|data| data.map(function)).await?)
    }

    /// The current and the committed version of a wavelet; `None` if it
    /// does not exist.
    pub async fn get_versions(
        &self,
        wavelet_name: &WaveletName,
    ) -> Result<Option<(HashedVersion, HashedVersion)>, WaveServerError> {
        let Some(wavelet) = self.wavelet(wavelet_name).await? else {
            return Ok(None);
        };
        if wavelet.is_empty().await? {
            return Ok(None);
        }
        Ok(Some((
            wavelet.current_version().await?,
            wavelet.last_committed_version().await?,
        )))
    }

    /// How a wavelet was loaded from storage, loading it if necessary;
    /// `None` if it does not exist.
    pub async fn load_report(
        &self,
        wavelet_name: &WaveletName,
    ) -> Result<Option<LoadReport>, WaveServerError> {
        Ok(self
            .wavelet(wavelet_name)
            .await?
            .map(|wavelet| wavelet.load_report()))
    }

    /// Number of waves currently held in memory.
    pub fn loaded_wave_count(&self) -> usize {
        self.inner.wave_map.loaded_wave_count()
    }

    /// Drops the waves that were not used for the configured idle time and
    /// have nothing in flight. Returns how many were dropped.
    pub fn evict_idle_waves(&self) -> usize {
        self.inner.wave_map.evict_idle()
    }

    /// Drops every wave that is not in use (`unloadAllWavelets`). Returns
    /// how many were dropped.
    pub fn unload_all_waves(&self) -> usize {
        self.inner.wave_map.unload_all()
    }

    /// Starts a task that evicts idle waves every `interval`. It ends when
    /// the last handle to the server is dropped.
    pub fn spawn_maintenance(&self, interval: Duration) -> tokio::task::JoinHandle<()> {
        let inner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let Some(inner) = inner.upgrade() else {
                    break;
                };
                let evicted = inner.wave_map.evict_idle();
                if evicted > 0 {
                    tracing::debug!(evicted, "evicted idle waves");
                }
            }
        })
    }

    /// Waits until every delta applied so far is durably stored.
    pub async fn flush(&self) -> Result<(), WaveServerError> {
        Ok(self.inner.db.flush().await?)
    }
}

impl WaveletProvider for WaveServer {
    async fn submit_request(
        &self,
        wavelet_name: &WaveletName,
        delta: ProtocolWaveletDelta,
    ) -> Result<SubmitResult, SubmitError> {
        Ok(self.submit(wavelet_name, delta).await?.result())
    }

    async fn get_history(
        &self,
        wavelet_name: &WaveletName,
        version_start: &HashedVersion,
        version_end: &HashedVersion,
    ) -> Result<Vec<TransformedWaveletDelta>, WaveServerError> {
        self.existing(wavelet_name, "history")
            .await?
            .request_transformed_history(version_start, version_end, None)
            .await
    }

    async fn check_access_permission(
        &self,
        wavelet_name: &WaveletName,
        participant: Option<&ParticipantId>,
    ) -> Result<bool, WaveServerError> {
        match self.wavelet(wavelet_name).await? {
            Some(wavelet) => Ok(wavelet.check_access_permission(participant).await?),
            None => Ok(false),
        }
    }

    async fn get_wave_ids(&self) -> Result<Vec<WaveId>, WaveServerError> {
        // Stored waves plus those whose first delta is not committed yet.
        let mut ids: BTreeSet<WaveId> = self.inner.store.wave_ids().await?.into_iter().collect();
        ids.extend(self.inner.wave_map.loaded_wave_ids());
        Ok(ids.into_iter().collect())
    }

    async fn get_wavelet_ids(
        &self,
        wave_id: &WaveId,
    ) -> Result<BTreeSet<WaveletId>, WaveServerError> {
        let mut ids = self.inner.store.lookup(wave_id).await?;
        ids.extend(self.inner.wave_map.loaded_wavelet_ids(wave_id));
        Ok(ids)
    }

    async fn get_snapshot(
        &self,
        wavelet_name: &WaveletName,
    ) -> Result<Option<CommittedWaveletSnapshot>, WaveServerError> {
        match self.wavelet(wavelet_name).await? {
            Some(wavelet) => Ok(wavelet.get_snapshot().await?),
            None => Ok(None),
        }
    }
}

fn lock_answers(
    answers: &PendingAnswers,
) -> std::sync::MutexGuard<
    '_,
    std::collections::HashMap<i64, tokio::sync::watch::Sender<PendingAnswer>>,
> {
    answers
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A fresh id for a pending submission's requests.
fn new_pending_id() -> String {
    let mut random = [0u8; 16];
    // Without randomness the clock still makes ids unique enough.
    let _ = getrandom::fill(&mut random);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let hex: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("submit-{millis:x}-{hex}")
}
