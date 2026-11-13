//! Delivery of events to robots and application of their replies
//! (`RobotsGateway`, `Robot`, `RobotOperationApplicator`).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use resonator_model::data::WaveletData;
use resonator_model::id::{ParticipantId, WaveletName};
use resonator_model::operation::WaveletOperationKind;
use resonator_model::version::HashedVersion;
use resonator_server::account::{AccountData, RobotCapabilities};
use resonator_server::storage::{AccountStore, WaveletDeltaRecord};
use resonator_server::waveserver::{SubscriptionId, WaveBusSubscriber, WaveletProvider};
use thiserror::Error;
use tokio::sync::{Notify, mpsc};

use super::generator::{AgentTriggers, EventGenerator};
use super::wavelet_and_deltas::{EventError, WaveletAndDeltas, rollback};
use crate::api::{EventMessageBundle, OperationRequest, ProtocolVersion, RobotName};
use crate::convert::EventDataConverter;
use crate::ops::{BatchResult, BoundWavelet, CapabilitiesFetcher, OperationExecutor, Registry};

/// Failure to deliver a bundle to a robot.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct SinkError(pub String);

/// A bundle of events on its way to a robot.
#[derive(Debug, Clone)]
pub struct EventDelivery {
    /// The robot the bundle is for, proxy part included.
    pub robot: RobotName,
    pub wavelet_name: WaveletName,
    pub bundle: EventMessageBundle,
    /// The protocol version the robot declared; decides the bundle's
    /// shape.
    pub protocol_version: ProtocolVersion,
    /// How many robots acted in a row to cause these events: 0 if they
    /// stem from people or from programs acting on their own, 1 if from a
    /// robot's reaction to such events, and so on. Not part of the
    /// original, see [`DispatcherOptions::max_hops`].
    pub hop_count: u32,
    /// Whether the robot was added or directly replied to. Not part of
    /// the original.
    pub triggers: AgentTriggers,
    /// For acting on the events later than in the reply.
    pub reactor: Reactor,
}

/// Where bundles for a robot go: an HTTP call to an external robot, a
/// stream, or code in this process. The operations returned are the
/// robot's reply and are executed as the robot against the wavelet as the
/// bundle showed it.
///
/// Deliveries to one robot address are made one at a time, in order.
#[async_trait]
pub trait EventSink: Send + Sync {
    async fn deliver(&self, delivery: EventDelivery) -> Result<Vec<OperationRequest>, SinkError>;
}

/// A robot the dispatcher can deliver to.
#[derive(Clone)]
pub struct RobotEndpoint {
    /// The robot's account: its address without proxy and version parts.
    pub account_id: ParticipantId,
    pub capabilities: RobotCapabilities,
    pub sink: Arc<dyn EventSink>,
    /// Which operations the robot's replies may use: [`Registry::Passive`]
    /// for external robots, [`Registry::Agent`] for agents in the server.
    /// It also tells the two apart for deliveries: only an agent in the
    /// server hears who finished editing ([`AgentTriggers::finished_by`]),
    /// possibly in a delivery without events.
    pub registry: Registry,
}

impl std::fmt::Debug for RobotEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RobotEndpoint")
            .field("account_id", &self.account_id)
            .field("registry", &self.registry)
            .finish()
    }
}

/// Finds robots that are not registered with the dispatcher directly,
/// e.g. the external robots of the account store.
#[async_trait]
pub trait RobotDirectory: Send + Sync {
    async fn find(&self, account_id: &ParticipantId) -> Option<RobotEndpoint>;
}

/// The verified external robots of the account store, all delivered to
/// through one sink (`RobotsGateway.waveletUpdate`'s account lookup and
/// `updateRobotAccount`). A robot without capabilities on record has them
/// fetched and stored, if a fetcher is given; otherwise it gets no events.
///
/// Lookups are remembered for a short while ([`AccountRobotDirectory::CACHE_TIME`]),
/// since every update of a wavelet asks about every participant; a change
/// to a robot's account shows after that time.
pub struct AccountRobotDirectory {
    accounts: AccountStore,
    sink: Arc<dyn EventSink>,
    fetcher: Option<Arc<dyn CapabilitiesFetcher>>,
    cache: Mutex<HashMap<ParticipantId, (Instant, Option<RobotEndpoint>)>>,
}

impl AccountRobotDirectory {
    pub fn new(
        accounts: AccountStore,
        sink: Arc<dyn EventSink>,
        fetcher: Option<Arc<dyn CapabilitiesFetcher>>,
    ) -> Self {
        AccountRobotDirectory {
            accounts,
            sink,
            fetcher,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// How long the answer for an address is reused.
    pub const CACHE_TIME: Duration = Duration::from_secs(10);

    /// The most answers remembered.
    const CACHE_SIZE: usize = 10_000;

    async fn look_up(&self, account_id: &ParticipantId) -> Option<RobotEndpoint> {
        let Ok(Some(AccountData::Robot(mut robot))) = self.accounts.get_account(account_id).await
        else {
            return None;
        };
        // Agents are code in this process and register themselves.
        if !robot.is_verified || robot.is_agent() {
            return None;
        }
        let capabilities = match robot.capabilities.clone() {
            Some(capabilities) => capabilities,
            None => {
                let fetched = self.fetcher.as_ref()?.fetch_capabilities(&robot).await;
                let capabilities = fetched
                    .inspect_err(|error| {
                        tracing::info!(robot = %account_id, "couldn't fetch capabilities: {error}");
                    })
                    .ok()?;
                robot.capabilities = Some(capabilities.clone());
                if let Err(error) = self.accounts.put_account(AccountData::Robot(robot)).await {
                    tracing::warn!(robot = %account_id, "couldn't store capabilities: {error}");
                }
                capabilities
            }
        };
        Some(RobotEndpoint {
            account_id: account_id.clone(),
            capabilities,
            sink: Arc::clone(&self.sink),
            registry: Registry::Passive,
        })
    }
}

#[async_trait]
impl RobotDirectory for AccountRobotDirectory {
    async fn find(&self, account_id: &ParticipantId) -> Option<RobotEndpoint> {
        let cached = {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache
                .get(account_id)
                .filter(|(at, _)| at.elapsed() < Self::CACHE_TIME)
                .map(|(_, endpoint)| endpoint.clone())
        };
        if let Some(endpoint) = cached {
            return endpoint;
        }
        let endpoint = self.look_up(account_id).await;
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= Self::CACHE_SIZE {
            cache.retain(|_, (at, _)| at.elapsed() < Self::CACHE_TIME);
            if cache.len() >= Self::CACHE_SIZE {
                cache.clear();
            }
        }
        cache.insert(account_id.clone(), (Instant::now(), endpoint.clone()));
        endpoint
    }
}

/// Settings of a [`RobotDispatcher`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatcherOptions {
    /// The most deltas that may wait for one robot address. When a new
    /// update takes a robot's queue over this, the oldest waiting updates
    /// are dropped, whole, until it fits; the newest update is always
    /// kept. The robot never gets events for dropped deltas. Updates of a
    /// wavelet that follow each other are merged into one bundle while
    /// they wait, so a slow robot gets fewer, larger bundles rather than a
    /// backlog.
    pub max_pending_deltas: usize,
    /// The loop guard: bundles with a hop count above this are not
    /// delivered. `None` turns the guard off, as in the original, where
    /// two robots answering each other never stop.
    pub max_hops: Option<u32>,
    /// How long a sink may take to deliver a bundle and return the reply;
    /// after that the delivery counts as failed. `None` waits forever.
    pub delivery_timeout: Option<Duration>,
    /// How many deltas the dispatcher remembers the hop count of.
    pub hop_memory: usize,
}

impl Default for DispatcherOptions {
    fn default() -> Self {
        DispatcherOptions {
            max_pending_deltas: 1000,
            max_hops: Some(4),
            delivery_timeout: Some(Duration::from_secs(120)),
            hop_memory: 10_000,
        }
    }
}

/// Counters of a [`RobotDispatcher`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DispatcherStats {
    /// Bundles handed to sinks.
    pub bundles_delivered: u64,
    /// Deliveries that failed or timed out.
    pub deliveries_failed: u64,
    /// Bundles withheld by the loop guard.
    pub bundles_suppressed: u64,
    /// Deltas dropped from full queues.
    pub deltas_dropped: u64,
}

/// Remembers which deltas robots made in reaction to events.
#[derive(Debug, Default)]
struct HopTracker {
    /// By wavelet and resulting version: the hop count of the delta.
    recorded: HashMap<(WaveletName, u64), u32>,
    order: VecDeque<(WaveletName, u64)>,
    /// By robot account address: the hop counts of reactions being
    /// executed right now.
    in_flight: HashMap<String, Vec<u32>>,
    capacity: usize,
}

impl HopTracker {
    fn begin(&mut self, account: &str, hop: u32) {
        self.in_flight
            .entry(account.to_owned())
            .or_default()
            .push(hop);
    }

    fn end(&mut self, account: &str, hop: u32) {
        if let Some(hops) = self.in_flight.get_mut(account) {
            if let Some(position) = hops.iter().position(|&h| h == hop) {
                hops.swap_remove(position);
            }
            if hops.is_empty() {
                self.in_flight.remove(account);
            }
        }
    }

    fn record(&mut self, wavelet_name: &WaveletName, version: u64, hop: u32) {
        let key = (wavelet_name.clone(), version);
        if self.recorded.insert(key.clone(), hop).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > self.capacity.max(1) {
            if let Some(oldest) = self.order.pop_front() {
                self.recorded.remove(&oldest);
            }
        }
    }

    /// The hop count of a delta: what was recorded for it, else that of a
    /// reaction its author is executing right now, else zero.
    fn hop_of(&self, wavelet_name: &WaveletName, delta: &WaveletDeltaRecord) -> u32 {
        let key = (wavelet_name.clone(), delta.resulting_version().version());
        if let Some(&hop) = self.recorded.get(&key) {
            return hop;
        }
        let author = account_address(delta.author());
        self.in_flight
            .get(&author)
            .and_then(|hops| hops.iter().copied().max())
            .unwrap_or(0)
    }
}

/// The address of the account behind a participant address: without proxy
/// and version parts if it is a robot's.
fn account_address(participant: &ParticipantId) -> String {
    RobotName::from_address(participant.address())
        .map(|name| name.to_email_address())
        .unwrap_or_else(|| participant.address().to_owned())
}

/// Deltas of one wavelet waiting for a robot.
#[derive(Debug)]
struct Pending {
    wavelet_name: WaveletName,
    /// Consecutive deltas.
    deltas: Vec<Arc<WaveletDeltaRecord>>,
}

#[derive(Debug, Default)]
struct RobotQueue {
    pending: VecDeque<Pending>,
    running: bool,
}

struct Inner {
    executor: Arc<OperationExecutor>,
    options: DispatcherOptions,
    registered: RwLock<HashMap<ParticipantId, RobotEndpoint>>,
    directory: RwLock<Option<Arc<dyn RobotDirectory>>>,
    /// By robot address, proxy part included.
    queues: Mutex<HashMap<String, RobotQueue>>,
    hops: Mutex<HopTracker>,
    /// Updates received and not yet fully handled, plus running workers.
    busy: AtomicUsize,
    idle: Notify,
    bundles_delivered: AtomicU64,
    deliveries_failed: AtomicU64,
    bundles_suppressed: AtomicU64,
    deltas_dropped: AtomicU64,
}

/// For a robot to act on a delivery after it has replied: operations
/// executed through the reactor count as reactions to the delivery for
/// the loop guard, as the reply itself does. An agent that takes its time
/// (say, to stream text into a blip) replies at once, with no operations
/// or just the creation of a blip, and does the rest through this.
#[derive(Clone)]
pub struct Reactor {
    inner: Arc<Inner>,
    account_id: ParticipantId,
    registry: Registry,
    hop_count: u32,
}

impl std::fmt::Debug for Reactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reactor")
            .field("account_id", &self.account_id)
            .field("hop_count", &self.hop_count)
            .finish()
    }
}

impl Reactor {
    /// The robot's account.
    pub fn account_id(&self) -> &ParticipantId {
        &self.account_id
    }

    /// The hop count of the delivery.
    pub fn hop_count(&self) -> u32 {
        self.hop_count
    }

    /// Executes operations as the robot, against the current state of the
    /// wavelets, as a reaction to the delivery. Use the `proxyingFor`
    /// parameter to act as one of the robot's proxies.
    pub async fn execute(&self, requests: Vec<OperationRequest>) -> BatchResult {
        self.inner
            .execute_reaction(
                &self.account_id,
                self.registry,
                &requests,
                None,
                self.hop_count,
            )
            .await
    }
}

impl Inner {
    /// Executes a robot's operations and records the resulting deltas as
    /// one hop further than what the robot reacted to.
    async fn execute_reaction(
        &self,
        account_id: &ParticipantId,
        registry: Registry,
        requests: &[OperationRequest],
        bound: Option<BoundWavelet>,
        hop_count: u32,
    ) -> BatchResult {
        let hop = hop_count.saturating_add(1);
        let account = account_id.address().to_owned();
        self.lock_hops().begin(&account, hop);
        let result = self
            .executor
            .execute_detailed(account_id, registry, requests, bound)
            .await;
        let mut hops = self.lock_hops();
        for submission in &result.submissions {
            if let Ok(submitted) = &submission.result {
                hops.record(
                    &submission.wavelet_name,
                    submitted.hashed_version_after_application.version(),
                    hop,
                );
            }
        }
        hops.end(&account, hop);
        result
    }

    fn lock_hops(&self) -> std::sync::MutexGuard<'_, HopTracker> {
        self.hops.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_queues(&self) -> std::sync::MutexGuard<'_, HashMap<String, RobotQueue>> {
        self.queues.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn work_started(&self) {
        self.busy.fetch_add(1, Ordering::SeqCst);
    }

    fn work_done(&self) {
        if self.busy.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.idle.notify_waiters();
        }
    }

    async fn find_endpoint(&self, account_id: &ParticipantId) -> Option<RobotEndpoint> {
        let registered = self
            .registered
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(account_id)
            .cloned();
        if registered.is_some() {
            return registered;
        }
        let directory = self
            .directory
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        directory?.find(account_id).await
    }

    /// `RobotsGateway.waveletUpdate`: queues the deltas for every robot
    /// that is a participant after them or is added by them.
    async fn handle_update(
        self: &Arc<Self>,
        wavelet_name: WaveletName,
        deltas: Vec<Arc<WaveletDeltaRecord>>,
        participants: Vec<ParticipantId>,
    ) {
        if deltas.is_empty() {
            return;
        }
        let mut candidates = participants;
        for delta in &deltas {
            for op in delta.transformed.ops() {
                if let WaveletOperationKind::AddParticipant { participant, .. } = op.kind()
                    && !candidates.contains(participant)
                {
                    candidates.push(participant.clone());
                }
            }
        }
        for participant in candidates {
            let Some(robot_name) = RobotName::from_address(participant.address()) else {
                continue;
            };
            // A robot gets no events for what it did itself, so deltas
            // that are all its own need not be looked at. (This loses the
            // one event the original would generate from them: a form
            // button click that the robot records in someone else's name.)
            if deltas.iter().all(|delta| delta.author() == &participant) {
                continue;
            }
            let account_id = ParticipantId::new_unchecked(&robot_name.to_email_address());
            if self.find_endpoint(&account_id).await.is_none() {
                continue;
            }
            self.enqueue(robot_name, &wavelet_name, &deltas);
        }
    }

    /// `Robot.waveletUpdate` and `RobotsGateway.ensureScheduled`.
    fn enqueue(
        self: &Arc<Self>,
        robot_name: RobotName,
        wavelet_name: &WaveletName,
        deltas: &[Arc<WaveletDeltaRecord>],
    ) {
        let key = robot_name.to_participant_address();
        let start_worker = {
            let mut queues = self.lock_queues();
            let queue = queues.entry(key).or_default();
            let start_version = deltas[0].applied_at_version.version();
            let last_of_wavelet = queue
                .pending
                .iter_mut()
                .rev()
                .find(|pending| &pending.wavelet_name == wavelet_name);
            match last_of_wavelet {
                Some(pending)
                    if pending
                        .deltas
                        .last()
                        .is_some_and(|d| d.resulting_version().version() == start_version) =>
                {
                    pending.deltas.extend(deltas.iter().cloned());
                }
                // The first deltas of the wavelet, or a gap: a new entry.
                _ => queue.pending.push_back(Pending {
                    wavelet_name: wavelet_name.clone(),
                    deltas: deltas.to_vec(),
                }),
            }
            let mut total: usize = queue.pending.iter().map(|p| p.deltas.len()).sum();
            while total > self.options.max_pending_deltas && queue.pending.len() > 1 {
                if let Some(dropped) = queue.pending.pop_front() {
                    total -= dropped.deltas.len();
                    self.deltas_dropped
                        .fetch_add(dropped.deltas.len() as u64, Ordering::Relaxed);
                    tracing::warn!(
                        robot = %robot_name,
                        wavelet = %dropped.wavelet_name,
                        deltas = dropped.deltas.len(),
                        "robot is too slow, dropping its oldest events"
                    );
                }
            }
            !std::mem::replace(&mut queue.running, true)
        };
        if start_worker {
            self.work_started();
            let inner = Arc::clone(self);
            tokio::spawn(async move {
                inner.run_robot(robot_name).await;
                inner.work_done();
            });
        }
    }

    /// `Robot.run`: handles the robot's queue until it is empty.
    async fn run_robot(self: &Arc<Self>, robot_name: RobotName) {
        let key = robot_name.to_participant_address();
        loop {
            let next = {
                let mut queues = self.lock_queues();
                let Some(queue) = queues.get_mut(&key) else {
                    return;
                };
                match queue.pending.pop_front() {
                    Some(pending) => pending,
                    None => {
                        queues.remove(&key);
                        return;
                    }
                }
            };
            if let Err(error) = self.process(&robot_name, next).await {
                tracing::warn!(robot = %robot_name, "couldn't generate events: {error}");
            }
        }
    }

    /// The wavelet as it was right after the last of some deltas.
    async fn snapshot_after(
        &self,
        wavelet_name: &WaveletName,
        end_version: &HashedVersion,
    ) -> Result<WaveletData, EventError> {
        let server = self.executor.server();
        let unavailable = |what: String| EventError::Operation(what);
        let mut snapshot = server
            .get_snapshot(wavelet_name)
            .await
            .map_err(|e| unavailable(e.to_string()))?
            .ok_or_else(|| unavailable(format!("wavelet {wavelet_name} does not exist")))?
            .snapshot;
        if snapshot.version() < end_version.version() {
            return Err(unavailable(format!(
                "wavelet {wavelet_name} is at version {}, before the deltas' end {}",
                snapshot.version(),
                end_version.version()
            )));
        }
        if snapshot.version() > end_version.version() {
            // The wavelet has moved on; undo what came after.
            let later = server
                .get_history(wavelet_name, end_version, snapshot.hashed_version())
                .await
                .map_err(|e| unavailable(e.to_string()))?;
            rollback(&mut snapshot, &later)?;
        }
        snapshot.set_hashed_version(end_version.clone());
        Ok(snapshot)
    }

    /// `Robot.process`: generates the events of some deltas, delivers
    /// them, and applies the robot's reply.
    async fn process(
        self: &Arc<Self>,
        robot_name: &RobotName,
        pending: Pending,
    ) -> Result<(), EventError> {
        let account_id = ParticipantId::new_unchecked(&robot_name.to_email_address());
        let Some(endpoint) = self.find_endpoint(&account_id).await else {
            return Ok(());
        };
        let Some(last) = pending.deltas.last() else {
            return Ok(());
        };
        let after = self
            .snapshot_after(&pending.wavelet_name, last.resulting_version())
            .await?;
        let deltas = pending
            .deltas
            .iter()
            .map(|record| record.transformed.clone())
            .collect();
        let wavelet_and_deltas = WaveletAndDeltas::create(&after, deltas)?;
        let protocol_version = endpoint.capabilities.protocol_version;
        let mut generated = EventGenerator::new(robot_name.clone()).generate(
            wavelet_and_deltas.snapshot_before_deltas(),
            wavelet_and_deltas.deltas(),
            &endpoint.capabilities.capabilities,
            EventDataConverter::for_version(protocol_version),
        )?;
        let finished_by = std::mem::take(&mut generated.triggers.finished_by);
        // Only agents in this process are told who finished editing: the
        // bundles of external robots stay as in the original, and a bundle
        // without events is never sent to them.
        if endpoint.registry == Registry::Agent {
            for author in finished_by {
                let Ok(participant) = ParticipantId::of(&author) else {
                    continue;
                };
                let account = ParticipantId::new_unchecked(&account_address(&participant));
                // Robots and agents do not finish editing in that sense.
                if self.find_endpoint(&account).await.is_none() {
                    generated.triggers.finished_by.push(author);
                }
            }
        }
        if endpoint.registry != Registry::Agent {
            generated.triggers.mentions.clear();
        }
        // A mention made by annotating text that was already there changes
        // no content and so makes no event; agents still hear of it.
        if generated.bundle.events.is_empty()
            && generated.triggers.finished_by.is_empty()
            && generated.triggers.mentions.is_empty()
        {
            return Ok(());
        }
        let robot_participant = ParticipantId::new_unchecked(&robot_name.to_participant_address());
        let hop_count = {
            let hops = self.lock_hops();
            pending
                .deltas
                .iter()
                .filter(|delta| delta.author() != &robot_participant)
                .map(|delta| hops.hop_of(&pending.wavelet_name, delta))
                .max()
                .unwrap_or(0)
        };
        if self.options.max_hops.is_some_and(|max| hop_count > max) {
            self.bundles_suppressed.fetch_add(1, Ordering::Relaxed);
            tracing::info!(
                robot = %robot_name,
                wavelet = %pending.wavelet_name,
                hop_count,
                "loop guard: not delivering events caused by a chain of robots"
            );
            return Ok(());
        }
        let delivery = EventDelivery {
            robot: robot_name.clone(),
            wavelet_name: pending.wavelet_name.clone(),
            bundle: generated.bundle.for_protocol(protocol_version),
            protocol_version,
            hop_count,
            triggers: generated.triggers,
            reactor: Reactor {
                inner: Arc::clone(self),
                account_id: account_id.clone(),
                registry: endpoint.registry,
                hop_count,
            },
        };
        self.bundles_delivered.fetch_add(1, Ordering::Relaxed);
        let delivered = match self.options.delivery_timeout {
            Some(timeout) => tokio::time::timeout(timeout, endpoint.sink.deliver(delivery))
                .await
                .unwrap_or_else(|_| Err(SinkError("timed out".to_owned()))),
            None => endpoint.sink.deliver(delivery).await,
        };
        let operations = match delivered {
            Ok(operations) => operations,
            Err(error) => {
                // As in the original, a failed delivery is not retried.
                self.deliveries_failed.fetch_add(1, Ordering::Relaxed);
                tracing::info!(robot = %robot_name, "delivery failed: {error}");
                return Ok(());
            }
        };
        if operations.is_empty() {
            return Ok(());
        }
        // RobotOperationApplicator.applyOperations
        let bound = BoundWavelet {
            snapshot: wavelet_and_deltas.snapshot_after_deltas().clone(),
        };
        self.execute_reaction(
            &account_id,
            endpoint.registry,
            &operations,
            Some(bound),
            hop_count,
        )
        .await;
        Ok(())
    }
}

/// What the wave bus calls: hands every update to the dispatcher's task.
struct BusSubscriber {
    inner: Arc<Inner>,
    updates: mpsc::UnboundedSender<Update>,
}

type Update = (
    WaveletName,
    Vec<Arc<WaveletDeltaRecord>>,
    Vec<ParticipantId>,
);

impl WaveBusSubscriber for BusSubscriber {
    fn wavelet_update(&self, wavelet: &WaveletData, deltas: &[Arc<WaveletDeltaRecord>]) {
        self.inner.work_started();
        let update = (
            wavelet.wavelet_name(),
            deltas.to_vec(),
            wavelet.participants().to_vec(),
        );
        if self.updates.send(update).is_err() {
            self.inner.work_done();
        }
    }

    fn wavelet_committed(&self, _: &WaveletName, _: &HashedVersion) {
        // Robots are told of updates, not of commits.
    }
}

/// A running subscription of a dispatcher to the wave bus; ends when
/// stopped or dropped.
#[derive(Debug)]
pub struct DispatcherHandle {
    server_bus: Arc<resonator_server::waveserver::WaveBus>,
    subscription: SubscriptionId,
    task: tokio::task::JoinHandle<()>,
}

impl DispatcherHandle {
    /// Stops listening to the wave bus. Deliveries under way finish.
    pub fn stop(self) {}
}

impl Drop for DispatcherHandle {
    fn drop(&mut self) {
        self.server_bus.unsubscribe(self.subscription);
        self.task.abort();
    }
}

/// Sends robots the events of wavelet updates and applies their replies
/// (`RobotsGateway` with its `Robot`s).
///
/// For every update on the wave bus, each robot that is a participant of
/// the wavelet, or is made one by the update, has the deltas queued. One
/// task per robot address works its queue off: it generates the events of
/// the next waiting deltas, delivers the bundle through the robot's
/// [`EventSink`], and executes the operations that come back, bound to the
/// wavelet as the bundle showed it. The wave bus is never held up: its
/// callback only passes the update on.
///
/// Queues are bounded, see [`DispatcherOptions::max_pending_deltas`].
///
/// Cheap to clone.
#[derive(Clone)]
pub struct RobotDispatcher {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for RobotDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RobotDispatcher")
            .field("options", &self.inner.options)
            .finish()
    }
}

impl RobotDispatcher {
    pub fn new(executor: Arc<OperationExecutor>, options: DispatcherOptions) -> Self {
        let hops = HopTracker {
            capacity: options.hop_memory,
            ..HopTracker::default()
        };
        RobotDispatcher {
            inner: Arc::new(Inner {
                executor,
                options,
                registered: RwLock::new(HashMap::new()),
                directory: RwLock::new(None),
                queues: Mutex::new(HashMap::new()),
                hops: Mutex::new(hops),
                busy: AtomicUsize::new(0),
                idle: Notify::new(),
                bundles_delivered: AtomicU64::new(0),
                deliveries_failed: AtomicU64::new(0),
                bundles_suppressed: AtomicU64::new(0),
                deltas_dropped: AtomicU64::new(0),
            }),
        }
    }

    pub fn executor(&self) -> &Arc<OperationExecutor> {
        &self.inner.executor
    }

    pub fn options(&self) -> &DispatcherOptions {
        &self.inner.options
    }

    /// Registers a robot, replacing any registered under the same account.
    /// Its proxies (`robot+proxy@domain`) are served too.
    pub fn register(&self, endpoint: RobotEndpoint) {
        self.inner
            .registered
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(endpoint.account_id.clone(), endpoint);
    }

    pub fn unregister(&self, account_id: &ParticipantId) {
        self.inner
            .registered
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(account_id);
    }

    pub fn is_registered(&self, account_id: &ParticipantId) -> bool {
        self.inner
            .registered
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(account_id)
    }

    /// Sets where robots that are not registered are looked up.
    pub fn set_directory(&self, directory: Option<Arc<dyn RobotDirectory>>) {
        *self
            .inner
            .directory
            .write()
            .unwrap_or_else(|e| e.into_inner()) = directory;
    }

    /// Subscribes to the wave server's bus and starts handling updates.
    /// Must be called within a Tokio runtime.
    pub fn start(&self) -> DispatcherHandle {
        let (sender, mut receiver) = mpsc::unbounded_channel::<Update>();
        let server_bus = Arc::clone(self.inner.executor.server().bus());
        let subscription = server_bus.subscribe(Arc::new(BusSubscriber {
            inner: Arc::clone(&self.inner),
            updates: sender,
        }));
        let inner = Arc::clone(&self.inner);
        let task = tokio::spawn(async move {
            while let Some((wavelet_name, deltas, participants)) = receiver.recv().await {
                inner
                    .handle_update(wavelet_name, deltas, participants)
                    .await;
                inner.work_done();
            }
        });
        DispatcherHandle {
            server_bus,
            subscription,
            task,
        }
    }

    /// Handles one wavelet update: what [`RobotDispatcher::start`] does for
    /// every update of the wave bus. `participants` are the wavelet's
    /// participants after the deltas.
    pub async fn wavelet_update(
        &self,
        wavelet_name: WaveletName,
        deltas: Vec<Arc<WaveletDeltaRecord>>,
        participants: Vec<ParticipantId>,
    ) {
        self.inner.work_started();
        self.inner
            .handle_update(wavelet_name, deltas, participants)
            .await;
        self.inner.work_done();
    }

    /// Waits until every update received so far has been handled: events
    /// delivered, replies applied, and the updates those caused handled
    /// too. Operations a robot executes later through a [`Reactor`] are
    /// not waited for.
    pub async fn idle(&self) {
        loop {
            let notified = self.inner.idle.notified();
            if self.inner.busy.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }

    pub fn stats(&self) -> DispatcherStats {
        DispatcherStats {
            bundles_delivered: self.inner.bundles_delivered.load(Ordering::Relaxed),
            deliveries_failed: self.inner.deliveries_failed.load(Ordering::Relaxed),
            bundles_suppressed: self.inner.bundles_suppressed.load(Ordering::Relaxed),
            deltas_dropped: self.inner.deltas_dropped.load(Ordering::Relaxed),
        }
    }

    /// A reactor for a robot that acts on its own account, outside any
    /// delivery, but wants its operations counted as a reaction at a given
    /// hop count.
    pub fn reactor(
        &self,
        account_id: ParticipantId,
        registry: Registry,
        hop_count: u32,
    ) -> Reactor {
        Reactor {
            inner: Arc::clone(&self.inner),
            account_id,
            registry,
            hop_count,
        }
    }
}
