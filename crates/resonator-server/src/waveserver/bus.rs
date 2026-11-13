//! The wave bus: subscriptions to wavelet updates and commits.
//!
//! Derived from `WaveBus`, `WaveletNotificationSubscriber` and
//! `WaveletNotificationDispatcher` in
//! `org.waveprotocol.box.server.waveserver` of Apache Wave, licensed under
//! the Apache License, Version 2.0. The federation side of the dispatcher
//! is not ported.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use resonator_model::data::WaveletData;
use resonator_model::id::{ParticipantId, WaveletName};
use resonator_model::version::HashedVersion;
use tokio::sync::mpsc;

use crate::storage::WaveletDeltaRecord;

/// Receives wave bus messages (`WaveBus.Subscriber`).
///
/// Both methods are called synchronously while the wavelet is locked, in
/// the order things happen to that wavelet: an update is delivered before
/// any later update and before the commit notice covering it. They must
/// return quickly and must not call back into the wave server for the same
/// wavelet; hand the work to a channel or task instead (or use
/// [`WaveBus::subscribe_channel`]).
pub trait WaveBusSubscriber: Send + Sync + 'static {
    /// Deltas were applied to a wavelet in memory. `wavelet` is its state
    /// after them. They are not yet durable.
    fn wavelet_update(&self, wavelet: &WaveletData, deltas: &[Arc<WaveletDeltaRecord>]);

    /// The wavelet's history up to `version` is durably stored.
    fn wavelet_committed(&self, wavelet_name: &WaveletName, version: &HashedVersion);

    /// The wavelet's history was rewound to `version`, the start of a
    /// newer host epoch: `rewound` are the deltas taken out, in order.
    /// Deltas announced from now on continue from `version`. Called
    /// without the wavelet's lock.
    fn wavelet_reset(
        &self,
        _wavelet_name: &WaveletName,
        _version: &HashedVersion,
        _rewound: &[Arc<WaveletDeltaRecord>],
    ) {
    }
}

/// A wave bus message for channel subscribers.
#[derive(Debug, Clone)]
pub enum WaveBusEvent {
    Update {
        wavelet_name: WaveletName,
        deltas: Vec<Arc<WaveletDeltaRecord>>,
        /// The wavelet's version after the deltas.
        resulting_version: HashedVersion,
        /// The wavelet's participants after the deltas. A participant
        /// removed by them is not in this list but should still see them.
        participants: Vec<ParticipantId>,
    },
    Committed {
        wavelet_name: WaveletName,
        version: HashedVersion,
    },
}

impl WaveBusEvent {
    pub fn wavelet_name(&self) -> &WaveletName {
        match self {
            WaveBusEvent::Update { wavelet_name, .. }
            | WaveBusEvent::Committed { wavelet_name, .. } => wavelet_name,
        }
    }
}

/// Identifies a subscription for [`WaveBus::unsubscribe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(u64);

struct ChannelSubscriber {
    sender: mpsc::UnboundedSender<WaveBusEvent>,
    closed: AtomicBool,
}

impl ChannelSubscriber {
    fn send(&self, event: WaveBusEvent) {
        if self.sender.send(event).is_err() {
            self.closed.store(true, Ordering::Relaxed);
        }
    }
}

impl WaveBusSubscriber for ChannelSubscriber {
    fn wavelet_update(&self, wavelet: &WaveletData, deltas: &[Arc<WaveletDeltaRecord>]) {
        self.send(WaveBusEvent::Update {
            wavelet_name: wavelet.wavelet_name(),
            deltas: deltas.to_vec(),
            resulting_version: wavelet.hashed_version().clone(),
            participants: wavelet.participants().to_vec(),
        });
    }

    fn wavelet_committed(&self, wavelet_name: &WaveletName, version: &HashedVersion) {
        self.send(WaveBusEvent::Committed {
            wavelet_name: wavelet_name.clone(),
            version: version.clone(),
        });
    }
}

struct Entry {
    id: SubscriptionId,
    subscriber: Arc<dyn WaveBusSubscriber>,
    /// Set for channel subscribers, to drop them once the receiver is gone.
    channel: Option<Arc<ChannelSubscriber>>,
}

/// Distributes wavelet updates and commit notices to subscribers
/// (`WaveBus`). The front end, robot and agent gateways and the search
/// indexer all subscribe here.
#[derive(Default)]
pub struct WaveBus {
    subscribers: RwLock<Vec<Entry>>,
    next_id: AtomicU64,
}

impl std::fmt::Debug for WaveBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaveBus")
            .field("subscribers", &self.subscriber_count())
            .finish()
    }
}

impl WaveBus {
    pub fn new() -> Self {
        Self::default()
    }

    fn add(
        &self,
        subscriber: Arc<dyn WaveBusSubscriber>,
        channel: Option<Arc<ChannelSubscriber>>,
    ) -> SubscriptionId {
        let id = SubscriptionId(self.next_id.fetch_add(1, Ordering::Relaxed));
        self.subscribers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push(Entry {
                id,
                subscriber,
                channel,
            });
        id
    }

    /// Subscribes to all wavelet updates and commits.
    pub fn subscribe(&self, subscriber: Arc<dyn WaveBusSubscriber>) -> SubscriptionId {
        self.add(subscriber, None)
    }

    /// Subscribes with an unbounded channel. Events of one wavelet arrive
    /// in order. Dropping the receiver ends the subscription. The channel
    /// grows without bound if the receiver does not keep up.
    pub fn subscribe_channel(&self) -> (SubscriptionId, mpsc::UnboundedReceiver<WaveBusEvent>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let subscriber = Arc::new(ChannelSubscriber {
            sender,
            closed: AtomicBool::new(false),
        });
        let id = self.add(subscriber.clone(), Some(subscriber));
        (id, receiver)
    }

    /// Ends a subscription; does nothing if it already ended.
    pub fn unsubscribe(&self, id: SubscriptionId) {
        self.subscribers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|entry| entry.id != id);
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    fn each(&self, what: &str, call: impl Fn(&dyn WaveBusSubscriber)) {
        let mut any_closed = false;
        {
            let subscribers = self.subscribers.read().unwrap_or_else(|e| e.into_inner());
            for entry in subscribers.iter() {
                // A panicking subscriber must not take the wavelet down
                // with it (the original swallows runtime exceptions here).
                if catch_unwind(AssertUnwindSafe(|| call(entry.subscriber.as_ref()))).is_err() {
                    tracing::error!("panic in wave bus subscriber during {what}");
                }
                any_closed |= entry
                    .channel
                    .as_ref()
                    .is_some_and(|channel| channel.closed.load(Ordering::Relaxed));
            }
        }
        if any_closed {
            self.subscribers
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|entry| {
                    !entry
                        .channel
                        .as_ref()
                        .is_some_and(|channel| channel.closed.load(Ordering::Relaxed))
                });
        }
    }

    pub(crate) fn publish_update(&self, wavelet: &WaveletData, deltas: &[Arc<WaveletDeltaRecord>]) {
        self.each("update", |subscriber| {
            subscriber.wavelet_update(wavelet, deltas)
        });
    }

    pub(crate) fn publish_reset(
        &self,
        wavelet_name: &WaveletName,
        version: &HashedVersion,
        rewound: &[Arc<WaveletDeltaRecord>],
    ) {
        self.each("reset", |subscriber| {
            subscriber.wavelet_reset(wavelet_name, version, rewound)
        });
    }

    pub(crate) fn publish_committed(&self, wavelet_name: &WaveletName, version: &HashedVersion) {
        self.each("commit", |subscriber| {
            subscriber.wavelet_committed(wavelet_name, version)
        });
    }
}
