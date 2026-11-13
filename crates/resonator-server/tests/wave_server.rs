//! Tests of the wave server: ports of `WaveServerTest` and `WaveMapTest`
//! of Apache Wave where they apply, plus access control, transformation,
//! idempotence, concurrency, restart, snapshots and cache bounds.

mod common;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{
    DOMAIN, TestDb, add_participant, delta, doc_xml, insert, no_op, participant,
    remove_participant, server, server_with, wavelet_name,
};
use resonator_model::data::WaveletData;
use resonator_model::id::{ParticipantId, WaveletName};
use resonator_model::operation::{WaveletOperation, build_wavelet_from_deltas};
use resonator_model::proto::federation::ProtocolSignedDelta;
use resonator_model::proto::{
    build_applied_delta, build_transformed_delta, calculate_resulting_hashed_version, encode,
    get_hashed_version_applied_at,
};
use resonator_model::version::HashedVersion;
use resonator_server::app;
use resonator_server::storage::WaveletDeltaRecord;
use resonator_server::waveserver::{
    SubmitError, SubmitErrorKind, SubmitResult, WaveBusEvent, WaveBusSubscriber, WaveServer,
    WaveServerError, WaveServerOptions, WaveletProvider, WaveletSettings,
};

fn user1() -> ParticipantId {
    participant("user1@example.com")
}

fn user2() -> ParticipantId {
    participant("user2@example.com")
}

fn name() -> WaveletName {
    wavelet_name("abc123", "conv+root")
}

/// Submits a delta by `author` at `target`.
async fn submit(
    server: &WaveServer,
    wavelet: &WaveletName,
    author: &ParticipantId,
    target: &HashedVersion,
    ops: Vec<WaveletOperation>,
) -> Result<SubmitResult, SubmitError> {
    server
        .submit_request(wavelet, delta(author, target, ops))
        .await
}

/// Creates the wavelet with `user1` as its only participant; returns the
/// resulting version.
async fn create(server: &WaveServer, wavelet: &WaveletName) -> HashedVersion {
    let v0 = server.version_zero(wavelet);
    submit(
        server,
        wavelet,
        &user1(),
        &v0,
        vec![add_participant(&user1(), &user1())],
    )
    .await
    .unwrap()
    .hashed_version_after_application
}

async fn data(server: &WaveServer, wavelet: &WaveletName) -> WaveletData {
    server
        .get_snapshot(wavelet)
        .await
        .unwrap()
        .expect("wavelet exists")
        .snapshot
}

fn message(result: Result<SubmitResult, SubmitError>) -> (String, SubmitErrorKind) {
    let error = result.expect_err("submission should fail");
    (error.to_string(), error.kind())
}

/// Records everything the bus says.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<String>>,
}

impl WaveBusSubscriber for Recorder {
    fn wavelet_update(&self, wavelet: &WaveletData, deltas: &[Arc<WaveletDeltaRecord>]) {
        let last = deltas.last().unwrap();
        assert_eq!(wavelet.hashed_version(), last.resulting_version());
        self.events.lock().unwrap().push(format!(
            "update {} {}",
            wavelet.wavelet_name().wave_id.id(),
            wavelet.version()
        ));
    }

    fn wavelet_committed(&self, wavelet_name: &WaveletName, version: &HashedVersion) {
        self.events.lock().unwrap().push(format!(
            "committed {} {}",
            wavelet_name.wave_id.id(),
            version.version()
        ));
    }
}

/// Waits until `condition` holds; panics after five seconds.
async fn eventually(mut condition: impl FnMut() -> bool) {
    for _ in 0..500 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached");
}

// WaveServerTest.

#[tokio::test]
async fn wave_ids_list() {
    let test_db = TestDb::new();
    let (server, _) = server(test_db.open());
    assert!(server.get_wave_ids().await.unwrap().is_empty());
    create(&server, &name()).await;
    // Listed at once, whether or not the delta is committed yet.
    assert_eq!(server.get_wave_ids().await.unwrap(), [name().wave_id]);
    assert_eq!(
        server.get_wavelet_ids(&name().wave_id).await.unwrap(),
        BTreeSet::from([name().wavelet_id])
    );
    server.flush().await.unwrap();
    assert_eq!(server.get_wave_ids().await.unwrap(), [name().wave_id]);
}

#[tokio::test]
async fn wavelet_notification() {
    let test_db = TestDb::new();
    let (server, _) = server(test_db.open());
    let recorder = Arc::new(Recorder::default());
    let subscription = server.bus().subscribe(recorder.clone());
    let v0 = server.version_zero(&name());
    // The first delta may be by anyone and need not add its author.
    submit(
        &server,
        &name(),
        &user1(),
        &v0,
        vec![add_participant(&user1(), &user2())],
    )
    .await
    .unwrap();
    server.wait_committed(&name(), 1).await.unwrap();
    eventually(|| recorder.events.lock().unwrap().len() == 2).await;
    assert_eq!(
        *recorder.events.lock().unwrap(),
        ["update abc123 1", "committed abc123 1"]
    );

    server.bus().unsubscribe(subscription);
    assert_eq!(server.bus().subscriber_count(), 0);
}

// WaveMapTest.

#[tokio::test]
async fn wave_map_starts_empty_and_waves_start_with_no_wavelets() {
    let test_db = TestDb::new();
    let (server, _) = server(test_db.open());
    assert!(server.get_wave_ids().await.unwrap().is_empty());
    assert_eq!(server.loaded_wave_count(), 0);
    assert_eq!(server.get_snapshot(&name()).await.unwrap(), None);
    assert!(
        server
            .get_wavelet_ids(&name().wave_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !server
            .check_access_permission(&name(), Some(&user1()))
            .await
            .unwrap()
    );
    assert_eq!(server.get_versions(&name()).await.unwrap(), None);
    // Asking does not create anything.
    assert_eq!(server.loaded_wave_count(), 0);
}

#[tokio::test]
async fn wavelet_available_after_restart() {
    let test_db = TestDb::new();
    let (first, _) = server(test_db.open());
    let version = create(&first, &name()).await;
    let before = data(&first, &name()).await;
    app::shutdown(first).await;

    let (second, _) = server(test_db.open());
    assert_eq!(second.loaded_wave_count(), 0);
    assert_eq!(second.get_wave_ids().await.unwrap(), [name().wave_id]);
    let snapshot = second.get_snapshot(&name()).await.unwrap().unwrap();
    assert_eq!(snapshot.snapshot, before);
    assert_eq!(snapshot.committed_version, version);
    assert_eq!(second.loaded_wave_count(), 1);
}

#[tokio::test]
async fn failed_first_submit_creates_no_wavelet() {
    let test_db = TestDb::new();
    let (server, _) = server(test_db.open());
    let v0 = server.version_zero(&name());
    let (text, kind) = message(
        submit(
            &server,
            &name(),
            &user1(),
            &v0,
            vec![remove_participant(&user1(), &user2())],
        )
        .await,
    );
    assert!(text.contains("non-existent participant"), "{text}");
    assert_eq!(kind, SubmitErrorKind::BadRequest);
    assert_eq!(server.get_snapshot(&name()).await.unwrap(), None);
    assert!(server.get_wave_ids().await.unwrap().is_empty());
    // The wavelet can still be created.
    create(&server, &name()).await;
}

// Client-visible errors of the submit path.

#[tokio::test]
async fn submit_errors_have_the_original_messages() {
    let test_db = TestDb::new();
    let (server, _) = server(test_db.open());
    let v0 = server.version_zero(&name());
    let v1 = create(&server, &name()).await;

    assert_eq!(
        message(submit(&server, &name(), &user1(), &v1, vec![]).await),
        (
            "Empty delta at version 1".to_owned(),
            SubmitErrorKind::BadRequest
        )
    );

    let mut bad_author = delta(&user1(), &v1, vec![no_op(&user1())]);
    bad_author.author = Some("nobody".to_owned());
    let (text, kind) = message(server.submit_request(&name(), bad_author).await);
    assert!(text.starts_with("Invalid author address: "), "{text}");
    assert_eq!(kind, SubmitErrorKind::BadRequest);

    assert_eq!(
        message(submit(&server, &name(), &user2(), &v1, vec![no_op(&user2())]).await),
        (
            "user2@example.com is not a participant of \
             [WaveletName example.com/abc123/example.com/conv+root]"
                .to_owned(),
            SubmitErrorKind::BadRequest
        )
    );

    // Right version number, wrong hash. The expected hash is not revealed.
    let wrong_hash = HashedVersion::of(1, [7u8; 20]);
    let (text, kind) = message(
        submit(
            &server,
            &name(),
            &user1(),
            &wrong_hash,
            vec![no_op(&user1())],
        )
        .await,
    );
    assert_eq!(text, format!("Mismatched hash at version 1: {wrong_hash}"));
    assert_eq!(kind, SubmitErrorKind::BadRequest);

    // An old version with the wrong hash, and a version from the future.
    for target in [
        HashedVersion::unsigned(0),
        HashedVersion::of(9, v1.history_hash().to_vec()),
    ] {
        let (text, kind) =
            message(submit(&server, &name(), &user1(), &target, vec![no_op(&user1())]).await);
        assert_eq!(
            text,
            format!(
                "Unrecognized target version at version {}",
                target.version()
            )
        );
        assert_eq!(kind, SubmitErrorKind::BadRequest);
    }

    // The authenticated participant must be the author.
    let as_other = server
        .submit_request_as(
            &user2(),
            &name(),
            delta(&user1(), &v1, vec![no_op(&user1())]),
        )
        .await;
    assert_eq!(
        message(as_other).0,
        "Author field on delta must match logged in user"
    );

    // Nothing of this changed the wavelet.
    assert_eq!(data(&server, &name()).await.hashed_version(), &v1);
    // And a proper submission as the author works.
    let result = server
        .submit_request_as(
            &user1(),
            &name(),
            delta(&user1(), &v1, vec![no_op(&user1())]),
        )
        .await
        .unwrap();
    assert_eq!(result.operations_applied, 1);
    assert_eq!(result.application_timestamp, 1_000_000);
    assert_eq!(result.hashed_version_after_application.version(), 2);
    let _ = v0;
}

#[tokio::test]
async fn only_local_wavelets_are_hosted() {
    let test_db = TestDb::new();
    let (server, _) = server(test_db.open());
    let remote = WaveletName::of_parts(DOMAIN, "abc123", "elsewhere.org", "conv+root").unwrap();
    assert!(server.is_local_wavelet(&name()));
    assert!(!server.is_local_wavelet(&remote));
    let v0 = server.version_zero(&remote);
    let result = submit(&server, &remote, &user1(), &v0, vec![no_op(&user1())]).await;
    match &result {
        Err(SubmitError::NotLocal { domain, .. }) => assert_eq!(domain, "elsewhere.org"),
        other => panic!("unexpected result {other:?}"),
    }
    assert_eq!(
        message(result).0,
        "Wavelet [WaveletName example.com/abc123/elsewhere.org/conv+root] is not hosted here: \
         elsewhere.org is not a local domain"
    );
    assert_eq!(server.get_snapshot(&remote).await.unwrap(), None);
    assert!(
        !server
            .check_access_permission(&remote, Some(&user1()))
            .await
            .unwrap()
    );
    assert_eq!(server.loaded_wave_count(), 0);
}

// Access control.

#[tokio::test]
async fn access_control() {
    let test_db = TestDb::new();
    let (server, _) = server(test_db.open());
    let shared = participant("@example.com");
    let v1 = create(&server, &name()).await;

    assert!(
        server
            .check_access_permission(&name(), Some(&user1()))
            .await
            .unwrap()
    );
    assert!(
        !server
            .check_access_permission(&name(), Some(&user2()))
            .await
            .unwrap()
    );
    assert!(!server.check_access_permission(&name(), None).await.unwrap());

    // A participant adds another, who may then write.
    let v2 = submit(
        &server,
        &name(),
        &user1(),
        &v1,
        vec![add_participant(&user1(), &user2())],
    )
    .await
    .unwrap()
    .hashed_version_after_application;
    assert!(
        server
            .check_access_permission(&name(), Some(&user2()))
            .await
            .unwrap()
    );
    let v3 = submit(
        &server,
        &name(),
        &user2(),
        &v2,
        vec![insert(&user2(), "b+1", 0, 0, "hi")],
    )
    .await
    .unwrap()
    .hashed_version_after_application;

    // Removed again, the write is refused.
    let v4 = submit(
        &server,
        &name(),
        &user1(),
        &v3,
        vec![remove_participant(&user1(), &user2())],
    )
    .await
    .unwrap()
    .hashed_version_after_application;
    let (text, _) = message(
        submit(
            &server,
            &name(),
            &user2(),
            &v4,
            vec![insert(&user2(), "b+1", 2, 2, "!")],
        )
        .await,
    );
    assert!(
        text.starts_with("user2@example.com is not a participant of "),
        "{text}"
    );

    // The shared domain participant opens the wavelet to everyone.
    let v5 = submit(
        &server,
        &name(),
        &user1(),
        &v4,
        vec![add_participant(&user1(), &shared)],
    )
    .await
    .unwrap()
    .hashed_version_after_application;
    assert!(
        server
            .check_access_permission(&name(), Some(&user2()))
            .await
            .unwrap()
    );
    assert!(!server.check_access_permission(&name(), None).await.unwrap());
    submit(
        &server,
        &name(),
        &user2(),
        &v5,
        vec![insert(&user2(), "b+1", 2, 2, "!")],
    )
    .await
    .unwrap();
    assert_eq!(doc_xml(&data(&server, &name()).await, "b+1"), "hi!");
}

// Transformation.

#[tokio::test]
async fn submit_against_stale_version_is_transformed() {
    let test_db = TestDb::new();
    let (server, _) = server(test_db.open());
    let v1 = create(&server, &name()).await;
    let v2 = submit(
        &server,
        &name(),
        &user1(),
        &v1,
        vec![add_participant(&user1(), &user2())],
    )
    .await
    .unwrap()
    .hashed_version_after_application;
    let v3 = submit(
        &server,
        &name(),
        &user1(),
        &v2,
        vec![insert(&user1(), "b+1", 0, 0, "hello")],
    )
    .await
    .unwrap()
    .hashed_version_after_application;

    // Both users edit the document as it was at version 3.
    let a = submit(
        &server,
        &name(),
        &user1(),
        &v3,
        vec![insert(&user1(), "b+1", 5, 0, "A")],
    )
    .await
    .unwrap();
    assert_eq!(a.hashed_version_after_application.version(), 4);
    let b = submit(
        &server,
        &name(),
        &user2(),
        &v3,
        vec![insert(&user2(), "b+1", 5, 5, "B")],
    )
    .await
    .unwrap();
    assert_eq!(b.operations_applied, 1);
    assert_eq!(b.hashed_version_after_application.version(), 5);
    assert_eq!(doc_xml(&data(&server, &name()).await, "b+1"), "AhelloB");

    // A stale delta of several operations on the same document (rejected by
    // the original's transformOps; see resonator-model's transform_submitted_delta).
    let c = submit(
        &server,
        &name(),
        &user2(),
        &v3,
        vec![
            insert(&user2(), "b+1", 5, 5, "1"),
            insert(&user2(), "b+1", 6, 6, "2"),
        ],
    )
    .await
    .unwrap();
    assert_eq!(c.operations_applied, 2);
    assert_eq!(c.hashed_version_after_application.version(), 7);
    let wavelet = data(&server, &name()).await;
    let xml = doc_xml(&wavelet, "b+1");
    assert_eq!(xml.len(), 9);
    assert!(
        xml.starts_with("Ahello") && xml.contains("12") && xml.contains('B'),
        "{xml}"
    );

    // The stored transformed deltas replay to the same wavelet.
    let v0 = server.version_zero(&name());
    let history = server
        .get_history(&name(), &v0, wavelet.hashed_version())
        .await
        .unwrap();
    assert_eq!(history.len(), 6);
    assert_eq!(
        build_wavelet_from_deltas(&name(), &history).unwrap(),
        wavelet
    );
}

#[tokio::test]
async fn concurrent_removal_of_the_author_is_rejected() {
    let test_db = TestDb::new();
    let (server, _) = server(test_db.open());
    let v1 = create(&server, &name()).await;
    let v2 = submit(
        &server,
        &name(),
        &user1(),
        &v1,
        vec![add_participant(&user1(), &user2())],
    )
    .await
    .unwrap()
    .hashed_version_after_application;
    // user2 leaves; user1's concurrent delta adding user2 again conflicts.
    submit(
        &server,
        &name(),
        &user2(),
        &v2,
        vec![remove_participant(&user2(), &user2())],
    )
    .await
    .unwrap();
    let result = submit(
        &server,
        &name(),
        &user1(),
        &v2,
        vec![add_participant(&user1(), &user2())],
    )
    .await;
    assert!(
        matches!(result, Err(SubmitError::Transform(_))),
        "{result:?}"
    );
    assert_eq!(data(&server, &name()).await.version(), 3);
}

// Idempotent resubmission.

#[tokio::test]
async fn duplicate_resubmission_is_idempotent() {
    let test_db = TestDb::new();
    let (server, clock) = server(test_db.open());
    let recorder = Arc::new(Recorder::default());
    server.bus().subscribe(recorder.clone());
    let v1 = create(&server, &name()).await;
    let v2 = submit(
        &server,
        &name(),
        &user1(),
        &v1,
        vec![add_participant(&user1(), &user2())],
    )
    .await
    .unwrap()
    .hashed_version_after_application;

    let ops = || vec![insert(&user1(), "b+1", 0, 0, "once")];
    clock.advance(10);
    let first = submit(&server, &name(), &user1(), &v2, ops())
        .await
        .unwrap();
    clock.advance(10);
    // The response got lost; the client sends the same delta again.
    let again = submit(&server, &name(), &user1(), &v2, ops())
        .await
        .unwrap();
    assert_eq!(again, first);
    assert_eq!(first.application_timestamp, 1_000_010);

    // Others move on; the duplicate still resolves to the first application.
    let v4 = submit(
        &server,
        &name(),
        &user2(),
        &first.hashed_version_after_application,
        vec![insert(&user2(), "b+2", 0, 0, "other")],
    )
    .await
    .unwrap()
    .hashed_version_after_application;
    let late = submit(&server, &name(), &user1(), &v2, ops())
        .await
        .unwrap();
    assert_eq!(late, first);

    // The same operations by another author are not a duplicate.
    let other = submit(
        &server,
        &name(),
        &user2(),
        &v2,
        vec![insert(&user2(), "b+1", 0, 0, "once")],
    )
    .await
    .unwrap();
    assert_eq!(other.hashed_version_after_application.version(), 5);

    let wavelet = data(&server, &name()).await;
    assert_eq!(doc_xml(&wavelet, "b+1"), "onceonce");
    assert_eq!(wavelet.version(), 5);
    server.wait_committed(&name(), 5).await.unwrap();
    assert_eq!(
        server.database().delta_store().delta_count().await.unwrap(),
        5
    );
    eventually(|| recorder.events.lock().unwrap().len() == 10).await;
    let updates = recorder
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.starts_with("update"))
        .count();
    assert_eq!(updates, 5);
    let _ = v4;
}

// Notifications.

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn updates_precede_their_commits_and_arrive_in_order() {
    let test_db = TestDb::new();
    let (server, _) = server(test_db.open());
    let (_, mut events) = server.bus().subscribe_channel();
    let names = [
        wavelet_name("w1", "conv+root"),
        wavelet_name("w2", "conv+root"),
    ];
    let mut tasks = Vec::new();
    for wavelet in names.clone() {
        let server = server.clone();
        tasks.push(tokio::spawn(async move {
            let mut version = create(&server, &wavelet).await;
            for i in 0..40 {
                version = submit(
                    &server,
                    &wavelet,
                    &user1(),
                    &version,
                    vec![insert(&user1(), "b+1", i, i, "x")],
                )
                .await
                .unwrap()
                .hashed_version_after_application;
            }
            server.wait_committed(&wavelet, 41).await.unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }

    // 41 updates and 41 commits per wavelet, interleaved across wavelets.
    let mut received = Vec::new();
    while received.len() < 164 {
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("event")
            .expect("bus open");
        received.push(event);
    }
    for wavelet in &names {
        let (mut updated, mut committed) = (0u64, 0u64);
        for event in received.iter().filter(|e| e.wavelet_name() == wavelet) {
            match event {
                WaveBusEvent::Update {
                    deltas,
                    resulting_version,
                    participants,
                    ..
                } => {
                    assert_eq!(deltas.len(), 1);
                    assert_eq!(deltas[0].applied_at_version.version(), updated);
                    assert_eq!(deltas[0].resulting_version(), resulting_version);
                    assert_eq!(participants, &[user1()]);
                    updated = resulting_version.version();
                }
                WaveBusEvent::Committed { version, .. } => {
                    // Never ahead of what was announced as applied.
                    assert!(version.version() <= updated);
                    assert_eq!(version.version(), committed + 1);
                    committed = version.version();
                }
            }
        }
        assert_eq!((updated, committed), (41, 41));
    }
}

#[tokio::test]
async fn dropped_channel_subscribers_are_forgotten() {
    let test_db = TestDb::new();
    let (server, _) = server(test_db.open());
    let (_, events) = server.bus().subscribe_channel();
    assert_eq!(server.bus().subscriber_count(), 1);
    drop(events);
    create(&server, &name()).await;
    assert_eq!(server.bus().subscriber_count(), 0);
}

#[tokio::test]
async fn panicking_subscriber_does_not_break_submission() {
    struct Bomb;
    impl WaveBusSubscriber for Bomb {
        fn wavelet_update(&self, _: &WaveletData, _: &[Arc<WaveletDeltaRecord>]) {
            panic!("subscriber bug");
        }
        fn wavelet_committed(&self, _: &WaveletName, _: &HashedVersion) {
            panic!("subscriber bug");
        }
    }
    let test_db = TestDb::new();
    let (server, _) = server(test_db.open());
    server.bus().subscribe(Arc::new(Bomb));
    let recorder = Arc::new(Recorder::default());
    server.bus().subscribe(recorder.clone());
    let v1 = create(&server, &name()).await;
    server.wait_committed(&name(), 1).await.unwrap();
    submit(&server, &name(), &user1(), &v1, vec![no_op(&user1())])
        .await
        .unwrap();
    eventually(|| recorder.events.lock().unwrap().len() >= 3).await;
}

// History and snapshots through the provider.

#[tokio::test]
async fn history_and_snapshot_requests() {
    let test_db = TestDb::new();
    let (server, _) = server(test_db.open());
    let v0 = server.version_zero(&name());
    let missing = server.get_history(&name(), &v0, &v0).await;
    match missing {
        Err(WaveServerError::AccessControl(text)) => assert_eq!(
            text,
            "Client request for history made for non-existent wavelet: \
             [WaveletName example.com/abc123/example.com/conv+root]"
        ),
        other => panic!("unexpected result {other:?}"),
    }

    let v1 = create(&server, &name()).await;
    let result = server
        .submit_request_durably(
            &name(),
            delta(&user1(), &v1, vec![insert(&user1(), "b+1", 0, 0, "abc")]),
        )
        .await
        .unwrap();
    let v2 = result.hashed_version_after_application;
    let snapshot = server.get_snapshot(&name()).await.unwrap().unwrap();
    assert_eq!(snapshot.committed_version, v2);
    assert_eq!(snapshot.snapshot.hashed_version(), &v2);
    assert_eq!(
        server.get_versions(&name()).await.unwrap(),
        Some((v2.clone(), v2.clone()))
    );
    assert_eq!(
        server
            .with_wavelet(&name(), |wavelet| doc_xml(wavelet, "b+1"))
            .await
            .unwrap(),
        Some("abc".to_owned())
    );

    let history = server.get_history(&name(), &v0, &v2).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].resulting_version(), &v2);
    assert_eq!(history[1].author(), &user1());
    assert_eq!(
        server
            .get_history_limited(&name(), &v0, &v2, 1)
            .await
            .unwrap(),
        history[..1]
    );
    assert_eq!(
        server.get_history(&name(), &v1, &v2).await.unwrap(),
        history[1..]
    );
    assert!(matches!(
        server
            .get_history(&name(), &HashedVersion::unsigned(1), &v2)
            .await,
        Err(WaveServerError::AccessControl(_))
    ));
    let applied = server
        .get_applied_history(&name(), &v0, &v2, None)
        .await
        .unwrap();
    assert_eq!(applied.len(), 2);
    assert_eq!(calculate_resulting_hashed_version(&applied[1]).unwrap(), v2);
}

// Concurrency, hash chain and restart.

/// Checks the applied-delta hash chain from version zero to `end` and
/// returns the number of deltas.
async fn verify_hash_chain(
    server: &WaveServer,
    wavelet: &WaveletName,
    end: &HashedVersion,
) -> usize {
    let v0 = server.version_zero(wavelet);
    let applied = server
        .get_applied_history(wavelet, &v0, end, None)
        .await
        .unwrap();
    let mut at = v0;
    for delta in &applied {
        assert_eq!(get_hashed_version_applied_at(delta).unwrap(), at);
        at = calculate_resulting_hashed_version(delta).unwrap();
    }
    assert_eq!(&at, end);
    applied.len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_submitters_converge_and_restart_reproduces_the_wavelet() {
    const CLIENTS: usize = 8;
    const DELTAS: usize = 60;
    let test_db = TestDb::new();
    let options = WaveServerOptions {
        wavelet: WaveletSettings {
            snapshot_interval: 64,
            delta_cache_size: 16,
        },
        ..common::options()
    };
    let (first, _) = server_with(test_db.open(), options.clone());
    let wavelet = name();
    let authors: Vec<ParticipantId> = (0..CLIENTS)
        .map(|i| participant(&format!("client{i}@example.com")))
        .collect();
    // Version 1 + CLIENTS: every client is a participant.
    let mut base = create(&first, &wavelet).await;
    for author in &authors {
        base = submit(
            &first,
            &wavelet,
            &user1(),
            &base,
            vec![add_participant(&user1(), author)],
        )
        .await
        .unwrap()
        .hashed_version_after_application;
    }
    let base_version = base.version();

    // Every delta inserts one character into the same document. All
    // deltas being single inserts, the document has `version -
    // base_version` characters at any version, so a client that only
    // knows the version of its own last delta can still build a valid
    // operation against it; the server transforms it over the others'.
    let transformed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for (index, author) in authors.iter().cloned().enumerate() {
        let (server, wavelet, mut known) = (first.clone(), wavelet.clone(), base.clone());
        let transformed = Arc::clone(&transformed);
        tasks.push(tokio::spawn(async move {
            let marker = char::from(b'a' + index as u8).to_string();
            for i in 0..DELTAS {
                let size = (known.version() - base_version) as usize;
                // Front, back and middle.
                let position = match i % 3 {
                    0 => 0,
                    1 => size,
                    _ => size / 2,
                };
                let result = submit(
                    &server,
                    &wavelet,
                    &author,
                    &known,
                    vec![insert(&author, "b+shared", size, position, &marker)],
                )
                .await
                .unwrap();
                assert_eq!(result.operations_applied, 1);
                if result.hashed_version_after_application.version() != known.version() + 1 {
                    transformed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                known = result.hashed_version_after_application;
                if i % 7 == 0 {
                    tokio::task::yield_now().await;
                }
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }

    // All clients started from the same version, so all but the first had
    // to be transformed at least once.
    assert!(transformed.load(std::sync::atomic::Ordering::Relaxed) >= CLIENTS - 1);

    let total = CLIENTS * DELTAS;
    let end_version = base_version + total as u64;
    let live = data(&first, &wavelet).await;
    assert_eq!(live.version(), end_version);
    let end = live.hashed_version().clone();

    // Every character arrived exactly once.
    let text = doc_xml(&live, "b+shared");
    assert_eq!(text.chars().count(), total);
    for index in 0..CLIENTS {
        let marker = char::from(b'a' + index as u8);
        assert_eq!(text.chars().filter(|c| *c == marker).count(), DELTAS);
    }

    // The hash chain verifies end to end, and replaying the transformed
    // history from scratch converges on the same state.
    assert_eq!(
        verify_hash_chain(&first, &wavelet, &end).await,
        total + 1 + CLIENTS
    );
    let v0 = first.version_zero(&wavelet);
    let history = first.get_history(&wavelet, &v0, &end).await.unwrap();
    assert_eq!(build_wavelet_from_deltas(&wavelet, &history).unwrap(), live);

    first.wait_committed(&wavelet, end_version).await.unwrap();
    app::shutdown(first).await;

    // Restart: same state and hashed version, loaded through a snapshot.
    let (second, _) = server_with(test_db.open(), options);
    let reloaded = second.get_snapshot(&wavelet).await.unwrap().unwrap();
    assert_eq!(reloaded.snapshot, live);
    assert_eq!(reloaded.snapshot.hashed_version(), &end);
    assert_eq!(reloaded.committed_version, end);
    let report = second.load_report(&wavelet).await.unwrap().unwrap();
    assert!(report.snapshot_version.is_some());
    assert!(report.deltas_replayed < 64);
    assert_eq!(
        verify_hash_chain(&second, &wavelet, &end).await,
        total + 1 + CLIENTS
    );

    // And it keeps going where it left off.
    let size = total;
    let next = submit(
        &second,
        &wavelet,
        &authors[0],
        &end,
        vec![insert(&authors[0], "b+shared", size, size, "!")],
    )
    .await
    .unwrap();
    assert_eq!(
        next.hashed_version_after_application.version(),
        end_version + 1
    );
    app::shutdown(second).await;
}

// Snapshots.

/// A wavelet with a varied history of `deltas` deltas.
async fn varied_history(
    server: &WaveServer,
    wavelet: &WaveletName,
    deltas: usize,
) -> HashedVersion {
    let mut version = create(server, wavelet).await;
    let mut sizes = [0usize; 3];
    for i in 1..deltas {
        let ops = match i % 10 {
            3 => vec![add_participant(&user1(), &user2())],
            8 => vec![remove_participant(&user1(), &user2())],
            5 => vec![no_op(&user1()), no_op(&user1())],
            _ => {
                let blip = i % 3;
                let op = insert(
                    &user1(),
                    &format!("b+{blip}"),
                    sizes[blip],
                    sizes[blip] / 2,
                    "xy",
                );
                sizes[blip] += 2;
                vec![op]
            }
        };
        version = submit(server, wavelet, &user1(), &version, ops)
            .await
            .unwrap()
            .hashed_version_after_application;
    }
    server
        .wait_committed(wavelet, version.version())
        .await
        .unwrap();
    version
}

fn tamper(test_db: &TestDb, sql: &str) -> usize {
    rusqlite::Connection::open(&test_db.path)
        .unwrap()
        .execute(sql, [])
        .unwrap()
}

#[tokio::test]
async fn wavelet_loaded_via_snapshot_equals_full_replay() {
    let test_db = TestDb::new();
    let options = WaveServerOptions {
        wavelet: WaveletSettings {
            snapshot_interval: 10,
            delta_cache_size: 4,
        },
        ..common::options()
    };
    let (first, _) = server_with(test_db.open(), options.clone());
    let end = varied_history(&first, &name(), 57).await;
    let live = data(&first, &name()).await;
    // A wavelet below the interval has no snapshot.
    let small = wavelet_name("small", "conv+root");
    varied_history(&first, &small, 5).await;
    app::shutdown(first).await;

    let snapshots = first_column(&test_db, "SELECT version FROM snapshots ORDER BY version");
    assert_eq!(
        snapshots.len(),
        2,
        "two newest snapshots are kept: {snapshots:?}"
    );

    // Loaded from the newest snapshot plus the deltas after it.
    let (via_snapshot, _) = server_with(test_db.open(), options.clone());
    let loaded = via_snapshot.get_snapshot(&name()).await.unwrap().unwrap();
    let report = via_snapshot.load_report(&name()).await.unwrap().unwrap();
    assert_eq!(report.snapshot_version, Some(snapshots[1] as u64));
    assert_eq!(report.snapshots_rejected, 0);
    assert_eq!(report.deltas_replayed, 7);
    assert_eq!(loaded.snapshot, live);
    assert_eq!(loaded.snapshot.hashed_version(), &end);
    let report = via_snapshot.load_report(&small).await.unwrap().unwrap();
    assert_eq!((report.snapshot_version, report.deltas_replayed), (None, 5));
    app::shutdown(via_snapshot).await;

    // The newest snapshot is damaged: the older one is used.
    assert_eq!(
        tamper(
            &test_db,
            &format!(
                "UPDATE snapshots SET snapshot = x'ff01' WHERE version = {}",
                snapshots[1]
            )
        ),
        1
    );
    let (via_older, _) = server_with(test_db.open(), options.clone());
    assert_eq!(data(&via_older, &name()).await, live);
    let report = via_older.load_report(&name()).await.unwrap().unwrap();
    assert_eq!(report.snapshot_version, Some(snapshots[0] as u64));
    assert_eq!(report.snapshots_rejected, 1);
    assert_eq!(report.deltas_replayed, 17);
    app::shutdown(via_older).await;

    // No snapshot matches the history: full replay.
    assert_eq!(tamper(&test_db, "UPDATE snapshots SET hash = x'0102'"), 2);
    let (via_replay, _) = server_with(test_db.open(), options.clone());
    let replayed = via_replay.get_snapshot(&name()).await.unwrap().unwrap();
    let report = via_replay.load_report(&name()).await.unwrap().unwrap();
    assert_eq!(report.snapshot_version, None);
    assert_eq!(report.snapshots_rejected, 2);
    assert_eq!(report.deltas_replayed, 57);
    assert_eq!(replayed.snapshot, live);
    assert_eq!(replayed.snapshot.hashed_version(), &end);
    // The next delta writes a fresh, usable snapshot.
    let next = submit(&via_replay, &name(), &user1(), &end, vec![no_op(&user1())])
        .await
        .unwrap()
        .hashed_version_after_application;
    via_replay
        .wait_committed(&name(), next.version())
        .await
        .unwrap();
    let live = data(&via_replay, &name()).await;
    app::shutdown(via_replay).await;

    let (last, _) = server_with(test_db.open(), options);
    assert_eq!(data(&last, &name()).await, live);
    let report = last.load_report(&name()).await.unwrap().unwrap();
    assert_eq!(report.snapshot_version, Some(next.version()));
    assert_eq!(report.deltas_replayed, 0);
}

fn first_column(test_db: &TestDb, sql: &str) -> Vec<i64> {
    let conn = rusqlite::Connection::open(&test_db.path).unwrap();
    let mut statement = conn.prepare(sql).unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[tokio::test]
async fn snapshots_can_be_disabled() {
    let test_db = TestDb::new();
    let options = WaveServerOptions {
        wavelet: WaveletSettings {
            snapshot_interval: 0,
            delta_cache_size: 4,
        },
        ..common::options()
    };
    let (first, _) = server_with(test_db.open(), options.clone());
    varied_history(&first, &name(), 30).await;
    let live = data(&first, &name()).await;
    app::shutdown(first).await;
    assert!(first_column(&test_db, "SELECT version FROM snapshots").is_empty());
    let (second, _) = server_with(test_db.open(), options);
    assert_eq!(data(&second, &name()).await, live);
}

#[tokio::test]
async fn corrupt_history_is_an_error_naming_the_wavelet() {
    let test_db = TestDb::new();
    let (first, _) = server(test_db.open());
    let end = varied_history(&first, &name(), 12).await;
    let other = wavelet_name("fine", "conv+root");
    varied_history(&first, &other, 3).await;
    app::shutdown(first).await;

    // A delta in the middle of one wavelet's history is damaged.
    assert_eq!(
        tamper(
            &test_db,
            "UPDATE deltas SET applied = x'00' WHERE applied_at_version = 4 AND wavelet = \
             (SELECT id FROM wavelets WHERE wave_id = 'abc123')"
        ),
        1
    );
    let (second, _) = server(test_db.open());
    for _ in 0..2 {
        let error = second.get_snapshot(&name()).await.unwrap_err();
        let text = error.to_string();
        assert!(
            text.contains("abc123") && text.contains("Failed to load wavelet"),
            "{text}"
        );
    }
    let (text, kind) =
        message(submit(&second, &name(), &user1(), &end, vec![no_op(&user1())]).await);
    assert!(text.contains("abc123"), "{text}");
    assert_eq!(kind, SubmitErrorKind::InternalServerError);
    // Other wavelets are unaffected.
    assert_eq!(data(&second, &other).await.version(), 3);
}

// Cache bounds.

#[tokio::test]
async fn wave_cache_is_bounded_by_size_and_idle_time() {
    let test_db = TestDb::new();
    let options = WaveServerOptions {
        wave_cache_size: 2,
        wave_cache_expire: Duration::from_secs(60),
        ..common::options()
    };
    let (server, clock) = server_with(test_db.open(), options);
    let names: Vec<WaveletName> = (0..5)
        .map(|i| wavelet_name(&format!("wave{i}"), "conv+root"))
        .collect();
    let mut live = Vec::new();
    for wavelet in &names {
        clock.advance(1000);
        let v0 = server.version_zero(wavelet);
        server
            .submit_request_durably(
                wavelet,
                delta(&user1(), &v0, vec![add_participant(&user1(), &user1())]),
            )
            .await
            .unwrap();
        live.push(data(&server, wavelet).await);
        assert!(
            server.loaded_wave_count() <= 2,
            "{}",
            server.loaded_wave_count()
        );
    }
    assert_eq!(server.loaded_wave_count(), 2);
    assert_eq!(server.get_wave_ids().await.unwrap().len(), 5);

    // Evicted waves load again on demand, least recently used go first.
    clock.advance(1000);
    assert_eq!(data(&server, &names[0]).await, live[0]);
    clock.advance(1000);
    assert_eq!(data(&server, &names[1]).await, live[1]);
    assert_eq!(server.loaded_wave_count(), 2);
    assert_eq!(
        server
            .load_report(&names[0])
            .await
            .unwrap()
            .unwrap()
            .deltas_replayed,
        1
    );

    // Idle expiry.
    assert_eq!(server.evict_idle_waves(), 0);
    clock.advance(59_000);
    assert_eq!(server.evict_idle_waves(), 0);
    clock.advance(2_000);
    // Both loaded waves were last used 61 seconds ago.
    assert_eq!(server.evict_idle_waves(), 2);
    assert_eq!(server.loaded_wave_count(), 0);
    assert_eq!(data(&server, &names[4]).await, live[4]);
    assert_eq!(server.unload_all_waves(), 1);
    assert_eq!(server.loaded_wave_count(), 0);
}

// Recovery from a failed write.

/// A valid first delta for `wavelet`, built outside any server.
fn foreign_first_delta(wavelet: &WaveletName, timestamp: i64) -> WaveletDeltaRecord {
    let v0 = resonator_model::version::HashedVersionFactory::create_version_zero(wavelet);
    let model = resonator_model::operation::WaveletDelta::new(
        user2(),
        v0.clone(),
        vec![add_participant(&user2(), &user2())],
    );
    let signed = ProtocolSignedDelta {
        delta: Some(encode(&resonator_model::proto::serialize_delta(&model).unwrap()).unwrap()),
        signature: vec![],
    };
    let applied = build_applied_delta(signed, &v0, 1, timestamp).unwrap();
    let transformed = build_transformed_delta(&applied, &model).unwrap();
    WaveletDeltaRecord::new(v0, Some(applied), transformed)
}

#[tokio::test]
async fn wavelet_is_reloaded_from_storage_after_a_failed_write() {
    let test_db = TestDb::new();
    let (server, _) = server(test_db.open());
    let recorder = Arc::new(Recorder::default());
    server.bus().subscribe(recorder.clone());
    let v0 = server.version_zero(&name());
    // Load the wavelet as empty (the failed submission leaves it in memory).
    submit(
        &server,
        &name(),
        &user1(),
        &v0,
        vec![remove_participant(&user1(), &user1())],
    )
    .await
    .unwrap_err();
    // Storage changes behind the server's back, so its next write cannot
    // continue the stored history.
    let foreign = foreign_first_delta(&name(), 5);
    server
        .database()
        .delta_store()
        .open(name())
        .append(std::slice::from_ref(&foreign))
        .await
        .unwrap();

    // Applied in memory, but the write fails.
    let applied = submit(
        &server,
        &name(),
        &user1(),
        &v0,
        vec![add_participant(&user1(), &user1())],
    )
    .await
    .unwrap();
    let waited = server.wait_committed(&name(), 1).await;
    assert!(
        matches!(&waited, Err(WaveServerError::State(_))),
        "{waited:?}"
    );
    assert_eq!(*recorder.events.lock().unwrap(), ["update abc123 1"]);

    // The next access starts over from what is stored.
    let reloaded = data(&server, &name()).await;
    assert_eq!(reloaded.hashed_version(), foreign.resulting_version());
    assert_ne!(
        reloaded.hashed_version(),
        &applied.hashed_version_after_application
    );
    assert_eq!(reloaded.participants(), [user2()]);
    // The lost version is no longer known.
    let (text, _) = message(
        submit(
            &server,
            &name(),
            &user2(),
            &applied.hashed_version_after_application,
            vec![no_op(&user2())],
        )
        .await,
    );
    assert_eq!(
        text,
        "Mismatched hash at version 1: ".to_owned()
            + &applied.hashed_version_after_application.to_string()
    );
    submit(
        &server,
        &name(),
        &user2(),
        foreign.resulting_version(),
        vec![no_op(&user2())],
    )
    .await
    .unwrap();
    server.wait_committed(&name(), 2).await.unwrap();
}

#[tokio::test]
async fn submissions_fail_cleanly_once_the_database_is_closed() {
    let test_db = TestDb::new();
    let (server, _) = server(test_db.open());
    let v1 = create(&server, &name()).await;
    server.flush().await.unwrap();
    server.database().close().await;
    let (text, kind) =
        message(submit(&server, &name(), &user1(), &v1, vec![no_op(&user1())]).await);
    assert!(
        text.contains("abc123") && text.contains("database is closed"),
        "{text}"
    );
    assert_eq!(kind, SubmitErrorKind::InternalServerError);
}
