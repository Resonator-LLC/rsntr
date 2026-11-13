//! The client front end: wave view subscriptions and delta submission on
//! behalf of participants.
//!
//! Derived from `ClientFrontendImpl`, `UserManager`, `WaveViewSubscription`,
//! `WaveletInfo` and `WaveClientRpcImpl` in
//! `org.waveprotocol.box.server.frontend` of Apache Wave, licensed under
//! the Apache License, Version 2.0.
//!
//! This is the layer everything that acts as a participant goes through:
//! the WebSocket protocol ([`crate::rpc`]) and, in process, agents and
//! robot gateways. [`ClientFrontend::open`] subscribes a participant to a
//! wave view and delivers [`WaveViewUpdate`]s to an [`UpdateSink`];
//! [`ClientFrontend::submit_request`] submits a delta.
//!
//! What a subscription sees, as in the original: the wavelets of the wave
//! that its id filter accepts and that the participant may access (a
//! participant, or anyone of the domain if the wavelet has the shared
//! domain participant). After the open request it receives a snapshot of
//! each such wavelet (or just a channel id if there is none), then a
//! marker, then deltas and commit notices as they happen. A wavelet created
//! later, or one the participant is added to later, comes into view with
//! its deltas from version zero or with a snapshot. A participant removed
//! from a wavelet still receives the delta that removes it and nothing
//! after. A subscription's own submissions (those naming its channel id)
//! are not echoed back to it; deltas arriving while such a submission is
//! outstanding are held back until it is answered.
//!
//! Differences from the original, which are deliberate:
//! - Updates that happen while the view is being opened are delivered
//!   after the marker and never duplicate or precede a snapshot (the
//!   original can deliver them early and repeat the snapshot's content).
//! - Known wavelet versions in the open request are honoured (the original
//!   answers "Known wavelets not supported"): see [`ClientFrontend::open`].
//! - Participants who see a wavelet through the shared domain participant
//!   receive its live updates (the original only sends them the snapshot).
//! - A wavelet coming into view in the middle of its history is announced
//!   with a snapshot (the original sends bare deltas the client cannot
//!   apply).
//! - Subscriptions end ([`SubscriptionHandle`]); the original keeps them
//!   for the life of the server.
//! - A failed submission releases held-back deltas (the original then
//!   holds them back for ever).
//! - Who may use which wavelet is checked against a [`WaveletPolicy`].

mod policy;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use resonator_model::data::WaveletData;
use resonator_model::id::{IdFilter, ParticipantId, WaveId, WaveletId, WaveletName};
use resonator_model::id::{IdSerialiser, ModernIdSerialiser};
use resonator_model::operation::WaveletOperationKind;
use resonator_model::proto::federation::ProtocolWaveletDelta;
use resonator_model::proto::waveclient_rpc::ProtocolWaveletUpdate;
use resonator_model::proto::{
    ProtoError, serialize_hashed_version, serialize_transformed_delta, serialize_wavelet,
};
use resonator_model::version::HashedVersion;
use tokio::sync::mpsc;

pub use policy::{DefaultPolicy, WaveletPolicy};

use crate::storage::WaveletDeltaRecord;
use crate::waveserver::{
    SubmitError, SubmitResult, SubscriptionId, WaveBusSubscriber, WaveServer, WaveletProvider,
};

/// Events of one subscription buffered while its view is being opened,
/// before the subscription is given up as overloaded.
const MAX_PENDING_EVENTS: usize = 4096;

/// A message of a wave view stream (the arguments of
/// `ClientFrontend.OpenListener.onUpdate`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaveViewUpdate {
    pub wavelet_name: WaveletName,
    /// The wavelet's state, when it comes into view.
    pub snapshot: Option<Arc<WaveletData>>,
    /// Deltas applied to the wavelet, in order.
    pub deltas: Vec<Arc<WaveletDeltaRecord>>,
    /// Set on the message that resynchronises a wavelet the client already
    /// knows: the known version the following deltas start from.
    pub resync_version: Option<HashedVersion>,
    /// The version up to which the wavelet is durably stored.
    pub committed_version: Option<HashedVersion>,
    /// Set on the message that ends the initial snapshots.
    pub marker: Option<bool>,
    pub channel_id: Option<String>,
    /// Set when the wavelet's history was rewound to this version (a
    /// change of host epoch, see [`crate::waveserver::EpochAdoption`]):
    /// `deltas` are then the deltas the client has seen since that
    /// version, which no longer exist, in order. Deltas that follow
    /// continue from this version.
    pub reset_version: Option<HashedVersion>,
}

impl WaveViewUpdate {
    fn new(wavelet_name: WaveletName) -> Self {
        WaveViewUpdate {
            wavelet_name,
            snapshot: None,
            deltas: Vec::new(),
            resync_version: None,
            committed_version: None,
            marker: None,
            channel_id: None,
            reset_version: None,
        }
    }

    /// The version of the wavelet after this update, if the update says.
    pub fn resulting_version(&self) -> Option<&HashedVersion> {
        if let Some(version) = &self.reset_version {
            return Some(version);
        }
        match (&self.snapshot, self.deltas.last()) {
            (Some(snapshot), _) => Some(snapshot.hashed_version()),
            (None, Some(delta)) => Some(delta.resulting_version()),
            (None, None) => self.resync_version.as_ref(),
        }
    }

    /// The wire message (`WaveClientRpcImpl.open`'s listener). As in the
    /// original, a snapshot's own `version` field is the committed version
    /// (when known) and `resulting_version` the version of the state.
    pub fn to_proto(&self) -> Result<ProtocolWaveletUpdate, ProtoError> {
        let mut update = ProtocolWaveletUpdate {
            wavelet_name: Some(ModernIdSerialiser.serialise_wavelet_name(&self.wavelet_name)?),
            marker: self.marker,
            channel_id: self.channel_id.clone(),
            ..Default::default()
        };
        for delta in &self.deltas {
            update
                .applied_delta
                .push(serialize_transformed_delta(&delta.transformed)?);
        }
        if let Some(version) = self.resulting_version() {
            update.resulting_version = Some(serialize_hashed_version(version)?);
        }
        if let Some(snapshot) = &self.snapshot {
            let version = self
                .committed_version
                .as_ref()
                .unwrap_or_else(|| snapshot.hashed_version());
            update.snapshot = Some(serialize_wavelet(snapshot, version)?);
        }
        if let Some(version) = &self.committed_version {
            update.commit_notice = Some(serialize_hashed_version(version)?);
        }
        if let Some(version) = &self.reset_version {
            update.reset_version = Some(serialize_hashed_version(version)?);
        }
        Ok(update)
    }
}

/// Receives the messages of a wave view (`ClientFrontend.OpenListener`).
///
/// Both methods are called synchronously while wavelets are locked: they
/// must not block and must not call back into the front end. Put the
/// message on a queue.
pub trait UpdateSink: Send + Sync + 'static {
    /// Delivers a message. Returning `false` means the receiver is gone or
    /// cannot keep up; the subscription then ends without further calls.
    fn update(&self, update: WaveViewUpdate) -> bool;

    /// The stream ended with an error; no further calls follow.
    fn failed(&self, error: &str);

    /// Resolves when the receiver has room for a large message. Awaited
    /// before each message of an opening view up to the marker (never
    /// while a wavelet is locked), so that a view of many large wavelets
    /// is paced by its receiver instead of overrunning it. The default
    /// never waits.
    fn ready(&self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::ready(()))
    }

    /// Whether the receiver understands [`WaveViewUpdate::reset_version`].
    /// A subscription whose receiver does not ends with an error when a
    /// wavelet it sees is rewound, so that the receiver reopens the view.
    fn accepts_resets(&self) -> bool {
        false
    }
}

/// What a channel subscription ([`ClientFrontend::open_channel`]) yields.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum WaveViewEvent {
    Update(WaveViewUpdate),
    /// The stream ended with an error.
    Failed(String),
}

struct ChannelSink(mpsc::Sender<WaveViewEvent>);

impl UpdateSink for ChannelSink {
    fn update(&self, update: WaveViewUpdate) -> bool {
        self.0.try_send(WaveViewEvent::Update(update)).is_ok()
    }

    fn failed(&self, error: &str) {
        let _ = self.0.try_send(WaveViewEvent::Failed(error.to_owned()));
    }
}

/// A wavelet version the client says it has (`WaveletVersion` of the open
/// request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownWavelet {
    pub wavelet_id: WaveletId,
    pub version: HashedVersion,
}

/// Why a wave view could not be opened. The display string is the message
/// for the client.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenError {
    #[error("Wave server failed to look up wave")]
    Lookup,
    #[error("Wave server failure retrieving wavelet")]
    Snapshot,
    /// The receiver went away or fell behind while the view was opened.
    #[error("Wave view receiver cannot keep up")]
    Overloaded,
}

/// The state of one wavelet within a subscription
/// (`WaveViewSubscription.WaveletChannelState`).
#[derive(Default)]
struct WaveletChannel {
    /// The version the client has been brought to; `None` while the
    /// wavelet is not in view.
    last_version: Option<u64>,
    /// End versions of the subscription's own deltas, which are not echoed.
    submitted_end_versions: HashSet<u64>,
    /// Submissions on this channel awaiting their response.
    outstanding_submits: usize,
    /// Deltas received while a submission was outstanding.
    held_back: Vec<Arc<WaveletDeltaRecord>>,
    /// A rewind that happened while a submission was outstanding: the
    /// fork version and the deltas taken out.
    pending_reset: Option<(HashedVersion, Vec<Arc<WaveletDeltaRecord>>)>,
}

enum Pending {
    Update {
        wavelet_name: WaveletName,
        deltas: Vec<Arc<WaveletDeltaRecord>>,
        participants: Vec<ParticipantId>,
    },
    Commit {
        wavelet_name: WaveletName,
        version: HashedVersion,
    },
    Reset {
        wavelet_name: WaveletName,
        version: HashedVersion,
        rewound: Vec<Arc<WaveletDeltaRecord>>,
    },
}

struct SubscriptionState {
    /// While the view is being opened, events queue up here.
    opening: Option<Vec<Pending>>,
    channels: HashMap<WaveletId, WaveletChannel>,
}

/// A subscription to a wave view (`WaveViewSubscription`).
struct Subscription {
    id: u64,
    participant: ParticipantId,
    wave_id: WaveId,
    filter: IdFilter,
    channel_id: String,
    sink: Box<dyn UpdateSink>,
    state: Mutex<SubscriptionState>,
    /// Set once the stream ended; nothing is delivered afterwards.
    closed: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether `participant` sees a wavelet with these participants
/// (`WaveletDataUtil.checkAccessPermission`).
fn sees(participant: &ParticipantId, participants: &[ParticipantId]) -> bool {
    participants.contains(participant)
        || ParticipantId::shared_domain_participant(participant.domain())
            .is_ok_and(|shared| participants.contains(&shared))
}

impl Subscription {
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn includes(&self, wavelet_id: &WaveletId) -> bool {
        self.filter.accepts(wavelet_id)
    }

    /// Delivers a message; ends the subscription if the sink refuses it.
    fn send(&self, update: WaveViewUpdate) -> bool {
        if self.is_closed() {
            return false;
        }
        if self.sink.update(update) {
            true
        } else {
            self.closed.store(true, Ordering::SeqCst);
            false
        }
    }

    /// Ends the stream with an error.
    fn fail(&self, error: &str) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.sink.failed(error);
        }
    }

    fn update_for(&self, wavelet_name: &WaveletName) -> WaveViewUpdate {
        WaveViewUpdate {
            channel_id: Some(self.channel_id.clone()),
            ..WaveViewUpdate::new(wavelet_name.clone())
        }
    }

    /// Sends deltas that are not the subscription's own
    /// (`WaveViewSubscription.filterOwnDeltas` and `sendUpdate`).
    fn send_deltas(
        &self,
        wavelet_name: &WaveletName,
        channel: &mut WaveletChannel,
        deltas: Vec<Arc<WaveletDeltaRecord>>,
    ) {
        let deltas: Vec<_> = deltas
            .into_iter()
            .filter(|delta| {
                !channel
                    .submitted_end_versions
                    .remove(&delta.resulting_version().version())
            })
            .collect();
        // Own end versions at or below the delivered version can no
        // longer match anything.
        if let Some(last) = channel.last_version {
            channel
                .submitted_end_versions
                .retain(|version| *version > last);
        }
        if !deltas.is_empty() {
            self.send(WaveViewUpdate {
                deltas,
                ..self.update_for(wavelet_name)
            });
        }
    }

    /// Handles deltas applied to a wavelet of this subscription's wave
    /// (`ClientFrontendImpl.waveletUpdate` and
    /// `WaveViewSubscription.onUpdate` for one subscription).
    /// `participants` are the wavelet's participants after the deltas and
    /// `wavelet` its state after them, if at hand.
    fn on_update(
        &self,
        state: &mut SubscriptionState,
        policy: &dyn WaveletPolicy,
        wavelet_name: &WaveletName,
        deltas: &[Arc<WaveletDeltaRecord>],
        participants: &[ParticipantId],
        wavelet: Option<&WaveletData>,
    ) {
        if deltas.is_empty()
            || !self.includes(&wavelet_name.wavelet_id)
            || !policy.may_access(&self.participant, wavelet_name)
        {
            return;
        }
        let visible_after = sees(&self.participant, participants);
        // A participant removed by these deltas sees them up to its
        // removal.
        let visible_deltas = if visible_after {
            deltas
        } else {
            let shared = ParticipantId::shared_domain_participant(self.participant.domain()).ok();
            let removal = deltas.iter().rposition(|delta| {
                delta.transformed.ops().iter().any(|op| {
                    matches!(
                        op.kind(),
                        WaveletOperationKind::RemoveParticipant { participant }
                            if *participant == self.participant || Some(participant) == shared.as_ref()
                    )
                })
            });
            match removal {
                Some(index) => &deltas[..=index],
                None => return,
            }
        };
        let channel = state
            .channels
            .entry(wavelet_name.wavelet_id.clone())
            .or_default();
        let mut new_deltas = visible_deltas;
        match channel.last_version {
            Some(last) => {
                // Skip what a snapshot already covered.
                let covered = new_deltas
                    .iter()
                    .take_while(|delta| delta.resulting_version().version() <= last)
                    .count();
                new_deltas = &new_deltas[covered..];
                let Some(first) = new_deltas.first() else {
                    return;
                };
                if first.applied_at_version.version() != last {
                    // `WaveViewSubscription.checkUpdateVersion`.
                    tracing::error!(
                        wavelet = %wavelet_name,
                        expected = last,
                        got = first.applied_at_version.version(),
                        channel = %self.channel_id,
                        "subscription received a delta that does not continue its view"
                    );
                    self.fail("Wave view lost synchronisation; reopen the wave");
                    return;
                }
            }
            None => {
                // The wavelet comes into view.
                let starts_at_zero = new_deltas
                    .first()
                    .is_some_and(|delta| delta.applied_at_version.version() == 0);
                if !starts_at_zero {
                    if !visible_after {
                        // Added and removed again before it saw anything.
                        state.channels.remove(&wavelet_name.wavelet_id);
                        return;
                    }
                    match wavelet {
                        Some(wavelet) => {
                            channel.last_version = Some(wavelet.version());
                            self.send(WaveViewUpdate {
                                snapshot: Some(Arc::new(wavelet.clone())),
                                ..self.update_for(wavelet_name)
                            });
                        }
                        None => {
                            // Only when replaying events that queued up
                            // while the view was opened; too rare to merit
                            // fetching the snapshot from here.
                            self.fail("Wave view changed while it was opened; reopen the wave");
                        }
                    }
                    return;
                }
            }
        }
        channel.last_version = new_deltas
            .last()
            .map(|delta| delta.resulting_version().version());
        if !visible_after {
            // Lost access: flush everything now, then forget the wavelet.
            let mut all = std::mem::take(&mut channel.held_back);
            all.extend(new_deltas.iter().cloned());
            self.send_deltas(wavelet_name, channel, all);
            state.channels.remove(&wavelet_name.wavelet_id);
        } else if channel.outstanding_submits > 0 {
            channel.held_back.extend(new_deltas.iter().cloned());
        } else {
            self.send_deltas(wavelet_name, channel, new_deltas.to_vec());
        }
    }

    /// `WaveViewSubscription.onCommit`, for wavelets in view.
    fn on_commit(
        &self,
        state: &mut SubscriptionState,
        wavelet_name: &WaveletName,
        version: &HashedVersion,
    ) {
        let in_view = state
            .channels
            .get(&wavelet_name.wavelet_id)
            .is_some_and(|channel| channel.last_version.is_some());
        if in_view {
            self.send(WaveViewUpdate {
                committed_version: Some(version.clone()),
                ..self.update_for(wavelet_name)
            });
        }
    }

    /// A submission naming this subscription's channel begins
    /// (`WaveViewSubscription.submitRequest`). Unlike the original,
    /// overlapping submissions are counted rather than refused.
    fn submit_started(&self, wavelet_id: &WaveletId) {
        lock(&self.state)
            .channels
            .entry(wavelet_id.clone())
            .or_default()
            .outstanding_submits += 1;
    }

    /// The submission was answered, with the resulting version if it
    /// succeeded (`WaveViewSubscription.submitResponse`): deltas held back
    /// meanwhile are released, minus the subscription's own.
    fn submit_finished(&self, wavelet_name: &WaveletName, resulting_version: Option<u64>) {
        let mut state = lock(&self.state);
        let Some(channel) = state.channels.get_mut(&wavelet_name.wavelet_id) else {
            return;
        };
        channel.outstanding_submits = channel.outstanding_submits.saturating_sub(1);
        if let Some(version) = resulting_version {
            channel.submitted_end_versions.insert(version);
        }
        if channel.outstanding_submits == 0 {
            if let Some((version, rewound)) = channel.pending_reset.take() {
                self.reset_channel(wavelet_name, channel, &version, &rewound);
            }
            let held_back = std::mem::take(&mut channel.held_back);
            self.send_deltas(wavelet_name, channel, held_back);
        }
    }

    /// The history of a wavelet was rewound to `version`; `rewound` are
    /// the deltas taken out, in order.
    fn on_reset(
        &self,
        state: &mut SubscriptionState,
        wavelet_name: &WaveletName,
        version: &HashedVersion,
        rewound: &[Arc<WaveletDeltaRecord>],
    ) {
        let Some(channel) = state.channels.get_mut(&wavelet_name.wavelet_id) else {
            return;
        };
        let Some(last) = channel.last_version else {
            return;
        };
        if last <= version.version() {
            return;
        }
        if !self.sink.accepts_resets() {
            self.fail("Wave view reset after a change of host; reopen the wave");
            return;
        }
        if channel.outstanding_submits > 0 {
            // After the submission's answer, so that the client knows
            // where its delta went.
            if channel.pending_reset.is_some() {
                // Two rewinds during one submission: too rare to merge.
                self.fail("Wave view reset twice; reopen the wave");
                return;
            }
            channel.pending_reset = Some((version.clone(), rewound.to_vec()));
            return;
        }
        self.reset_channel(wavelet_name, channel, version, rewound);
    }

    /// Tells the client of a rewind: the deltas it has seen since the
    /// fork go back. Deltas held back for a submission go out first, so
    /// that what goes back is everything up to the channel's version, the
    /// client's own deltas (which it has from their acknowledgement)
    /// included.
    fn reset_channel(
        &self,
        wavelet_name: &WaveletName,
        channel: &mut WaveletChannel,
        version: &HashedVersion,
        rewound: &[Arc<WaveletDeltaRecord>],
    ) {
        let held_back = std::mem::take(&mut channel.held_back);
        self.send_deltas(wavelet_name, channel, held_back);
        let Some(last) = channel.last_version else {
            return;
        };
        channel.submitted_end_versions.clear();
        if last <= version.version() {
            return;
        }
        channel.last_version = Some(version.version());
        let seen: Vec<_> = rewound
            .iter()
            .take_while(|delta| delta.resulting_version().version() <= last)
            .cloned()
            .collect();
        let continuous = seen
            .first()
            .is_some_and(|delta| delta.applied_at_version == *version)
            && seen
                .last()
                .is_some_and(|delta| delta.resulting_version().version() == last);
        if !continuous {
            tracing::error!(
                wavelet = %wavelet_name,
                fork = version.version(),
                last,
                channel = %self.channel_id,
                "rewound deltas do not cover the subscription's view"
            );
            self.fail("Wave view lost synchronisation; reopen the wave");
            return;
        }
        self.send(WaveViewUpdate {
            deltas: seen,
            reset_version: Some(version.clone()),
            ..self.update_for(wavelet_name)
        });
    }

    /// Queues an event if the view is still being opened; gives the event
    /// back otherwise.
    fn queue_if_opening(&self, state: &mut SubscriptionState, event: Pending) -> Option<Pending> {
        match &mut state.opening {
            Some(pending) => {
                if pending.len() >= MAX_PENDING_EVENTS {
                    self.fail(&OpenError::Overloaded.to_string());
                } else {
                    pending.push(event);
                }
                None
            }
            None => Some(event),
        }
    }
}

struct Inner {
    server: WaveServer,
    policy: Arc<dyn WaveletPolicy>,
    /// Subscriptions by wave (`UserManager.subscriptions`, for all users).
    waves: Mutex<HashMap<WaveId, Vec<Arc<Subscription>>>>,
    next_subscription: AtomicU64,
    bus_subscription: Mutex<Option<SubscriptionId>>,
    submits: AtomicU64,
    submit_errors: AtomicU64,
}

impl Inner {
    fn subscriptions_of(&self, wave_id: &WaveId) -> Vec<Arc<Subscription>> {
        lock(&self.waves).get(wave_id).cloned().unwrap_or_default()
    }

    fn remove(&self, subscription: &Subscription) {
        let mut waves = lock(&self.waves);
        if let Some(subscriptions) = waves.get_mut(&subscription.wave_id) {
            subscriptions.retain(|other| other.id != subscription.id);
            if subscriptions.is_empty() {
                waves.remove(&subscription.wave_id);
            }
        }
    }
}

/// Forwards wave bus messages to the subscriptions.
struct BusAdapter(Weak<Inner>);

impl WaveBusSubscriber for BusAdapter {
    fn wavelet_update(&self, wavelet: &WaveletData, deltas: &[Arc<WaveletDeltaRecord>]) {
        let Some(inner) = self.0.upgrade() else {
            return;
        };
        if deltas.is_empty() {
            return;
        }
        let subscriptions = inner.subscriptions_of(wavelet.wave_id());
        if subscriptions.is_empty() {
            return;
        }
        let wavelet_name = wavelet.wavelet_name();
        for subscription in subscriptions {
            if subscription.is_closed() {
                continue;
            }
            let mut state = lock(&subscription.state);
            let event = Pending::Update {
                wavelet_name: wavelet_name.clone(),
                deltas: deltas.to_vec(),
                participants: wavelet.participants().to_vec(),
            };
            if subscription.queue_if_opening(&mut state, event).is_some() {
                subscription.on_update(
                    &mut state,
                    inner.policy.as_ref(),
                    &wavelet_name,
                    deltas,
                    wavelet.participants(),
                    Some(wavelet),
                );
            }
        }
    }

    fn wavelet_committed(&self, wavelet_name: &WaveletName, version: &HashedVersion) {
        let Some(inner) = self.0.upgrade() else {
            return;
        };
        for subscription in inner.subscriptions_of(&wavelet_name.wave_id) {
            if subscription.is_closed() {
                continue;
            }
            let mut state = lock(&subscription.state);
            let event = Pending::Commit {
                wavelet_name: wavelet_name.clone(),
                version: version.clone(),
            };
            if subscription.queue_if_opening(&mut state, event).is_some() {
                subscription.on_commit(&mut state, wavelet_name, version);
            }
        }
    }

    fn wavelet_reset(
        &self,
        wavelet_name: &WaveletName,
        version: &HashedVersion,
        rewound: &[Arc<WaveletDeltaRecord>],
    ) {
        let Some(inner) = self.0.upgrade() else {
            return;
        };
        for subscription in inner.subscriptions_of(&wavelet_name.wave_id) {
            if subscription.is_closed() {
                continue;
            }
            let mut state = lock(&subscription.state);
            let event = Pending::Reset {
                wavelet_name: wavelet_name.clone(),
                version: version.clone(),
                rewound: rewound.to_vec(),
            };
            if subscription.queue_if_opening(&mut state, event).is_some() {
                subscription.on_reset(&mut state, wavelet_name, version, rewound);
            }
        }
    }
}

/// Keeps a subscription alive; dropping it (or calling
/// [`SubscriptionHandle::close`]) ends the subscription.
pub struct SubscriptionHandle {
    inner: Weak<Inner>,
    subscription: Arc<Subscription>,
}

impl std::fmt::Debug for SubscriptionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubscriptionHandle")
            .field("wave_id", &self.subscription.wave_id)
            .field("channel_id", &self.subscription.channel_id)
            .finish()
    }
}

impl SubscriptionHandle {
    /// The channel id, which submissions name so that their deltas are not
    /// echoed to this subscription.
    pub fn channel_id(&self) -> &str {
        &self.subscription.channel_id
    }

    pub fn wave_id(&self) -> &WaveId {
        &self.subscription.wave_id
    }

    /// Whether the stream ended (the sink refused a message or the
    /// subscription failed).
    pub fn is_closed(&self) -> bool {
        self.subscription.is_closed()
    }

    /// Ends the subscription.
    pub fn close(self) {}
}

impl Drop for SubscriptionHandle {
    fn drop(&mut self) {
        {
            // Under the lock that deliveries hold, so that none is in
            // flight once this returns.
            let _state = lock(&self.subscription.state);
            self.subscription.closed.store(true, Ordering::SeqCst);
        }
        if let Some(inner) = self.inner.upgrade() {
            inner.remove(&self.subscription);
        }
    }
}

/// The client front end (`ClientFrontendImpl`); cheap to clone.
#[derive(Clone)]
pub struct ClientFrontend {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ClientFrontend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientFrontend")
            .field("subscriptions", &self.subscription_count())
            .finish()
    }
}

/// Channel ids are unique within the process (`channel_counter`).
static CHANNEL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The name under which the channel id and the marker are sent when no
/// wavelet is (`ClientFrontendImpl.createDummyWaveletName`).
pub fn dummy_wavelet_name(wave_id: &WaveId) -> WaveletName {
    // "dummy+root" is a valid id in any valid domain.
    let wavelet_id = WaveletId::of(wave_id.domain(), "dummy+root")
        .or_else(|_| WaveletId::of_legacy(wave_id.domain(), "dummy+root"))
        .expect("the domain of a wave id is valid in a wavelet id");
    WaveletName::of(wave_id.clone(), wavelet_id)
}

impl ClientFrontend {
    /// Creates the front end of `server` with the default policy and
    /// subscribes it to the server's wave bus.
    pub fn new(server: WaveServer) -> Self {
        Self::with_policy(server, Arc::new(DefaultPolicy))
    }

    pub fn with_policy(server: WaveServer, policy: Arc<dyn WaveletPolicy>) -> Self {
        let inner = Arc::new(Inner {
            server,
            policy,
            waves: Mutex::new(HashMap::new()),
            next_subscription: AtomicU64::new(0),
            bus_subscription: Mutex::new(None),
            submits: AtomicU64::new(0),
            submit_errors: AtomicU64::new(0),
        });
        let id = inner
            .server
            .bus()
            .subscribe(Arc::new(BusAdapter(Arc::downgrade(&inner))));
        *lock(&inner.bus_subscription) = Some(id);
        ClientFrontend { inner }
    }

    pub fn server(&self) -> &WaveServer {
        &self.inner.server
    }

    pub fn policy(&self) -> &Arc<dyn WaveletPolicy> {
        &self.inner.policy
    }

    /// Number of open subscriptions.
    pub fn subscription_count(&self) -> usize {
        lock(&self.inner.waves).values().map(Vec::len).sum()
    }

    /// Submissions handled and how many of them failed.
    pub fn submit_counts(&self) -> (u64, u64) {
        (
            self.inner.submits.load(Ordering::Relaxed),
            self.inner.submit_errors.load(Ordering::Relaxed),
        )
    }

    /// Detaches from the wave bus and ends every subscription.
    pub fn shutdown(&self) {
        if let Some(id) = lock(&self.inner.bus_subscription).take() {
            self.inner.server.bus().unsubscribe(id);
        }
        let waves = std::mem::take(&mut *lock(&self.inner.waves));
        for subscription in waves.into_values().flatten() {
            subscription.closed.store(true, Ordering::SeqCst);
        }
    }

    /// Whether `participant` may read the wavelet: it exists, the policy
    /// allows it and the participant has access. The check for every read
    /// on a participant's behalf.
    pub async fn check_access(
        &self,
        participant: &ParticipantId,
        wavelet_name: &WaveletName,
    ) -> Result<bool, crate::waveserver::WaveServerError> {
        if !self.inner.policy.may_access(participant, wavelet_name) {
            return Ok(false);
        }
        self.inner
            .server
            .check_access_permission(wavelet_name, Some(participant))
            .await
    }

    /// Opens a view of a wave for `participant`
    /// (`ClientFrontendImpl.openRequest`). The sink first receives, for
    /// each wavelet in view, a snapshot with the committed version; or, if
    /// none is in view, a message carrying only the channel id. Then comes
    /// the marker, then deltas and commit notices as they happen.
    ///
    /// For a wavelet listed in `known_wavelets` at a version that is part
    /// of its history, the snapshot is replaced by a message without
    /// content whose `resync_version` is that version, followed by the
    /// deltas since, if any. A known version the history does not contain
    /// gets a snapshot like an unknown wavelet.
    ///
    /// The subscription lasts until the returned handle is dropped.
    pub async fn open(
        &self,
        participant: &ParticipantId,
        wave_id: &WaveId,
        filter: IdFilter,
        known_wavelets: &[KnownWavelet],
        sink: Box<dyn UpdateSink>,
    ) -> Result<SubscriptionHandle, OpenError> {
        let inner = &self.inner;
        let channel_id = format!("ch{}", CHANNEL_COUNTER.fetch_add(1, Ordering::SeqCst) + 1);
        tracing::debug!(
            %participant, wave = ?wave_id, %filter, known = known_wavelets.len(), %channel_id,
            "open request"
        );
        let subscription = Arc::new(Subscription {
            id: inner.next_subscription.fetch_add(1, Ordering::Relaxed),
            participant: participant.clone(),
            wave_id: wave_id.clone(),
            filter,
            channel_id,
            sink,
            state: Mutex::new(SubscriptionState {
                opening: Some(Vec::new()),
                channels: HashMap::new(),
            }),
            closed: AtomicBool::new(false),
        });
        // Subscribe first, so that nothing that happens from here on is
        // missed; it queues up until the snapshots are out.
        lock(&inner.waves)
            .entry(wave_id.clone())
            .or_default()
            .push(Arc::clone(&subscription));
        let handle = SubscriptionHandle {
            inner: Arc::downgrade(inner),
            subscription: Arc::clone(&subscription),
        };

        let wavelet_ids = inner
            .server
            .get_wavelet_ids(wave_id)
            .await
            .map_err(|error| {
                tracing::error!(wave = ?wave_id, %error, "Wave server failed lookup");
                OpenError::Lookup
            })?;
        let mut in_view = 0;
        for wavelet_id in wavelet_ids {
            if !subscription.includes(&wavelet_id) {
                continue;
            }
            let wavelet_name = WaveletName::of(wave_id.clone(), wavelet_id);
            // A failed access check hides the wavelet, as in the original.
            if !self
                .check_access(participant, &wavelet_name)
                .await
                .unwrap_or(false)
            {
                continue;
            }
            let snapshot = inner
                .server
                .get_snapshot(&wavelet_name)
                .await
                .map_err(|error| {
                    tracing::warn!(wavelet = %wavelet_name, %error, "Failed to retrieve snapshot");
                    OpenError::Snapshot
                })?;
            let Some(snapshot) = snapshot else {
                continue;
            };
            // Access may have been lost since the check.
            if !sees(participant, snapshot.snapshot.participants()) {
                continue;
            }
            let known = known_wavelets
                .iter()
                .find(|known| known.wavelet_id == wavelet_name.wavelet_id);
            let current = snapshot.snapshot.hashed_version().clone();
            let resync = match known {
                Some(known) if known.version == current => Some(Vec::new()),
                Some(known) if known.version.version() < current.version() => inner
                    .server
                    .get_history_records(&wavelet_name, &known.version, &current, None)
                    .await
                    .ok()
                    .filter(|deltas| {
                        deltas
                            .first()
                            .is_some_and(|d| d.applied_at_version == known.version)
                    }),
                _ => None,
            };
            let base = WaveViewUpdate {
                committed_version: Some(snapshot.committed_version.clone()),
                ..subscription.update_for(&wavelet_name)
            };
            subscription.sink.ready().await;
            let delivered = match (resync, known) {
                (Some(deltas), Some(known)) => {
                    subscription.send(WaveViewUpdate {
                        resync_version: Some(known.version.clone()),
                        ..base
                    }) && (deltas.is_empty()
                        || subscription.send(WaveViewUpdate {
                            deltas,
                            ..subscription.update_for(&wavelet_name)
                        }))
                }
                _ => subscription.send(WaveViewUpdate {
                    snapshot: Some(Arc::new(snapshot.snapshot)),
                    ..base
                }),
            };
            if !delivered {
                return Err(OpenError::Overloaded);
            }
            lock(&subscription.state)
                .channels
                .entry(wavelet_name.wavelet_id.clone())
                .or_default()
                .last_version = Some(current.version());
            in_view += 1;
        }

        let dummy = dummy_wavelet_name(wave_id);
        if in_view == 0 {
            // Just the channel id.
            subscription.send(subscription.update_for(&dummy));
        }
        subscription.sink.ready().await;
        // The marker, and with it everything that queued up meanwhile.
        let mut state = lock(&subscription.state);
        let marker_sent = subscription.send(WaveViewUpdate {
            marker: Some(true),
            ..WaveViewUpdate::new(dummy)
        });
        if !marker_sent {
            return Err(OpenError::Overloaded);
        }
        for event in state.opening.take().unwrap_or_default() {
            match event {
                Pending::Update {
                    wavelet_name,
                    deltas,
                    participants,
                } => subscription.on_update(
                    &mut state,
                    inner.policy.as_ref(),
                    &wavelet_name,
                    &deltas,
                    &participants,
                    None,
                ),
                Pending::Commit {
                    wavelet_name,
                    version,
                } => subscription.on_commit(&mut state, &wavelet_name, &version),
                Pending::Reset {
                    wavelet_name,
                    version,
                    rewound,
                } => subscription.on_reset(&mut state, &wavelet_name, &version, &rewound),
            }
        }
        drop(state);
        Ok(handle)
    }

    /// [`ClientFrontend::open`] delivering to a channel of `capacity`
    /// messages. If the receiver falls that far behind, the subscription
    /// ends and the channel closes.
    pub async fn open_channel(
        &self,
        participant: &ParticipantId,
        wave_id: &WaveId,
        filter: IdFilter,
        known_wavelets: &[KnownWavelet],
        capacity: usize,
    ) -> Result<(SubscriptionHandle, mpsc::Receiver<WaveViewEvent>), OpenError> {
        let (sender, receiver) = mpsc::channel(capacity.max(1));
        let handle = self
            .open(
                participant,
                wave_id,
                filter,
                known_wavelets,
                Box::new(ChannelSink(sender)),
            )
            .await?;
        Ok((handle, receiver))
    }

    fn find_subscription(
        &self,
        participant: &ParticipantId,
        wavelet_name: &WaveletName,
        channel_id: Option<&str>,
    ) -> Option<Arc<Subscription>> {
        // `UserManager.findSubscription`.
        let channel_id = channel_id?;
        self.inner
            .subscriptions_of(&wavelet_name.wave_id)
            .into_iter()
            .find(|subscription| {
                subscription.participant == *participant
                    && subscription.channel_id == channel_id
                    && subscription.includes(&wavelet_name.wavelet_id)
            })
    }

    /// Submits a delta for `participant`, who must be its author
    /// (`ClientFrontendImpl.submitRequest`). `channel_id` names the
    /// participant's subscription the delta comes from, which then does
    /// not receive it back. `respond` is called with the outcome (an error
    /// is the message for the client) before any deltas that were held
    /// back during the submission are released to that subscription.
    pub async fn submit_request(
        &self,
        participant: &ParticipantId,
        wavelet_name: &WaveletName,
        delta: ProtocolWaveletDelta,
        channel_id: Option<&str>,
        mut respond: impl FnMut(&Result<SubmitResult, String>) + Send + 'static,
    ) -> Result<SubmitResult, String> {
        let inner = &self.inner;
        inner.submits.fetch_add(1, Ordering::Relaxed);
        let rejected = if ParticipantId::new_unchecked(delta.author.as_deref().unwrap_or_default())
            != *participant
        {
            Some(SubmitError::AuthorMismatch.to_string())
        } else {
            inner.policy.may_submit(participant, wavelet_name).err()
        };
        if let Some(error) = rejected {
            inner.submit_errors.fetch_add(1, Ordering::Relaxed);
            let result = Err(error);
            respond(&result);
            return result;
        }
        let subscription = self.find_subscription(participant, wavelet_name, channel_id);
        if let Some(subscription) = &subscription {
            subscription.submit_started(&wavelet_name.wavelet_id);
        }
        let result = inner
            .server
            .submit_request_as(participant, wavelet_name, delta)
            .await
            .map_err(|error| {
                inner.submit_errors.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(wavelet = %wavelet_name, %participant, %error, "submit failed");
                error.to_string()
            });
        respond(&result);
        if let Ok(SubmitResult {
            pending: Some(seq), ..
        }) = &result
        {
            // The host cannot be reached: the delta waits here. The
            // client was told so; it hears the final answer (and its
            // channel resumes) once the host applied it.
            let seq = *seq;
            let server = inner.server.clone();
            let wavelet_name = wavelet_name.clone();
            let inner = Arc::clone(inner);
            tokio::spawn(async move {
                let result = server.wait_pending(seq).await;
                if result.is_err() {
                    inner.submit_errors.fetch_add(1, Ordering::Relaxed);
                }
                respond(&result);
                if let Some(subscription) = &subscription {
                    subscription.submit_finished(
                        &wavelet_name,
                        result
                            .as_ref()
                            .ok()
                            .map(|result| result.hashed_version_after_application.version()),
                    );
                }
            });
            return result;
        }
        if let Some(subscription) = &subscription {
            subscription.submit_finished(
                wavelet_name,
                result
                    .as_ref()
                    .ok()
                    .map(|result| result.hashed_version_after_application.version()),
            );
        }
        result
    }
}
