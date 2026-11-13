//! Tests of the client front end.
//!
//! Ports `UserManagerTest` and `ClientFrontendImplTest` of
//! `org.waveprotocol.box.server.frontend` in Apache Wave (the former
//! against a bare subscription, the latter against a real wave server
//! instead of a mocked wavelet provider), and adds tests of what this
//! front end does beyond the original.

use std::sync::Arc;
use std::time::Duration;

use resonator_model::docop::DocOpBuilder;
use resonator_model::id::IdFilter;
use resonator_model::operation::{
    NO_TIMESTAMP, TransformedWaveletDelta, WaveletDelta, WaveletOperation, WaveletOperationContext,
};
use resonator_model::proto::serialize_delta;
use tempfile::TempDir;

use super::*;
use crate::storage::{Database, DatabaseOptions};
use crate::waveserver::WaveServerOptions;

const DOMAIN: &str = "example.com";

fn user(name: &str) -> ParticipantId {
    ParticipantId::of(&format!("{name}@{DOMAIN}")).unwrap()
}

fn wave_id() -> WaveId {
    WaveId::of(DOMAIN, "w+abc").unwrap()
}

fn wavelet_id(id: &str) -> WaveletId {
    WaveletId::of(DOMAIN, id).unwrap()
}

fn name(id: &str) -> WaveletName {
    WaveletName::of(wave_id(), wavelet_id(id))
}

fn context(author: &ParticipantId) -> WaveletOperationContext {
    WaveletOperationContext::new(author.clone(), NO_TIMESTAMP, 1)
}

fn add(author: &ParticipantId, who: &ParticipantId) -> WaveletOperation {
    WaveletOperation::add_participant(context(author), who.clone())
}

fn remove(author: &ParticipantId, who: &ParticipantId) -> WaveletOperation {
    WaveletOperation::remove_participant(context(author), who.clone())
}

fn no_op(author: &ParticipantId) -> WaveletOperation {
    WaveletOperation::no_op(context(author))
}

fn append(author: &ParticipantId, blip: &str, size: usize, text: &str) -> WaveletOperation {
    let mut builder = DocOpBuilder::new();
    if size > 0 {
        builder = builder.retain(size);
    }
    let op = builder.characters(text).build().unwrap();
    WaveletOperation::blip_content(context(author), blip, op)
}

/// Records everything a subscription is sent.
#[derive(Clone, Default)]
struct Recorder {
    events: Arc<Mutex<Vec<WaveViewEvent>>>,
    /// Refuses messages once this many were accepted.
    capacity: Option<usize>,
    /// Understands wavelet resets.
    resets: bool,
}

impl UpdateSink for Recorder {
    fn update(&self, update: WaveViewUpdate) -> bool {
        let mut events = lock(&self.events);
        if self
            .capacity
            .is_some_and(|capacity| events.len() >= capacity)
        {
            return false;
        }
        events.push(WaveViewEvent::Update(update));
        true
    }

    fn failed(&self, error: &str) {
        lock(&self.events).push(WaveViewEvent::Failed(error.to_owned()));
    }

    fn accepts_resets(&self) -> bool {
        self.resets
    }
}

impl Recorder {
    /// Everything received since the last call.
    fn take(&self) -> Vec<WaveViewEvent> {
        std::mem::take(&mut *lock(&self.events))
    }

    /// The updates received since the last call, without bare commit
    /// notices (whose timing depends on the database writer).
    fn updates(&self) -> Vec<WaveViewUpdate> {
        self.take()
            .into_iter()
            .map(|event| match event {
                WaveViewEvent::Update(update) => update,
                WaveViewEvent::Failed(error) => panic!("subscription failed: {error}"),
            })
            .filter(|update| !is_bare_commit(update))
            .collect()
    }
}

fn is_bare_commit(update: &WaveViewUpdate) -> bool {
    update.committed_version.is_some()
        && update.snapshot.is_none()
        && update.deltas.is_empty()
        && update.resync_version.is_none()
}

fn is_marker(update: &WaveViewUpdate) -> bool {
    update.marker == Some(true)
        && update.channel_id.is_none()
        && update.snapshot.is_none()
        && update.deltas.is_empty()
        && update.committed_version.is_none()
        && update.wavelet_name == dummy_wavelet_name(&update.wavelet_name.wave_id)
}

/// The versions `(applied at, resulting)` of an update's deltas.
fn versions(update: &WaveViewUpdate) -> Vec<(u64, u64)> {
    update
        .deltas
        .iter()
        .map(|delta| {
            (
                delta.applied_at_version.version(),
                delta.resulting_version().version(),
            )
        })
        .collect()
}

struct Fixture {
    _dir: TempDir,
    server: WaveServer,
    frontend: ClientFrontend,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("test.db"), DatabaseOptions::default()).unwrap();
        let server = WaveServer::new(db, WaveServerOptions::new(DOMAIN));
        let frontend = ClientFrontend::new(server.clone());
        Fixture {
            _dir: dir,
            server,
            frontend,
        }
    }

    async fn open_with(
        &self,
        who: &ParticipantId,
        filter: IdFilter,
        known: &[KnownWavelet],
    ) -> (SubscriptionHandle, Recorder) {
        let recorder = Recorder::default();
        let handle = self
            .frontend
            .open(who, &wave_id(), filter, known, Box::new(recorder.clone()))
            .await
            .unwrap();
        (handle, recorder)
    }

    async fn open(&self, who: &ParticipantId) -> (SubscriptionHandle, Recorder) {
        self.open_with(who, IdFilter::all_ids(), &[]).await
    }

    async fn version(&self, wavelet: &WaveletName) -> HashedVersion {
        match self.server.get_versions(wavelet).await.unwrap() {
            Some((current, _)) => current,
            None => self.server.version_zero(wavelet),
        }
    }

    /// Submits operations at the wavelet's current version.
    async fn submit(
        &self,
        author: &ParticipantId,
        wavelet: &WaveletName,
        ops: Vec<WaveletOperation>,
        channel_id: Option<&str>,
    ) -> Result<SubmitResult, String> {
        let target = self.version(wavelet).await;
        let delta = serialize_delta(&WaveletDelta::new(author.clone(), target, ops)).unwrap();
        self.frontend
            .submit_request(author, wavelet, delta, channel_id, |_| {})
            .await
    }

    /// Creates a wavelet with `creator` as its participant.
    async fn create(&self, creator: &ParticipantId, wavelet: &WaveletName) {
        self.submit(creator, wavelet, vec![add(creator, creator)], None)
            .await
            .unwrap();
    }

    async fn committed(&self, wavelet: &WaveletName) -> HashedVersion {
        let version = self.version(wavelet).await;
        self.server
            .wait_committed(wavelet, version.version())
            .await
            .unwrap()
    }
}

// ---- UserManagerTest, against a bare subscription ----

fn bare_subscription(filter: IdFilter, channel_id: &str) -> (Arc<Subscription>, Recorder) {
    let recorder = Recorder::default();
    let subscription = Arc::new(Subscription {
        id: 0,
        participant: user("user"),
        wave_id: wave_id(),
        filter,
        channel_id: channel_id.to_owned(),
        sink: Box::new(recorder.clone()),
        state: Mutex::new(SubscriptionState {
            opening: None,
            channels: HashMap::new(),
        }),
        closed: AtomicBool::new(false),
    });
    (subscription, recorder)
}

/// A delta of no-ops from `start` to `end`.
fn record(start: u64, end: u64) -> Arc<WaveletDeltaRecord> {
    let author = user("user");
    let ops: Vec<_> = (start..end).map(|_| no_op(&author)).collect();
    let transformed =
        TransformedWaveletDelta::clone_operations(author, HashedVersion::unsigned(end), 0, &ops)
            .unwrap();
    Arc::new(WaveletDeltaRecord::new(
        HashedVersion::unsigned(start),
        None,
        transformed,
    ))
}

fn deliver(subscription: &Subscription, wavelet: &WaveletName, deltas: &[Arc<WaveletDeltaRecord>]) {
    let mut state = lock(&subscription.state);
    subscription.on_update(
        &mut state,
        &DefaultPolicy,
        wavelet,
        deltas,
        &[user("user")],
        None,
    );
}

#[test]
fn match_subscriptions() {
    // `UserManagerTest.testMatchSubscriptions`: which filters include
    // which wavelets.
    let wa = wavelet_id("A");
    let wb = wavelet_id("B");
    let filters = [
        IdFilter::of_ids([wa.clone()]),
        IdFilter::all_ids(),
        IdFilter::no_ids(),
        IdFilter::of_prefixes(["A", "B"]),
    ];
    let includes = |id: &WaveletId| -> Vec<bool> {
        filters
            .iter()
            .map(|filter| bare_subscription(filter.clone(), "").0.includes(id))
            .collect()
    };
    assert_eq!(includes(&wa), [true, true, false, true]);
    assert_eq!(includes(&wb), [false, true, false, true]);
}

#[test]
fn empty_delta_not_received() {
    let (subscription, recorder) = bare_subscription(IdFilter::all_ids(), "ch");
    deliver(&subscription, &name("A"), &[]);
    assert!(recorder.take().is_empty());
}

#[test]
fn single_delta_received() {
    let (subscription, recorder) = bare_subscription(IdFilter::all_ids(), "ch");
    let delta = record(0, 2);
    deliver(&subscription, &name("A"), std::slice::from_ref(&delta));
    let updates = recorder.updates();
    assert_eq!(updates.len(), 1);
    assert_eq!(
        updates[0],
        WaveViewUpdate {
            deltas: vec![delta],
            channel_id: Some("ch".to_owned()),
            ..WaveViewUpdate::new(name("A"))
        }
    );
}

#[test]
fn update_several_deltas() {
    let (subscription, recorder) = bare_subscription(IdFilter::all_ids(), "ch1");
    let (delta, delta2) = (record(0, 2), record(2, 3));
    deliver(&subscription, &name("A"), &[delta.clone(), delta2.clone()]);
    let updates = recorder.updates();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].deltas, [delta.clone(), delta2.clone()]);

    // One at a time, on another wavelet.
    deliver(&subscription, &name("B"), std::slice::from_ref(&delta));
    deliver(&subscription, &name("B"), std::slice::from_ref(&delta2));
    let updates = recorder.updates();
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0].deltas, [delta]);
    assert_eq!(updates[1].deltas, [delta2]);
    assert!(updates.iter().all(|u| u.wavelet_name == name("B")));
}

#[test]
fn delta_held_back_while_outstanding_submit() {
    let (subscription, recorder) = bare_subscription(IdFilter::all_ids(), "ch");
    let delta = record(0, 2);
    subscription.submit_started(&wavelet_id("A"));
    deliver(&subscription, &name("A"), std::slice::from_ref(&delta));
    assert!(recorder.take().is_empty());
    // Version 3 is not the update's version, so the delta is another's.
    subscription.submit_finished(&name("A"), Some(3));
    let updates = recorder.updates();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].deltas, [delta]);
}

#[test]
fn own_deltas_are_dropped() {
    let (subscription, recorder) = bare_subscription(IdFilter::all_ids(), "ch");
    subscription.submit_started(&wavelet_id("A"));
    subscription.submit_finished(&name("A"), Some(2));
    deliver(&subscription, &name("A"), &[record(0, 2)]);
    assert!(recorder.take().is_empty());
    // Only once: the next delta is delivered.
    deliver(&subscription, &name("A"), &[record(2, 3)]);
    assert_eq!(versions(&recorder.updates()[0]), [(2, 3)]);
}

#[test]
fn own_delta_dropped_after_being_held_back() {
    let (subscription, recorder) = bare_subscription(IdFilter::all_ids(), "ch");
    subscription.submit_started(&wavelet_id("A"));
    deliver(&subscription, &name("A"), &[record(0, 2)]);
    subscription.submit_finished(&name("A"), Some(2));
    assert!(recorder.take().is_empty());
}

fn reset(
    subscription: &Subscription,
    wavelet: &WaveletName,
    version: u64,
    rewound: &[Arc<WaveletDeltaRecord>],
) {
    let mut state = lock(&subscription.state);
    subscription.on_reset(
        &mut state,
        wavelet,
        &HashedVersion::unsigned(version),
        rewound,
    );
}

fn resetting_subscription() -> (Arc<Subscription>, Recorder) {
    let recorder = Recorder {
        resets: true,
        ..Recorder::default()
    };
    let (mut subscription, _) = bare_subscription(IdFilter::all_ids(), "ch");
    Arc::get_mut(&mut subscription).unwrap().sink = Box::new(recorder.clone());
    (subscription, recorder)
}

#[test]
fn a_rewind_sends_the_deltas_taken_back_and_continues_from_the_fork() {
    let (subscription, recorder) = resetting_subscription();
    let rewound = [record(2, 3), record(3, 5)];
    deliver(&subscription, &name("A"), &[record(0, 2)]);
    deliver(&subscription, &name("A"), &rewound);
    recorder.take();
    reset(&subscription, &name("A"), 2, &rewound);
    let updates = recorder.updates();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].reset_version, Some(HashedVersion::unsigned(2)));
    assert_eq!(updates[0].deltas, rewound);
    assert_eq!(
        updates[0].resulting_version(),
        Some(&HashedVersion::unsigned(2))
    );
    let proto = updates[0].to_proto().unwrap();
    assert_eq!(proto.reset_version.unwrap().version, Some(2));
    // The new history goes on from the fork.
    deliver(&subscription, &name("A"), &[record(2, 4)]);
    assert_eq!(versions(&recorder.updates()[0]), [(2, 4)]);
    // A rewind behind what the client has seen changes nothing.
    reset(&subscription, &name("A"), 4, &[]);
    assert!(recorder.take().is_empty());
}

#[test]
fn a_rewind_fails_a_subscription_that_cannot_follow_it() {
    let (subscription, recorder) = bare_subscription(IdFilter::all_ids(), "ch");
    let rewound = [record(2, 3)];
    deliver(&subscription, &name("A"), &[record(0, 2)]);
    deliver(&subscription, &name("A"), &rewound);
    recorder.take();
    reset(&subscription, &name("A"), 2, &rewound);
    assert!(matches!(recorder.take()[..], [WaveViewEvent::Failed(_)]));
}

#[test]
fn a_rewind_waits_for_an_outstanding_submit() {
    let (subscription, recorder) = resetting_subscription();
    deliver(&subscription, &name("A"), &[record(0, 2), record(2, 3)]);
    recorder.take();
    subscription.submit_started(&wavelet_id("A"));
    // Held back, then taken out again.
    deliver(&subscription, &name("A"), &[record(3, 4)]);
    reset(&subscription, &name("A"), 2, &[record(2, 3), record(3, 4)]);
    assert!(recorder.take().is_empty());
    subscription.submit_finished(&name("A"), Some(5));
    // What was held back goes out first, then everything goes back.
    let updates = recorder.updates();
    assert_eq!(updates.len(), 2);
    assert_eq!(versions(&updates[0]), [(3, 4)]);
    assert_eq!(updates[1].reset_version, Some(HashedVersion::unsigned(2)));
    assert_eq!(versions(&updates[1]), [(2, 3), (3, 4)]);
}

#[test]
fn own_deltas_go_back_too() {
    let (subscription, recorder) = resetting_subscription();
    deliver(&subscription, &name("A"), &[record(0, 2)]);
    recorder.take();
    subscription.submit_started(&wavelet_id("A"));
    deliver(&subscription, &name("A"), &[record(2, 3)]);
    reset(&subscription, &name("A"), 2, &[record(2, 3)]);
    subscription.submit_finished(&name("A"), Some(3));
    let updates = recorder.updates();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].reset_version, Some(HashedVersion::unsigned(2)));
    assert_eq!(versions(&updates[0]), [(2, 3)]);
}

#[test]
fn failed_submit_releases_held_back_deltas() {
    // The original trips over its own null check here and holds the
    // deltas back for ever.
    let (subscription, recorder) = bare_subscription(IdFilter::all_ids(), "ch");
    subscription.submit_started(&wavelet_id("A"));
    deliver(&subscription, &name("A"), &[record(0, 2)]);
    subscription.submit_finished(&name("A"), None);
    assert_eq!(versions(&recorder.updates()[0]), [(0, 2)]);
}

#[test]
fn overlapping_submits_hold_back_until_the_last_response() {
    let (subscription, recorder) = bare_subscription(IdFilter::all_ids(), "ch");
    subscription.submit_started(&wavelet_id("A"));
    subscription.submit_started(&wavelet_id("A"));
    deliver(&subscription, &name("A"), &[record(0, 1), record(1, 2)]);
    deliver(&subscription, &name("A"), &[record(2, 3)]);
    subscription.submit_finished(&name("A"), Some(1));
    assert!(recorder.take().is_empty());
    subscription.submit_finished(&name("A"), Some(3));
    // Own deltas (ending at 1 and 3) are dropped, the other is delivered.
    let updates = recorder.updates();
    assert_eq!(updates.len(), 1);
    assert_eq!(versions(&updates[0]), [(1, 2)]);
}

#[test]
fn a_gap_in_the_deltas_fails_the_subscription() {
    // `WaveViewSubscription.checkUpdateVersion`.
    let (subscription, recorder) = bare_subscription(IdFilter::all_ids(), "ch");
    deliver(&subscription, &name("A"), &[record(0, 2)]);
    recorder.take();
    deliver(&subscription, &name("A"), &[record(3, 4)]);
    assert!(matches!(
        recorder.take().as_slice(),
        [WaveViewEvent::Failed(_)]
    ));
    assert!(subscription.is_closed());
    // Nothing more is delivered.
    deliver(&subscription, &name("A"), &[record(2, 3)]);
    assert!(recorder.take().is_empty());
}

#[test]
fn deltas_already_covered_are_skipped() {
    let (subscription, recorder) = bare_subscription(IdFilter::all_ids(), "ch");
    lock(&subscription.state)
        .channels
        .entry(wavelet_id("A"))
        .or_default()
        .last_version = Some(2);
    deliver(&subscription, &name("A"), &[record(0, 1), record(1, 2)]);
    assert!(recorder.take().is_empty());
    deliver(&subscription, &name("A"), &[record(1, 2), record(2, 3)]);
    assert_eq!(versions(&recorder.updates()[0]), [(2, 3)]);
}

// ---- ClientFrontendImplTest, against a real wave server ----

#[tokio::test]
async fn open_empty_wave_receives_channel_id_and_marker() {
    let fx = Fixture::new();
    let (handle, recorder) = fx.open(&user("user")).await;
    let updates = recorder.updates();
    assert_eq!(updates.len(), 2);
    // Just a channel id, on the dummy wavelet.
    assert_eq!(
        updates[0],
        WaveViewUpdate {
            channel_id: Some(handle.channel_id().to_owned()),
            ..WaveViewUpdate::new(dummy_wavelet_name(&wave_id()))
        }
    );
    assert!(handle.channel_id().starts_with("ch"));
    assert!(is_marker(&updates[1]));
    assert_eq!(
        dummy_wavelet_name(&wave_id()).wavelet_id,
        wavelet_id("dummy+root")
    );
}

#[tokio::test]
async fn two_subscriptions_receive_different_channel_ids() {
    let fx = Fixture::new();
    let (first, _) = fx.open(&user("user")).await;
    let (second, _) = fx.open(&user("user")).await;
    assert_ne!(first.channel_id(), second.channel_id());
    assert_eq!(fx.frontend.subscription_count(), 2);
}

#[tokio::test]
async fn open_wave_receives_snapshots_then_marker() {
    let fx = Fixture::new();
    let alice = user("alice");
    fx.create(&alice, &name("conv+root")).await;
    fx.create(&alice, &name("conv+other")).await;
    fx.submit(
        &alice,
        &name("conv+root"),
        vec![append(&alice, "b+1", 0, "hello")],
        None,
    )
    .await
    .unwrap();
    let committed = fx.committed(&name("conv+root")).await;
    fx.committed(&name("conv+other")).await;

    let (handle, recorder) = fx.open(&alice).await;
    let updates = recorder.updates();
    assert_eq!(updates.len(), 3);
    for update in &updates[..2] {
        let snapshot = update.snapshot.as_ref().expect("snapshot");
        assert_eq!(update.wavelet_name, snapshot.wavelet_name());
        assert!(update.deltas.is_empty());
        assert_eq!(update.marker, None);
        assert_eq!(update.channel_id.as_deref(), Some(handle.channel_id()));
        assert_eq!(update.resulting_version(), Some(snapshot.hashed_version()));
        assert_eq!(snapshot.participants(), std::slice::from_ref(&alice));
    }
    let root = updates
        .iter()
        .find(|update| update.wavelet_name == name("conv+root"))
        .unwrap();
    assert_eq!(root.committed_version.as_ref(), Some(&committed));
    assert_eq!(root.snapshot.as_ref().unwrap().version(), 2);
    assert!(is_marker(&updates[2]));
}

#[tokio::test]
async fn unsubscribed_snapshot_not_received() {
    let fx = Fixture::new();
    let alice = user("alice");
    fx.create(&alice, &name("conv+root")).await;
    let (_handle, recorder) = fx
        .open_with(&alice, IdFilter::of_prefixes(["non-existing"]), &[])
        .await;
    let updates = recorder.updates();
    assert_eq!(updates.len(), 2);
    assert!(updates[0].snapshot.is_none() && updates[0].channel_id.is_some());
    assert!(is_marker(&updates[1]));
    // Nor are its deltas.
    fx.submit(&alice, &name("conv+root"), vec![no_op(&alice)], None)
        .await
        .unwrap();
    fx.committed(&name("conv+root")).await;
    assert!(recorder.take().is_empty());
}

#[tokio::test]
async fn received_deltas_sent_to_client() {
    let fx = Fixture::new();
    let alice = user("alice");
    fx.create(&alice, &name("conv+root")).await;
    let (handle, recorder) = fx.open(&alice).await;
    recorder.updates();
    let result = fx
        .submit(&alice, &name("conv+root"), vec![no_op(&alice)], None)
        .await
        .unwrap();
    let updates = recorder.updates();
    assert_eq!(updates.len(), 1);
    let update = &updates[0];
    assert_eq!(update.wavelet_name, name("conv+root"));
    assert!(update.snapshot.is_none());
    assert_eq!(update.marker, None);
    assert_eq!(update.committed_version, None);
    assert_eq!(update.channel_id.as_deref(), Some(handle.channel_id()));
    assert_eq!(versions(update), [(1, 2)]);
    assert_eq!(
        update.resulting_version(),
        Some(&result.hashed_version_after_application)
    );
}

#[tokio::test]
async fn submit_is_forwarded_to_the_resonator_server() {
    let fx = Fixture::new();
    let alice = user("alice");
    let (handle, _) = fx.open(&alice).await;
    let responded = Arc::new(std::sync::Mutex::new(None));
    let wavelet = name("conv+root");
    let delta = serialize_delta(&WaveletDelta::new(
        alice.clone(),
        fx.server.version_zero(&wavelet),
        vec![add(&alice, &alice)],
    ))
    .unwrap();
    let result = fx
        .frontend
        .submit_request(&alice, &wavelet, delta, Some(handle.channel_id()), {
            let responded = Arc::clone(&responded);
            move |result| {
                *responded.lock().unwrap() = Some(result.clone());
            }
        })
        .await
        .unwrap();
    assert_eq!(*responded.lock().unwrap(), Some(Ok(result.clone())));
    assert_eq!(result.operations_applied, 1);
    assert_eq!(fx.version(&wavelet).await.version(), 1);
    assert_eq!(fx.frontend.submit_counts(), (1, 0));
}

#[tokio::test]
async fn cannot_submit_as_different_user() {
    let fx = Fixture::new();
    let alice = user("alice");
    let wavelet = name("conv+root");
    let delta = serialize_delta(&WaveletDelta::new(
        alice.clone(),
        fx.server.version_zero(&wavelet),
        vec![add(&alice, &alice)],
    ))
    .unwrap();
    let responded = Arc::new(std::sync::Mutex::new(None));
    let result = fx
        .frontend
        .submit_request(&user("another"), &wavelet, delta, None, {
            let responded = Arc::clone(&responded);
            move |result| {
                *responded.lock().unwrap() = Some(result.clone());
            }
        })
        .await;
    assert_eq!(
        result,
        Err("Author field on delta must match logged in user".to_owned())
    );
    assert_eq!(*responded.lock().unwrap(), Some(result));
    assert_eq!(fx.server.get_versions(&wavelet).await.unwrap(), None);
    assert_eq!(fx.frontend.submit_counts(), (1, 1));
}

// ---- Beyond the original's tests ----

#[tokio::test]
async fn own_submissions_are_not_echoed_to_their_channel() {
    let fx = Fixture::new();
    let alice = user("alice");
    let wavelet = name("conv+root");
    fx.create(&alice, &wavelet).await;
    let (first, first_seen) = fx.open(&alice).await;
    let (_second, second_seen) = fx.open(&alice).await;
    first_seen.updates();
    second_seen.updates();

    fx.submit(
        &alice,
        &wavelet,
        vec![no_op(&alice)],
        Some(first.channel_id()),
    )
    .await
    .unwrap();
    // The submitting channel does not get its delta back; the same user's
    // other view does.
    assert!(first_seen.updates().is_empty());
    assert_eq!(versions(&second_seen.updates()[0]), [(1, 2)]);

    // Without a channel id (or with someone else's) the delta is echoed.
    fx.submit(&alice, &wavelet, vec![no_op(&alice)], None)
        .await
        .unwrap();
    assert_eq!(versions(&first_seen.updates()[0]), [(2, 3)]);
    fx.submit(&alice, &wavelet, vec![no_op(&alice)], Some("ch-nobody"))
        .await
        .unwrap();
    assert_eq!(versions(&first_seen.updates()[0]), [(3, 4)]);
    // And the next delta of someone else's arrives as usual.
    let bob = user("bob");
    fx.submit(
        &alice,
        &wavelet,
        vec![add(&alice, &bob)],
        Some(first.channel_id()),
    )
    .await
    .unwrap();
    fx.submit(&bob, &wavelet, vec![no_op(&bob)], None)
        .await
        .unwrap();
    assert_eq!(versions(&first_seen.updates()[0]), [(5, 6)]);
}

#[tokio::test]
async fn a_channel_id_only_works_for_its_own_user() {
    let fx = Fixture::new();
    let (alice, bob) = (user("alice"), user("bob"));
    let wavelet = name("conv+root");
    fx.submit(
        &alice,
        &wavelet,
        vec![add(&alice, &alice), add(&alice, &bob)],
        None,
    )
    .await
    .unwrap();
    let (alices, alice_seen) = fx.open(&alice).await;
    alice_seen.updates();
    // Bob naming Alice's channel does not hide his delta from her.
    fx.submit(&bob, &wavelet, vec![no_op(&bob)], Some(alices.channel_id()))
        .await
        .unwrap();
    assert_eq!(versions(&alice_seen.updates()[0]), [(2, 3)]);
}

#[tokio::test]
async fn commit_notices_follow_updates() {
    let fx = Fixture::new();
    let alice = user("alice");
    let wavelet = name("conv+root");
    fx.create(&alice, &wavelet).await;
    fx.committed(&wavelet).await;
    let (handle, recorder) = fx.open(&alice).await;
    recorder.take();
    fx.submit(&alice, &wavelet, vec![no_op(&alice)], None)
        .await
        .unwrap();
    let committed = fx.committed(&wavelet).await;
    // The notice is published right after the commit that was waited for.
    let mut events = Vec::new();
    for _ in 0..200 {
        events.extend(recorder.take());
        if events.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let updates: Vec<_> = events
        .into_iter()
        .map(|event| match event {
            WaveViewEvent::Update(update) => update,
            WaveViewEvent::Failed(error) => panic!("{error}"),
        })
        .collect();
    assert_eq!(versions(&updates[0]), [(1, 2)]);
    assert_eq!(
        updates[1],
        WaveViewUpdate {
            committed_version: Some(committed),
            channel_id: Some(handle.channel_id().to_owned()),
            ..WaveViewUpdate::new(wavelet)
        }
    );
}

#[tokio::test]
async fn new_wavelet_comes_into_view_with_its_deltas() {
    let fx = Fixture::new();
    let alice = user("alice");
    let (_handle, recorder) = fx.open(&alice).await;
    recorder.updates();
    fx.create(&alice, &name("conv+root")).await;
    let updates = recorder.updates();
    assert_eq!(updates.len(), 1);
    assert!(updates[0].snapshot.is_none());
    assert_eq!(versions(&updates[0]), [(0, 1)]);
}

#[tokio::test]
async fn added_participant_gets_a_snapshot_and_removed_one_the_removal() {
    let fx = Fixture::new();
    let (alice, bob) = (user("alice"), user("bob"));
    let wavelet = name("conv+root");
    fx.create(&alice, &wavelet).await;
    fx.submit(
        &alice,
        &wavelet,
        vec![append(&alice, "b+1", 0, "secret")],
        None,
    )
    .await
    .unwrap();

    // Bob has the wave open but is no participant: he sees nothing.
    let (_handle, bob_seen) = fx.open(&bob).await;
    let updates = bob_seen.updates();
    assert_eq!(updates.len(), 2);
    assert!(updates[0].snapshot.is_none() && is_marker(&updates[1]));
    fx.submit(&alice, &wavelet, vec![no_op(&alice)], None)
        .await
        .unwrap();
    assert!(bob_seen.take().is_empty());

    // Added: the wavelet comes into view with a snapshot of the state
    // after the delta that added him (the original would send that delta
    // alone, which a client cannot apply to nothing).
    fx.submit(&alice, &wavelet, vec![add(&alice, &bob)], None)
        .await
        .unwrap();
    let updates = bob_seen.updates();
    assert_eq!(updates.len(), 1);
    let snapshot = updates[0].snapshot.as_ref().expect("snapshot");
    assert_eq!(snapshot.version(), 4);
    assert_eq!(snapshot.participants(), [alice.clone(), bob.clone()]);
    assert!(updates[0].deltas.is_empty());
    assert_eq!(updates[0].committed_version, None);

    // From then on, deltas.
    fx.submit(&alice, &wavelet, vec![no_op(&alice)], None)
        .await
        .unwrap();
    assert_eq!(versions(&bob_seen.updates()[0]), [(4, 5)]);

    // Removed: he still gets the delta that removes him, and no more.
    fx.submit(&alice, &wavelet, vec![remove(&alice, &bob)], None)
        .await
        .unwrap();
    assert_eq!(versions(&bob_seen.updates()[0]), [(5, 6)]);
    fx.submit(&alice, &wavelet, vec![no_op(&alice)], None)
        .await
        .unwrap();
    fx.committed(&wavelet).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(bob_seen.take().is_empty());

    // Added again: a fresh snapshot.
    fx.submit(&alice, &wavelet, vec![add(&alice, &bob)], None)
        .await
        .unwrap();
    assert_eq!(
        bob_seen.updates()[0].snapshot.as_ref().unwrap().version(),
        8
    );
}

#[tokio::test]
async fn shared_domain_participant_gives_the_domain_a_live_view() {
    let fx = Fixture::new();
    let (alice, bob) = (user("alice"), user("bob"));
    let everyone = ParticipantId::shared_domain_participant(DOMAIN).unwrap();
    let outsider = ParticipantId::of("eve@other.com").unwrap();
    let wavelet = name("conv+root");
    fx.submit(
        &alice,
        &wavelet,
        vec![add(&alice, &alice), add(&alice, &everyone)],
        None,
    )
    .await
    .unwrap();

    let (_bobs, bob_seen) = fx.open(&bob).await;
    assert!(bob_seen.updates()[0].snapshot.is_some());
    let (_eves, eve_seen) = fx.open(&outsider).await;
    assert!(eve_seen.updates()[0].snapshot.is_none());

    // Live updates reach Bob too (not in the original), but not Eve.
    fx.submit(&alice, &wavelet, vec![no_op(&alice)], None)
        .await
        .unwrap();
    assert_eq!(versions(&bob_seen.updates()[0]), [(2, 3)]);
    assert!(eve_seen.updates().is_empty());

    // Closing the wave to the domain is the last thing Bob sees.
    fx.submit(&alice, &wavelet, vec![remove(&alice, &everyone)], None)
        .await
        .unwrap();
    assert_eq!(versions(&bob_seen.updates()[0]), [(3, 4)]);
    fx.submit(&alice, &wavelet, vec![no_op(&alice)], None)
        .await
        .unwrap();
    assert!(bob_seen.updates().is_empty());
}

#[tokio::test]
async fn known_wavelets_resynchronise_without_a_snapshot() {
    let fx = Fixture::new();
    let alice = user("alice");
    let wavelet = name("conv+root");
    fx.create(&alice, &wavelet).await;
    let v1 = fx.version(&wavelet).await;
    fx.submit(&alice, &wavelet, vec![append(&alice, "b+1", 0, "ab")], None)
        .await
        .unwrap();
    fx.submit(&alice, &wavelet, vec![no_op(&alice), no_op(&alice)], None)
        .await
        .unwrap();
    let current = fx.version(&wavelet).await;
    let committed = fx.committed(&wavelet).await;
    let known = |version: &HashedVersion| {
        [KnownWavelet {
            wavelet_id: wavelet_id("conv+root"),
            version: version.clone(),
        }]
    };

    // Up to date: the resync message alone.
    let (handle, recorder) = fx
        .open_with(&alice, IdFilter::all_ids(), &known(&current))
        .await;
    let updates = recorder.updates();
    assert_eq!(updates.len(), 2);
    assert_eq!(
        updates[0],
        WaveViewUpdate {
            resync_version: Some(current.clone()),
            committed_version: Some(committed.clone()),
            channel_id: Some(handle.channel_id().to_owned()),
            ..WaveViewUpdate::new(wavelet.clone())
        }
    );
    assert_eq!(updates[0].resulting_version(), Some(&current));
    assert!(is_marker(&updates[1]));
    // And live deltas continue from there.
    fx.submit(&alice, &wavelet, vec![no_op(&alice)], None)
        .await
        .unwrap();
    assert_eq!(versions(&recorder.updates()[0]), [(4, 5)]);
    drop(handle);

    // Behind: the resync message, then the deltas since.
    let (_handle, recorder) = fx.open_with(&alice, IdFilter::all_ids(), &known(&v1)).await;
    let updates = recorder.updates();
    assert_eq!(updates.len(), 3);
    assert_eq!(updates[0].resync_version.as_ref(), Some(&v1));
    assert!(updates[0].snapshot.is_none() && updates[0].deltas.is_empty());
    assert!(updates[1].snapshot.is_none());
    assert_eq!(versions(&updates[1]), [(1, 2), (2, 4), (4, 5)]);
    assert!(is_marker(&updates[2]));

    // A version the history does not contain: a snapshot.
    for unknown in [
        HashedVersion::of(1, vec![9; 20]),
        HashedVersion::of(3, current.history_hash().to_vec()),
        HashedVersion::of(99, vec![1; 20]),
    ] {
        let (_handle, recorder) = fx
            .open_with(&alice, IdFilter::all_ids(), &known(&unknown))
            .await;
        let updates = recorder.updates();
        assert_eq!(updates.len(), 2, "{unknown}");
        assert_eq!(updates[0].snapshot.as_ref().unwrap().version(), 5);
        assert_eq!(updates[0].resync_version, None);
    }

    // A known wavelet that is not in view is simply not mentioned.
    let (_handle, recorder) = fx
        .open_with(
            &alice,
            IdFilter::all_ids(),
            &[KnownWavelet {
                wavelet_id: wavelet_id("conv+gone"),
                version: v1.clone(),
            }],
        )
        .await;
    let updates = recorder.updates();
    assert_eq!(updates.len(), 2);
    assert!(updates[0].snapshot.is_some());
}

#[tokio::test]
async fn user_data_wavelets_are_private_to_their_user() {
    let fx = Fixture::new();
    let (alice, bob) = (user("alice"), user("bob"));
    let alices = name("user+alice@example.com");
    // Bob can neither create Alice's user-data wavelet...
    let error = fx
        .submit(&bob, &alices, vec![add(&bob, &bob)], None)
        .await
        .unwrap_err();
    assert!(
        error.contains("may not write the user data wavelet"),
        "{error}"
    );
    assert_eq!(fx.server.get_versions(&alices).await.unwrap(), None);
    // ...nor see it, even as a participant.
    fx.submit(
        &alice,
        &alices,
        vec![add(&alice, &alice), add(&alice, &bob)],
        None,
    )
    .await
    .unwrap();
    assert!(!fx.frontend.check_access(&bob, &alices).await.unwrap());
    assert!(fx.frontend.check_access(&alice, &alices).await.unwrap());
    let (_bobs, bob_seen) = fx.open(&bob).await;
    assert!(bob_seen.updates()[0].snapshot.is_none());
    let (_alices, alice_seen) = fx.open(&alice).await;
    assert!(alice_seen.updates()[0].snapshot.is_some());
    fx.submit(&alice, &alices, vec![no_op(&alice)], None)
        .await
        .unwrap();
    assert!(bob_seen.updates().is_empty());
    assert_eq!(alice_seen.updates().len(), 1);
    // Nor write to it.
    assert!(
        fx.submit(&bob, &alices, vec![no_op(&bob)], None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn dropping_the_handle_ends_the_subscription() {
    let fx = Fixture::new();
    let alice = user("alice");
    let wavelet = name("conv+root");
    fx.create(&alice, &wavelet).await;
    let (handle, recorder) = fx.open(&alice).await;
    assert_eq!(fx.frontend.subscription_count(), 1);
    recorder.take();
    drop(handle);
    assert_eq!(fx.frontend.subscription_count(), 0);
    fx.submit(&alice, &wavelet, vec![no_op(&alice)], None)
        .await
        .unwrap();
    fx.committed(&wavelet).await;
    assert!(recorder.take().is_empty());
}

#[tokio::test]
async fn a_sink_that_cannot_keep_up_ends_the_subscription() {
    let fx = Fixture::new();
    let alice = user("alice");
    let wavelet = name("conv+root");
    fx.create(&alice, &wavelet).await;
    fx.committed(&wavelet).await;
    // Room for the snapshot, the marker and one more message.
    let recorder = Recorder {
        capacity: Some(3),
        ..Recorder::default()
    };
    let handle = fx
        .frontend
        .open(
            &alice,
            &wave_id(),
            IdFilter::all_ids(),
            &[],
            Box::new(recorder.clone()),
        )
        .await
        .unwrap();
    for _ in 0..10 {
        fx.submit(&alice, &wavelet, vec![no_op(&alice)], None)
            .await
            .unwrap();
    }
    assert!(handle.is_closed());
    assert_eq!(recorder.take().len(), 3);

    // If not even the initial messages fit, the open fails.
    let recorder = Recorder {
        capacity: Some(1),
        ..Recorder::default()
    };
    let error = fx
        .frontend
        .open(
            &alice,
            &wave_id(),
            IdFilter::all_ids(),
            &[],
            Box::new(recorder),
        )
        .await
        .unwrap_err();
    assert_eq!(error, OpenError::Overloaded);
    assert_eq!(fx.frontend.subscription_count(), 1);
    drop(handle);
    assert_eq!(fx.frontend.subscription_count(), 0);
}

#[tokio::test]
async fn channel_subscription_for_in_process_participants() {
    let fx = Fixture::new();
    let alice = user("alice");
    let wavelet = name("conv+root");
    let (handle, mut events) = fx
        .frontend
        .open_channel(&alice, &wave_id(), IdFilter::all_ids(), &[], 16)
        .await
        .unwrap();
    let Some(WaveViewEvent::Update(first)) = events.recv().await else {
        panic!("expected the channel id");
    };
    assert_eq!(first.channel_id.as_deref(), Some(handle.channel_id()));
    let Some(WaveViewEvent::Update(marker)) = events.recv().await else {
        panic!("expected the marker");
    };
    assert!(is_marker(&marker));
    fx.create(&alice, &wavelet).await;
    let Some(WaveViewEvent::Update(update)) = events.recv().await else {
        panic!("expected a delta");
    };
    assert_eq!(versions(&update), [(0, 1)]);
    // The wire form of what was received.
    let proto = update.to_proto().unwrap();
    assert_eq!(
        proto.wavelet_name.as_deref(),
        Some("example.com/w+abc/~/conv+root")
    );
    assert_eq!(proto.applied_delta.len(), 1);
    assert_eq!(proto.resulting_version.unwrap().version, Some(1),);
    drop(handle);
}

#[tokio::test]
async fn snapshot_wire_form_follows_the_original() {
    let fx = Fixture::new();
    let alice = user("alice");
    let wavelet = name("conv+root");
    fx.create(&alice, &wavelet).await;
    let committed = fx.committed(&wavelet).await;
    fx.submit(&alice, &wavelet, vec![no_op(&alice)], None)
        .await
        .unwrap();
    let current = fx.version(&wavelet).await;
    // Not waiting for the commit of the second delta: the committed
    // version may or may not have caught up.
    let (_handle, recorder) = fx.open(&alice).await;
    let update = recorder.updates().remove(0);
    let proto = update.to_proto().unwrap();
    let committed_sent = update.committed_version.clone().unwrap();
    assert!(committed_sent == committed || committed_sent == current);
    // `WaveClientRpcImpl`: the snapshot's own version is the committed
    // version, the resulting version that of the state.
    assert_eq!(
        proto.snapshot.as_ref().unwrap().version,
        Some(serialize_hashed_version(&committed_sent).unwrap())
    );
    assert_eq!(
        proto.resulting_version,
        Some(serialize_hashed_version(&current).unwrap())
    );
    assert_eq!(
        proto.commit_notice,
        Some(serialize_hashed_version(&committed_sent).unwrap())
    );
    assert_eq!(proto.channel_id, update.channel_id);
    assert_eq!(proto.marker, None);
    assert!(proto.applied_delta.is_empty());
}

/// Opens views while deltas are being submitted: each view must start from
/// its snapshot and continue with exactly the deltas after it, in order,
/// with the marker in between.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn views_opened_during_updates_have_no_gaps_or_duplicates() {
    let fx = Arc::new(Fixture::new());
    let alice = user("alice");
    let wavelet = name("conv+root");
    fx.create(&alice, &wavelet).await;
    const DELTAS: u64 = 300;

    let writer = {
        let (fx, alice, wavelet) = (Arc::clone(&fx), alice.clone(), wavelet.clone());
        tokio::spawn(async move {
            for _ in 0..DELTAS {
                fx.submit(&alice, &wavelet, vec![no_op(&alice)], None)
                    .await
                    .unwrap();
                tokio::task::yield_now().await;
            }
        })
    };
    let mut views = Vec::new();
    while !writer.is_finished() {
        views.push(fx.open(&alice).await);
        tokio::task::yield_now().await;
    }
    writer.await.unwrap();
    assert!(views.len() > 1, "opened {} views", views.len());

    for (_handle, recorder) in &views {
        let updates = recorder.updates();
        let snapshot = updates[0].snapshot.as_ref().expect("snapshot first");
        assert!(is_marker(&updates[1]), "marker second");
        let mut version = snapshot.version();
        for update in &updates[2..] {
            assert!(update.snapshot.is_none() && update.marker.is_none());
            for (start, end) in versions(update) {
                assert_eq!(start, version, "delta continues the view");
                version = end;
            }
        }
        assert_eq!(version, DELTAS + 1, "view reaches the final version");
    }
}
