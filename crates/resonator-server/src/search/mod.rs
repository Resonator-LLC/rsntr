//! Search and the inbox: which waves a participant can see that match a
//! query, as digests.
//!
//! Derived from `SearchProvider`, `AbstractSearchProviderImpl`,
//! `SimpleSearchProviderImpl`, `QueryHelper`, `PerUserWaveViewHandler` and
//! `WaveDigester` in `org.waveprotocol.box.server.waveserver` of Apache
//! Wave, licensed under the Apache License, Version 2.0.
//!
//! [`SearchProvider`] is the interface, [`SearchService`] the
//! implementation. It has three parts:
//!
//! * The index ([`index`]), in SQLite: per wavelet its participants, what
//!   its wave is sorted by, its owner's archive and follow state if it is
//!   a user-data wavelet, and its text. It answers "which waves, in which
//!   order" without loading a wave.
//! * The indexer, a task fed by the wave bus. The bus callback only puts
//!   the wavelet's name on a queue. The task reads the wavelets named
//!   (briefly, under the wavelet's read lock) and writes the index in one
//!   transaction per batch through the database's single writer; the text
//!   of a changed wavelet is written at most once per debounce interval.
//!   At start-up a second task compares the index with the stored
//!   wavelets and indexes whatever is missing or behind, in the
//!   background.
//! * The search itself: the index gives the ordered list of candidate
//!   waves; then, for the page asked for only, every wavelet is checked
//!   against the live wavelet (the index is never trusted for access) and
//!   the digest is built, or taken from a cache keyed by the versions of
//!   the wavelets it was built from.
//!
//! A search first waits (briefly, and not for ever) until the index has
//! caught up with the deltas applied before the search began, so that a
//! client that submits a delta and then searches sees its effect. The
//! text index lags by up to the debounce interval.
//!
//! # Differences from the original
//!
//! * `in:inbox`, `in:archive` and `in:mute` look at the user's supplement;
//!   the original treats every `in:` alike, as "waves I am a participant
//!   of". Any other folder name still means just that.
//! * Free-text terms are searched in the full-text index, as the
//!   original's Solr provider did; its default in-memory provider rejects
//!   them as an invalid query.
//! * `id:` filters by wave id; the original accepts the token and ignores
//!   it.
//! * The user's own user-data wavelet is always used for the unread
//!   counts. The original drops it when a `creator:` or `with:` filter
//!   does not match it, and then reports every blip as unread.

pub mod index;
pub mod query;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use futures_util::StreamExt;
use resonator_model::data::WaveletData;
use resonator_model::digest::{Digest, build_digest_with_supplement};
use resonator_model::id::util::{
    build_user_data_wavelet_id, is_conversational_id, is_user_data_wavelet,
};
use resonator_model::id::{
    IdSerialiser, LegacyIdSerialiser, ModernIdSerialiser, ParticipantId, WaveId, WaveletId,
    WaveletName,
};
use resonator_model::version::HashedVersion;
use thiserror::Error;
use tokio::sync::{Notify, mpsc, oneshot, watch};

use self::index::{Folder, IndexQuery, IndexStore, KIND_CONVERSATIONAL, WaveletEntry};
use self::query::{TokenQueryType, build_validated_participant_ids, compare, parse_query};
use crate::config::Config;
use crate::frontend::{DefaultPolicy, WaveletPolicy};
use crate::storage::{StorageError, WaveletDeltaRecord};
use crate::waveserver::{
    SubscriptionId, WaveBusSubscriber, WaveServer, WaveServerError, WaveletProvider,
};

/// How long a search waits for the index to catch up with the deltas
/// applied before it began.
const INDEX_WAIT: Duration = Duration::from_secs(2);

/// Wavelets indexed in one transaction.
const INDEX_BATCH: usize = 64;

/// Waves of a result page that are checked and digested at a time.
const DIGEST_CONCURRENCY: usize = 8;

/// The result of a search (`com.google.wave.api.SearchResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    /// The query as given.
    pub query: String,
    /// The digests of the waves found, from `start_at` on.
    pub digests: Vec<Digest>,
    /// How many waves the index has for the query in all. Not exact while
    /// the index is catching up.
    pub total_results: usize,
}

/// Failure of a search. An invalid query is not a failure: it finds
/// nothing, as in the original.
#[derive(Debug, Error)]
pub enum SearchError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    WaveServer(#[from] WaveServerError),
}

/// Finds waves for a participant (`SearchProvider`). The interface for
/// everything that searches in process, such as robots and agents.
pub trait SearchProvider: Send + Sync {
    /// Runs `query` for `user` and returns the digests of `num_results`
    /// waves from position `start_at` of the result. `user` is trusted to
    /// be who is asking; only waves they may read are returned.
    fn search(
        &self,
        user: &ParticipantId,
        query: &str,
        start_at: usize,
        num_results: usize,
    ) -> impl Future<Output = Result<SearchResult, SearchError>> + Send;
}

/// Settings of a [`SearchService`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchSettings {
    /// Whether text is indexed and free-text terms are searched.
    pub fulltext_enabled: bool,
    /// How long a changed wavelet waits before its text is indexed.
    pub debounce: Duration,
    /// Digests kept in memory.
    pub digest_cache_size: usize,
}

impl Default for SearchSettings {
    fn default() -> Self {
        Self::from_config(&Config::default())
    }
}

impl SearchSettings {
    pub fn from_config(config: &Config) -> Self {
        SearchSettings {
            fulltext_enabled: config.search_fulltext_enabled,
            debounce: Duration::from_millis(config.search_index_debounce_ms.max(1)),
            digest_cache_size: config.search_digest_cache_size,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

enum Event {
    /// A wavelet changed; `seq` numbers the changes.
    Update { name: WaveletName, seq: u64 },
    /// Index everything queued so far, text included, then answer.
    Flush(oneshot::Sender<()>),
}

/// The queue to the indexer. Numbering and sending happen under one lock
/// so that the numbers arrive in order.
struct Feed {
    seq: u64,
    sender: mpsc::UnboundedSender<Event>,
}

const FTS_PENDING: u8 = 0;
const FTS_ON: u8 = 1;
const FTS_OFF: u8 = 2;

struct CachedDigest {
    /// The wavelets the digest was built from, with their versions.
    versions: Vec<(WaveletId, u64)>,
    digest: Digest,
    used: u64,
}

/// Digests by user and wave, least recently used out first.
struct DigestCache {
    capacity: usize,
    tick: u64,
    entries: HashMap<(String, WaveId), CachedDigest>,
}

impl DigestCache {
    fn get(&mut self, user: &str, wave: &WaveId, versions: &[(WaveletId, u64)]) -> Option<Digest> {
        self.tick += 1;
        let entry = self.entries.get_mut(&(user.to_owned(), wave.clone()))?;
        if entry.versions != versions {
            return None;
        }
        entry.used = self.tick;
        Some(entry.digest.clone())
    }

    fn put(&mut self, user: &str, wave: &WaveId, versions: Vec<(WaveletId, u64)>, digest: Digest) {
        if self.capacity == 0 {
            return;
        }
        self.tick += 1;
        self.entries.insert(
            (user.to_owned(), wave.clone()),
            CachedDigest {
                versions,
                digest,
                used: self.tick,
            },
        );
        if self.entries.len() > self.capacity {
            // Drop the least recently used tenth in one go.
            let mut used: Vec<u64> = self.entries.values().map(|entry| entry.used).collect();
            used.sort_unstable();
            let cut = used[(self.capacity / 10).min(used.len() - 1)];
            self.entries.retain(|_, entry| entry.used > cut);
        }
    }
}

/// Counters that change when a user's search results may have.
struct Generations {
    /// Distinguishes the counters of this process from those of an
    /// earlier one.
    boot: u32,
    /// Changes to waves shared with the whole domain.
    shared: AtomicU64,
    users: Mutex<HashMap<String, u64>>,
    changed: Notify,
}

struct Inner {
    server: WaveServer,
    policy: Arc<dyn WaveletPolicy>,
    store: IndexStore,
    settings: SearchSettings,
    /// The shared domain participant of the wave domain.
    shared: Option<ParticipantId>,
    feed: Mutex<Feed>,
    /// The number of the last change the index reflects.
    applied: watch::Sender<u64>,
    /// Whether the catch-up at start-up is done.
    caught_up: watch::Sender<bool>,
    fts: AtomicU8,
    cache: Mutex<DigestCache>,
    generations: Generations,
    bus_subscription: Mutex<Option<SubscriptionId>>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

struct BusAdapter(Weak<Inner>);

impl WaveBusSubscriber for BusAdapter {
    fn wavelet_update(&self, wavelet: &WaveletData, _deltas: &[Arc<WaveletDeltaRecord>]) {
        // Called under the wavelet's lock: just queue the name.
        if let Some(inner) = self.0.upgrade() {
            let mut feed = lock(&inner.feed);
            feed.seq += 1;
            let seq = feed.seq;
            let _ = feed.sender.send(Event::Update {
                name: wavelet.wavelet_name(),
                seq,
            });
        }
    }

    fn wavelet_committed(&self, _wavelet_name: &WaveletName, _version: &HashedVersion) {}

    fn wavelet_reset(
        &self,
        wavelet_name: &WaveletName,
        _version: &HashedVersion,
        _rewound: &[Arc<WaveletDeltaRecord>],
    ) {
        // The rewind lowered the index row's version, so the reindex is
        // not refused as older.
        if let Some(inner) = self.0.upgrade() {
            let mut feed = lock(&inner.feed);
            feed.seq += 1;
            let seq = feed.seq;
            let _ = feed.sender.send(Event::Update {
                name: wavelet_name.clone(),
                seq,
            });
        }
    }
}

/// What a wavelet must be like to be part of a search result
/// (`SimpleSearchProviderImpl.isWaveletMatchesCriteria`).
struct Criteria {
    user: ParticipantId,
    shared: Option<ParticipantId>,
    /// A query without `in:` also finds waves shared with the domain.
    is_all: bool,
    with: Vec<ParticipantId>,
    creators: Vec<ParticipantId>,
}

impl Criteria {
    fn is_own_user_data(&self, wavelet: &WaveletData) -> bool {
        is_user_data_wavelet(wavelet.wavelet_id()) && *wavelet.creator() == self.user
    }

    fn matches(&self, wavelet: &WaveletData) -> bool {
        if self
            .creators
            .iter()
            .any(|creator| creator != wavelet.creator())
        {
            return false;
        }
        let participants = wavelet.participants();
        let accessible = if self.is_own_user_data(wavelet) {
            true
        } else if self.is_all {
            wavelet.check_access_permission(&self.user, self.shared.as_ref())
        } else {
            participants.contains(&self.user)
        };
        accessible && self.with.iter().all(|other| participants.contains(other))
    }
}

/// The search service; cheap to clone.
#[derive(Clone)]
pub struct SearchService {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for SearchService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchService")
            .field("settings", &self.inner.settings)
            .finish()
    }
}

impl SearchService {
    /// Creates the service with the default policy; see
    /// [`SearchService::with_policy`].
    pub fn new(server: WaveServer, settings: SearchSettings) -> Self {
        Self::with_policy(server, Arc::new(DefaultPolicy), settings)
    }

    /// Creates the service on `server`'s database, subscribes it to the
    /// wave bus and starts the indexer and the catch-up. Must be called
    /// within a tokio runtime.
    pub fn with_policy(
        server: WaveServer,
        policy: Arc<dyn WaveletPolicy>,
        settings: SearchSettings,
    ) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut boot = [0u8; 4];
        let _ = getrandom::fill(&mut boot);
        let inner = Arc::new(Inner {
            store: IndexStore::new(server.database().clone()),
            shared: ParticipantId::shared_domain_participant(server.domain()).ok(),
            policy,
            feed: Mutex::new(Feed { seq: 0, sender }),
            applied: watch::Sender::new(0),
            caught_up: watch::Sender::new(false),
            fts: AtomicU8::new(if settings.fulltext_enabled {
                FTS_PENDING
            } else {
                FTS_OFF
            }),
            cache: Mutex::new(DigestCache {
                capacity: settings.digest_cache_size,
                tick: 0,
                entries: HashMap::new(),
            }),
            generations: Generations {
                boot: u32::from_le_bytes(boot),
                shared: AtomicU64::new(0),
                users: Mutex::new(HashMap::new()),
                changed: Notify::new(),
            },
            bus_subscription: Mutex::new(None),
            tasks: Mutex::new(Vec::new()),
            settings,
            server,
        });
        let id = inner
            .server
            .bus()
            .subscribe(Arc::new(BusAdapter(Arc::downgrade(&inner))));
        *lock(&inner.bus_subscription) = Some(id);
        let indexer = tokio::spawn(run_indexer(Arc::downgrade(&inner), receiver));
        let catch_up = tokio::spawn(run_catch_up(Arc::downgrade(&inner)));
        lock(&inner.tasks).extend([indexer, catch_up]);
        SearchService { inner }
    }

    /// Detaches from the wave bus and stops the indexer.
    pub fn shutdown(&self) {
        if let Some(id) = lock(&self.inner.bus_subscription).take() {
            self.inner.server.bus().unsubscribe(id);
        }
        for task in lock(&self.inner.tasks).drain(..) {
            task.abort();
        }
        // Nobody waits for an indexer that is gone.
        self.inner.caught_up.send_replace(true);
        self.inner.applied.send_replace(u64::MAX);
        self.inner.generations.changed.notify_waiters();
    }

    /// Whether free-text terms are searched: full text is enabled and the
    /// SQLite build has FTS5.
    pub fn full_text_available(&self) -> bool {
        self.inner.fts.load(Ordering::Relaxed) == FTS_ON
    }

    /// Waits until the catch-up at start-up is done and everything applied
    /// so far is in the index, text included. For tests and tools; a
    /// search does not need it.
    pub async fn flush(&self) {
        let mut caught_up = self.inner.caught_up.subscribe();
        if caught_up.wait_for(|done| *done).await.is_err() {
            return;
        }
        let (reply, done) = oneshot::channel();
        if lock(&self.inner.feed)
            .sender
            .send(Event::Flush(reply))
            .is_err()
        {
            return;
        }
        let _ = done.await;
    }

    /// Number of wavelets in the index.
    pub async fn indexed_wavelet_count(&self) -> Result<u64, SearchError> {
        Ok(self.inner.store.wavelet_count().await?)
    }

    /// A token that changes whenever the results of `user`'s searches may
    /// have changed: a wavelet they are (or were) a participant of, or one
    /// shared with the domain, was indexed at a new version. Opaque; equal
    /// tokens mean nothing changed.
    pub fn generation(&self, user: &ParticipantId) -> String {
        let generations = &self.inner.generations;
        let own = lock(&generations.users)
            .get(user.address())
            .copied()
            .unwrap_or(0);
        format!(
            "{:08x}-{}",
            generations.boot,
            own + generations.shared.load(Ordering::Relaxed)
        )
    }

    /// Waits until `user`'s generation differs from `since`, at most
    /// `timeout`, and returns the generation then.
    pub async fn changes(&self, user: &ParticipantId, since: &str, timeout: Duration) -> String {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.inner.generations.changed.notified();
            tokio::pin!(notified);
            // Register before looking, so that no change is missed.
            notified.as_mut().enable();
            let current = self.generation(user);
            if current != since || lock(&self.inner.bus_subscription).is_none() {
                return current;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.generation(user);
            }
        }
    }
}

impl Inner {
    fn full_text(&self) -> bool {
        self.fts.load(Ordering::Relaxed) == FTS_ON
    }

    fn bump_generations(&self, affected: &BTreeSet<String>) {
        if affected.is_empty() {
            return;
        }
        let shared = self.shared.as_ref().map(ParticipantId::address);
        {
            let mut users = lock(&self.generations.users);
            for address in affected {
                if Some(address.as_str()) == shared {
                    self.generations.shared.fetch_add(1, Ordering::Relaxed);
                } else {
                    *users.entry(address.clone()).or_insert(0) += 1;
                }
            }
        }
        self.generations.changed.notify_waiters();
    }

    /// Reads the wavelets named and writes them to the index. Returns the
    /// names of the conversational ones among them.
    async fn index_wavelets(
        &self,
        names: impl IntoIterator<Item = WaveletName>,
        with_text: bool,
    ) -> Vec<WaveletName> {
        let with_text = with_text && self.full_text();
        let mut entries = Vec::new();
        let mut conversational = Vec::new();
        for name in names {
            let extracted = self
                .server
                .with_wavelet(&name, |wavelet| WaveletEntry::extract(wavelet, with_text))
                .await;
            match extracted {
                Ok(Some(mut entry)) => {
                    entry.normalize_text();
                    if entry.kind == KIND_CONVERSATIONAL {
                        conversational.push(name);
                    }
                    entries.push(entry);
                }
                // Not a wavelet of this server, or gone.
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(wavelet = %name, %error, "cannot read wavelet for the search index");
                }
            }
        }
        match self.store.write(entries, with_text).await {
            Ok(outcome) => self.bump_generations(&outcome.affected),
            Err(error) => tracing::error!(%error, "writing the search index failed"),
        }
        conversational
    }
}

/// The indexer: takes wavelet names off the queue, indexes them in
/// batches, and indexes the text of changed wavelets once per debounce
/// interval.
async fn run_indexer(inner: Weak<Inner>, mut receiver: mpsc::UnboundedReceiver<Event>) {
    /// Should the indexer ever stop while the service lives, searches
    /// must not wait for it.
    struct Stopped(Weak<Inner>);
    impl Drop for Stopped {
        fn drop(&mut self) {
            if let Some(inner) = self.0.upgrade() {
                inner.applied.send_replace(u64::MAX);
            }
        }
    }
    let _stopped = Stopped(inner.clone());
    let debounce = match inner.upgrade() {
        Some(inner) => inner.settings.debounce,
        None => return,
    };
    let mut ticker = tokio::time::interval(debounce);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Wavelets whose text is behind.
    let mut dirty: HashSet<WaveletName> = HashSet::new();
    loop {
        let mut names: Vec<WaveletName> = Vec::new();
        let mut seen: HashSet<WaveletName> = HashSet::new();
        let mut max_seq = 0;
        let mut flushes = Vec::new();
        let mut text_due = false;
        tokio::select! {
            event = receiver.recv() => {
                let Some(mut event) = event else { return };
                loop {
                    match event {
                        Event::Update { name, seq } => {
                            max_seq = max_seq.max(seq);
                            if seen.insert(name.clone()) {
                                names.push(name);
                            }
                        }
                        Event::Flush(reply) => {
                            flushes.push(reply);
                            break;
                        }
                    }
                    if names.len() >= INDEX_BATCH {
                        break;
                    }
                    match receiver.try_recv() {
                        Ok(next) => event = next,
                        Err(_) => break,
                    }
                }
            }
            _ = ticker.tick() => text_due = true,
        }
        let Some(inner) = inner.upgrade() else { return };
        if !names.is_empty() {
            let conversational = inner.index_wavelets(names, false).await;
            if inner.fts.load(Ordering::Relaxed) != FTS_OFF {
                dirty.extend(conversational);
            }
        }
        if max_seq > 0 {
            inner.applied.send_if_modified(|applied| {
                let advanced = max_seq > *applied;
                *applied = (*applied).max(max_seq);
                advanced
            });
        }
        if (text_due || !flushes.is_empty()) && !dirty.is_empty() && inner.full_text() {
            let mut pending: Vec<WaveletName> = dirty.drain().collect();
            while !pending.is_empty() {
                let rest = pending.split_off(pending.len().min(INDEX_BATCH));
                inner.index_wavelets(pending, true).await;
                pending = rest;
            }
        }
        if inner.fts.load(Ordering::Relaxed) == FTS_OFF {
            dirty.clear();
        }
        for reply in flushes {
            let _ = reply.send(());
        }
    }
}

/// The catch-up at start-up: prepares the full-text table, then indexes
/// every stored wavelet whose index row is missing or behind.
async fn run_catch_up(inner: Weak<Inner>) {
    let Some(strong) = inner.upgrade() else {
        return;
    };
    if strong.settings.fulltext_enabled {
        let state = match strong.store.prepare_full_text().await {
            Ok(true) => FTS_ON,
            Ok(false) => {
                tracing::warn!(
                    "this SQLite build has no FTS5: full-text search is disabled; \
                     queries with free-text terms will find nothing"
                );
                FTS_OFF
            }
            Err(error) => {
                tracing::error!(%error, "cannot prepare the full-text index: full-text search is disabled");
                FTS_OFF
            }
        };
        strong.fts.store(state, Ordering::Relaxed);
    }
    let fts = strong.full_text();
    match strong.store.remove_orphans(fts).await {
        Ok(0) => {}
        Ok(removed) => tracing::info!(removed, "removed stale rows from the search index"),
        Err(error) => tracing::error!(%error, "cleaning the search index failed"),
    }
    let stale = match strong.store.stale_wavelets(fts).await {
        Ok(stale) => stale,
        Err(error) => {
            tracing::error!(%error, "cannot compare the search index with the stored wavelets");
            Vec::new()
        }
    };
    drop(strong);
    let total = stale.len();
    if total > 0 {
        tracing::info!(wavelets = total, "search index is behind; catching up");
    }
    let started = std::time::Instant::now();
    let mut stale = stale.into_iter();
    loop {
        let batch: Vec<WaveletName> = stale.by_ref().take(INDEX_BATCH).collect();
        if batch.is_empty() {
            break;
        }
        // Not holding the service alive between batches.
        let Some(strong) = inner.upgrade() else {
            return;
        };
        strong.index_wavelets(batch, true).await;
        drop(strong);
        tokio::task::yield_now().await;
    }
    if total > 0 {
        tracing::info!(
            wavelets = total,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "search index caught up"
        );
    }
    if let Some(strong) = inner.upgrade() {
        strong.caught_up.send_replace(true);
    }
}

/// The wave ids an `id:` filter names: `domain/id`, the legacy
/// `domain!id`, or a bare id of the user's domain.
fn parse_wave_ids(values: &[String], local_domain: &str) -> Option<Vec<WaveId>> {
    values
        .iter()
        .map(|value| {
            if value.contains('/') {
                ModernIdSerialiser.deserialise_wave_id(value).ok()
            } else if value.contains('!') {
                LegacyIdSerialiser.deserialise_wave_id(value).ok()
            } else {
                WaveId::of(local_domain, value).ok()
            }
        })
        .collect()
}

impl SearchService {
    async fn wait_until_indexed(&self) {
        let target = lock(&self.inner.feed).seq;
        let mut applied = self.inner.applied.subscribe();
        let caught_up = applied.wait_for(|applied| *applied >= target);
        if tokio::time::timeout(INDEX_WAIT, caught_up).await.is_err() {
            tracing::warn!("search index is behind; searching anyway");
        }
    }

    /// The digest of a wave for the user, from the wavelets the index has
    /// them on plus their user-data wavelet; `None` if no conversational
    /// wavelet of the wave matches the criteria on a live look.
    async fn digest(
        &self,
        criteria: &Criteria,
        wave_id: &WaveId,
        indexed: Vec<WaveletId>,
    ) -> Result<Option<Digest>, SearchError> {
        let inner = &self.inner;
        let user = &criteria.user;
        // `ensureWavesHaveUserDataWavelet`.
        let mut wavelet_ids: BTreeSet<WaveletId> = indexed.into_iter().collect();
        if let Ok(user_data) = build_user_data_wavelet_id(user) {
            wavelet_ids.insert(user_data);
        }
        let names: Vec<WaveletName> = wavelet_ids
            .into_iter()
            .map(|wavelet_id| WaveletName::of(wave_id.clone(), wavelet_id))
            .filter(|name| inner.policy.may_access(user, name))
            .collect();
        // A cheap look first: which wavelets match, at which versions.
        let include =
            |wavelet: &WaveletData| criteria.is_own_user_data(wavelet) || criteria.matches(wavelet);
        let mut versions = Vec::new();
        let mut matching = Vec::new();
        for name in names {
            let looked = inner
                .server
                .with_wavelet(&name, |wavelet| include(wavelet).then(|| wavelet.version()))
                .await;
            match looked {
                Ok(Some(Some(version))) => {
                    versions.push((name.wavelet_id.clone(), version));
                    matching.push(name);
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(wavelet = %name, %error, "cannot read wavelet for search");
                }
            }
        }
        let conversational = |id: &WaveletId| is_conversational_id(id).unwrap_or(false);
        if !versions.iter().any(|(id, _)| conversational(id)) {
            return Ok(None);
        }
        if let Some(digest) = lock(&inner.cache).get(user.address(), wave_id, &versions) {
            return Ok(Some(digest));
        }
        // Copies of the wavelets, so that the digest is built outside
        // their locks.
        let mut wavelets = Vec::with_capacity(matching.len());
        for name in &matching {
            match inner.server.get_snapshot(name).await {
                Ok(Some(committed)) if include(&committed.snapshot) => {
                    wavelets.push(committed.snapshot);
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(wavelet = %name, %error, "cannot read wavelet for search");
                }
            }
        }
        if !wavelets.iter().any(|w| conversational(w.wavelet_id())) {
            return Ok(None);
        }
        let versions: Vec<(WaveletId, u64)> = wavelets
            .iter()
            .map(|wavelet| (wavelet.wavelet_id().clone(), wavelet.version()))
            .collect();
        let references: Vec<&WaveletData> = wavelets.iter().collect();
        let digest = build_digest_with_supplement(user, wave_id, &references);
        lock(&inner.cache).put(user.address(), wave_id, versions, digest.clone());
        Ok(Some(digest))
    }
}

impl SearchProvider for SearchService {
    async fn search(
        &self,
        user: &ParticipantId,
        query: &str,
        start_at: usize,
        num_results: usize,
    ) -> Result<SearchResult, SearchError> {
        let inner = &self.inner;
        let nothing = || SearchResult {
            query: query.to_owned(),
            digests: Vec::new(),
            total_results: 0,
        };
        let parsed = match parse_query(query) {
            Ok(parsed) => parsed,
            Err(error) => {
                // Invalid query: empty result, as in the original.
                tracing::debug!(%user, %error, "invalid search query");
                return Ok(nothing());
            }
        };
        let local_domain = user.domain();
        let participants =
            |kind| build_validated_participant_ids(parsed.values(kind), local_domain);
        let (Ok(with), Ok(creators)) = (
            participants(TokenQueryType::With),
            participants(TokenQueryType::Creator),
        ) else {
            tracing::debug!(%user, query, "invalid participant in search query");
            return Ok(nothing());
        };
        let Some(wave_ids) = parse_wave_ids(parsed.values(TokenQueryType::Id), local_domain) else {
            tracing::debug!(%user, query, "invalid wave id in search query");
            return Ok(nothing());
        };
        let text = index::fts_query(&parsed.text);
        if text.is_some() {
            // Just after start-up the full-text table may not be there yet.
            let mut waited = Duration::ZERO;
            while inner.fts.load(Ordering::Relaxed) == FTS_PENDING && waited < INDEX_WAIT {
                tokio::time::sleep(Duration::from_millis(10)).await;
                waited += Duration::from_millis(10);
            }
            if !inner.full_text() {
                return Ok(nothing());
            }
        }
        let is_all = !parsed.has(TokenQueryType::In);
        let folders: BTreeSet<&str> = parsed
            .values(TokenQueryType::In)
            .iter()
            .map(String::as_str)
            .collect();
        let folders = folders
            .into_iter()
            .filter_map(|folder| match folder {
                "inbox" => Some(Folder::Inbox),
                "archive" => Some(Folder::Archive),
                "mute" | "muted" => Some(Folder::Mute),
                _ => None,
            })
            .collect();
        let criteria = Arc::new(Criteria {
            user: user.clone(),
            shared: inner.shared.clone(),
            is_all,
            with,
            creators,
        });
        let shared = inner
            .shared
            .as_ref()
            .filter(|_| is_all)
            .map(|shared| shared.address().to_owned());
        let address = |id: &ParticipantId| id.address().to_owned();

        self.wait_until_indexed().await;
        let mut waves = inner
            .store
            .find_waves(IndexQuery {
                user: address(user),
                shared: shared.clone(),
                user_data: build_user_data_wavelet_id(user).ok(),
                folders,
                with: criteria.with.iter().map(address).collect(),
                creators: criteria.creators.iter().map(address).collect(),
                wave_ids,
                text,
            })
            .await?;
        let orderings = parsed.orderings();
        waves.sort_by(|a, b| compare(&orderings, a, b));
        let total_results = waves.len();

        // Digests for the page asked for. A wave the live check drops
        // (the index was behind) is made up for from further down.
        let mut digests = Vec::new();
        let mut position = start_at;
        while digests.len() < num_results && position < waves.len() {
            let end = waves
                .len()
                .min(position.saturating_add(num_results - digests.len()));
            let page: Vec<WaveId> = waves[position..end]
                .iter()
                .map(|keys| keys.wave_id.clone())
                .collect();
            let views = inner
                .store
                .wavelets_in_view(page.clone(), user, shared.clone())
                .await?;
            let mut built = futures_util::stream::iter(page.into_iter().zip(views))
                .map(|(wave_id, wavelets)| {
                    let (service, criteria) = (self.clone(), Arc::clone(&criteria));
                    async move { service.digest(&criteria, &wave_id, wavelets).await }
                })
                .buffered(DIGEST_CONCURRENCY);
            while let Some(digest) = built.next().await {
                digests.extend(digest?);
            }
            position = end;
        }
        tracing::debug!(%user, query, results = digests.len(), total_results, "search");
        Ok(SearchResult {
            query: query.to_owned(),
            digests,
            total_results,
        })
    }
}
