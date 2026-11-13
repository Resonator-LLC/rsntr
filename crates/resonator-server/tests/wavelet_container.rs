//! Tests of the local wavelet container, porting
//! `LocalWaveletContainerImplTest`, the local half of
//! `WaveletContainerTest` and (through the container) `WaveletStateTestBase`
//! of Apache Wave.

mod common;

use std::sync::Arc;

use common::{TestDb, add_participant, delta, no_op, participant, remove_participant};
use resonator_model::docop::DocOpBuilder;
use resonator_model::id::{ParticipantId, WaveletName};
use resonator_model::operation::{WaveletOperation, WaveletOperationError};
use resonator_model::proto::encode;
use resonator_model::proto::federation::protocol_signature::SignatureAlgorithm;
use resonator_model::proto::federation::{
    ProtocolSignature, ProtocolSignedDelta, ProtocolWaveletDelta,
};
use resonator_model::version::{HashedVersion, HashedVersionFactory};
use resonator_server::clock::ManualClock;
use resonator_server::storage::{Database, WaveletDeltaRecord};
use resonator_server::waveserver::{
    LocalWaveletContainer, SubmitError, SubmitErrorKind, WaveBus, WaveServerError, WaveletSettings,
};

const LOCAL_DOMAIN: &str = "example.com";
const TS: i64 = 1234567890;

fn local_wavelet_name() -> WaveletName {
    WaveletName::of_parts(LOCAL_DOMAIN, "waveid", LOCAL_DOMAIN, "waveletid").unwrap()
}

fn author() -> ParticipantId {
    participant("admin@example.com")
}

fn participants() -> Vec<ParticipantId> {
    vec![
        participant("foo@example.com"),
        participant("bar@example3.com"),
    ]
}

fn fake_signature(signer: &[u8]) -> ProtocolSignature {
    ProtocolSignature {
        signature_bytes: Some(signer.to_vec()),
        signer_id: Some(signer.to_vec()),
        signature_algorithm: Some(SignatureAlgorithm::Sha1Rsa as i32),
    }
}

const FAKE_SIGNER_1: &[u8] = &[];
const FAKE_SIGNER_2: &[u8] = &[1];

fn signed(delta: ProtocolWaveletDelta, signer: &[u8]) -> ProtocolSignedDelta {
    ProtocolSignedDelta {
        delta: Some(encode(&delta).unwrap()),
        signature: vec![fake_signature(signer)],
    }
}

fn add_participant_ops() -> Vec<WaveletOperation> {
    participants()
        .iter()
        .map(|p| add_participant(&author(), p))
        .collect()
}

fn remove_participant_ops() -> Vec<WaveletOperation> {
    participants()
        .iter()
        .rev()
        .map(|p| remove_participant(&author(), p))
        .collect()
}

fn double_remove_participant_ops() -> Vec<WaveletOperation> {
    let mut ops = remove_participant_ops();
    ops.extend(remove_participant_ops());
    ops
}

struct Fixture {
    _test_db: TestDb,
    db: Database,
    clock: Arc<ManualClock>,
    wavelet: LocalWaveletContainer,
}

async fn fixture_with(settings: WaveletSettings) -> Fixture {
    let test_db = TestDb::new();
    let db = test_db.open();
    let clock = Arc::new(ManualClock::new(TS));
    let wavelet = LocalWaveletContainer::load(
        &db.delta_store(),
        local_wavelet_name(),
        Arc::new(WaveBus::new()),
        clock.clone(),
        Some(LOCAL_DOMAIN),
        settings,
    )
    .await
    .unwrap();
    Fixture {
        _test_db: test_db,
        db,
        clock,
        wavelet,
    }
}

async fn fixture() -> Fixture {
    fixture_with(WaveletSettings::default()).await
}

impl Fixture {
    async fn version(&self) -> HashedVersion {
        self.wavelet.current_version().await.unwrap()
    }

    async fn submit(
        &self,
        ops: Vec<WaveletOperation>,
        signer: &[u8],
    ) -> Result<Arc<WaveletDeltaRecord>, SubmitError> {
        let target = self.version().await;
        self.wavelet
            .submit_request(signed(delta(&author(), &target, ops), signer))
            .await
    }

    async fn current_participants(&self) -> Option<Vec<ParticipantId>> {
        self.wavelet
            .apply_function(|wavelet| wavelet.map(|wavelet| wavelet.participants().to_vec()))
            .await
            .unwrap()
    }
}

fn assert_operation_error(result: Result<Arc<WaveletDeltaRecord>, SubmitError>) {
    match result {
        Err(error @ SubmitError::Operation(_)) => {
            assert_eq!(error.kind(), SubmitErrorKind::BadRequest)
        }
        other => panic!("expected an operation error, got {other:?}"),
    }
}

// LocalWaveletContainerImplTest.

#[tokio::test]
async fn duplicate_operations() {
    let f = fixture().await;
    let kermit = participant("kermit@muppetshow.com");
    assert_eq!(f.version().await.version(), 0);
    let version_zero = HashedVersionFactory::create_version_zero(&local_wavelet_name());
    assert_eq!(f.version().await, version_zero);

    // Create the wavelet.
    let v0_response = f
        .wavelet
        .submit_request(signed(
            delta(
                &kermit,
                &version_zero,
                vec![add_participant(&kermit, &kermit)],
            ),
            FAKE_SIGNER_1,
        ))
        .await
        .unwrap();
    assert_eq!(f.version().await.version(), 1);

    // An empty document operation creates a new document.
    let add_blip = WaveletOperation::blip_content(
        common::context(&kermit),
        "b+muppet",
        DocOpBuilder::new().build().unwrap(),
    );
    let psd = signed(
        delta(&kermit, v0_response.resulting_version(), vec![add_blip]),
        FAKE_SIGNER_1,
    );
    let dar1 = f.wavelet.submit_request(psd.clone()).await.unwrap();
    assert_eq!(f.version().await.version(), 2);

    let dar2 = f.wavelet.submit_request(psd).await.unwrap();
    assert_eq!(f.version().await.version(), 2);
    assert_eq!(dar1.resulting_version(), dar2.resulting_version());
    assert_eq!(dar1, dar2);
}

// WaveletContainerTest.

#[tokio::test]
async fn local_apply_wavelet_operation() {
    let f = fixture().await;
    f.submit(add_participant_ops(), FAKE_SIGNER_1)
        .await
        .unwrap();
    assert_eq!(f.current_participants().await, Some(participants()));
    f.submit(remove_participant_ops(), FAKE_SIGNER_1)
        .await
        .unwrap();
    assert_eq!(f.current_participants().await, Some(vec![]));
}

#[tokio::test]
async fn local_failed_wavelet_operations() {
    let f = fixture().await;
    assert_operation_error(f.submit(remove_participant_ops(), FAKE_SIGNER_1).await);
    // Still no wavelet.
    assert!(f.wavelet.is_empty().await.unwrap());
    assert_eq!(f.current_participants().await, None);

    f.submit(add_participant_ops(), FAKE_SIGNER_1)
        .await
        .unwrap();
    assert_operation_error(f.submit(add_participant_ops(), FAKE_SIGNER_1).await);
    assert_eq!(f.current_participants().await, Some(participants()));

    // The first two removals succeed, the third fails: all are undone.
    let before = f.wavelet.copy_wavelet_data().await.unwrap();
    let result = f
        .submit(double_remove_participant_ops(), FAKE_SIGNER_1)
        .await;
    match &result {
        Err(SubmitError::Operation(WaveletOperationError::DeltaFailed { applied, .. })) => {
            assert_eq!(*applied, 2)
        }
        other => panic!("unexpected result {other:?}"),
    }
    assert_eq!(f.current_participants().await, Some(participants()));
    assert_eq!(f.wavelet.copy_wavelet_data().await.unwrap(), before);
}

#[tokio::test]
async fn successful_local_request() {
    let f = fixture().await;
    f.submit(add_participant_ops(), FAKE_SIGNER_1)
        .await
        .unwrap();
    let v2 = f.version().await;
    assert_eq!(v2.version(), 2);
    assert!(f.wavelet.is_delta_signer(&v2, FAKE_SIGNER_1).await.unwrap());
    assert!(!f.wavelet.is_delta_signer(&v2, FAKE_SIGNER_2).await.unwrap());

    f.submit(remove_participant_ops(), FAKE_SIGNER_2)
        .await
        .unwrap();
    let v4 = f.version().await;
    assert_eq!(v4.version(), 4);
    assert!(f.wavelet.is_delta_signer(&v2, FAKE_SIGNER_1).await.unwrap());
    assert!(!f.wavelet.is_delta_signer(&v2, FAKE_SIGNER_2).await.unwrap());
    assert!(f.wavelet.is_delta_signer(&v4, FAKE_SIGNER_2).await.unwrap());
    assert!(!f.wavelet.is_delta_signer(&v4, FAKE_SIGNER_1).await.unwrap());
    // Not a delta boundary, or the wrong hash.
    assert!(
        !f.wavelet
            .is_delta_signer(&HashedVersion::unsigned(2), FAKE_SIGNER_1)
            .await
            .unwrap()
    );
    assert!(
        !f.wavelet
            .is_delta_signer(&HashedVersion::unsigned(3), FAKE_SIGNER_1)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn failed_local_wavelet_request() {
    let f = fixture().await;
    let version_zero = f.version().await;
    assert_operation_error(f.submit(remove_participant_ops(), FAKE_SIGNER_1).await);
    assert_eq!(f.version().await, version_zero);

    f.submit(add_participant_ops(), FAKE_SIGNER_1)
        .await
        .unwrap();
    assert_operation_error(f.submit(add_participant_ops(), FAKE_SIGNER_2).await);
    let old_version = f.version().await;
    assert_eq!(old_version.version(), 2);
    assert!(
        f.wavelet
            .is_delta_signer(&old_version, FAKE_SIGNER_1)
            .await
            .unwrap()
    );
    assert!(
        !f.wavelet
            .is_delta_signer(&old_version, FAKE_SIGNER_2)
            .await
            .unwrap()
    );

    assert_operation_error(
        f.submit(double_remove_participant_ops(), FAKE_SIGNER_1)
            .await,
    );
    assert_eq!(f.version().await, old_version);

    // Nothing of the failed deltas reached storage.
    f.wavelet.wait_committed(2).await.unwrap();
    f.db.flush().await.unwrap();
    assert_eq!(f.db.delta_store().delta_count().await.unwrap(), 1);
}

#[tokio::test]
async fn local_empty_delta() {
    let f = fixture().await;
    let result = f.submit(vec![], FAKE_SIGNER_1).await;
    match result {
        Err(error @ SubmitError::EmptyDelta(0)) => {
            assert_eq!(error.to_string(), "Empty delta at version 0")
        }
        other => panic!("unexpected result {other:?}"),
    }
}

#[tokio::test]
async fn operations_of_different_sizes() {
    let f = fixture().await;
    let doc_id = "b+somedoc";
    let content = |text: &str| {
        WaveletOperation::blip_content(
            common::context(&author()),
            doc_id,
            DocOpBuilder::new().characters(text).build().unwrap(),
        )
    };
    f.submit(vec![content("hi")], FAKE_SIGNER_1).await.unwrap();
    // "bye" does not span the two existing characters.
    assert_operation_error(f.submit(vec![content("bye")], FAKE_SIGNER_1).await);
    let xml = f
        .wavelet
        .apply_function(|wavelet| common::doc_xml(wavelet.unwrap(), doc_id))
        .await
        .unwrap();
    assert_eq!(xml, "hi");
    assert_eq!(f.version().await.version(), 1);
}

#[tokio::test]
async fn failed_operation_on_a_new_document_leaves_no_document_behind() {
    let f = fixture().await;
    f.submit(add_participant_ops(), FAKE_SIGNER_1)
        .await
        .unwrap();
    let before = f.wavelet.copy_wavelet_data().await.unwrap();
    // Retains one item of a document that does not exist.
    let bad = WaveletOperation::blip_content(
        common::context(&author()),
        "b+new",
        DocOpBuilder::new().retain(1).build().unwrap(),
    );
    assert_operation_error(f.submit(vec![no_op(&author()), bad], FAKE_SIGNER_1).await);
    assert_eq!(f.wavelet.copy_wavelet_data().await.unwrap(), before);
    assert!(before.unwrap().document("b+new").is_none());
}

#[tokio::test]
async fn malformed_signed_deltas_are_rejected() {
    let f = fixture().await;
    let garbage = ProtocolSignedDelta {
        delta: Some(vec![0xff, 0xff, 0xff]),
        signature: vec![],
    };
    let missing = ProtocolSignedDelta {
        delta: None,
        signature: vec![],
    };
    for bad in [garbage, missing] {
        let error = f.wavelet.submit_request(bad).await.unwrap_err();
        assert!(matches!(error, SubmitError::InvalidDelta(_)), "{error}");
        assert_eq!(error.kind(), SubmitErrorKind::BadRequest);
    }
    assert_eq!(f.version().await.version(), 0);
}

// WaveletStateTestBase, through the container.

struct History {
    f: Fixture,
    v0: HashedVersion,
    d1: Arc<WaveletDeltaRecord>,
    d2: Arc<WaveletDeltaRecord>,
    d3: Arc<WaveletDeltaRecord>,
}

/// Three no-op deltas of 2, 2 and 1 operations at TS, TS+1 and TS+2. With
/// `wait` each is committed before the next, so that with a small delta
/// cache the older ones are only in storage.
async fn history(settings: WaveletSettings, wait: bool) -> History {
    let f = fixture_with(settings).await;
    let v0 = f.version().await;
    let mut records = Vec::new();
    for count in [2usize, 2, 1] {
        let ops = (0..count).map(|_| no_op(&author())).collect();
        let record = f.submit(ops, FAKE_SIGNER_1).await.unwrap();
        if wait {
            f.wavelet
                .wait_committed(record.resulting_version().version())
                .await
                .unwrap();
        }
        f.clock.advance(1);
        records.push(record);
    }
    let d3 = records.pop().unwrap();
    let d2 = records.pop().unwrap();
    let d1 = records.pop().unwrap();
    History { f, v0, d1, d2, d3 }
}

fn tiny_cache() -> WaveletSettings {
    WaveletSettings {
        delta_cache_size: 1,
        ..WaveletSettings::default()
    }
}

#[tokio::test]
async fn empty_state_is_empty() {
    let f = fixture().await;
    let v0 = HashedVersionFactory::create_version_zero(&local_wavelet_name());
    assert_eq!(f.wavelet.wavelet_name(), &local_wavelet_name());
    assert!(f.wavelet.is_empty().await.unwrap());
    assert_eq!(f.wavelet.get_snapshot().await.unwrap(), None);
    assert_eq!(f.version().await, v0);
    assert_eq!(f.wavelet.version_zero().await, v0);
    assert_eq!(f.wavelet.last_committed_version().await.unwrap(), v0);
    assert_eq!(f.wavelet.creator().await.unwrap(), None);
    assert!(
        f.wavelet
            .request_history(&v0, &v0, None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn snapshot_metadata_reflects_deltas() {
    let h = history(WaveletSettings::default(), false).await;
    let snapshot = h.f.wavelet.get_snapshot().await.unwrap().unwrap().snapshot;
    assert_eq!(snapshot.creator(), &author());
    assert_eq!(snapshot.creation_time(), TS);
    assert_eq!(snapshot.hashed_version(), h.d3.resulting_version());
    assert_eq!(snapshot.version(), 5);
    assert_eq!(h.d1.application_timestamp(), TS);
    assert_eq!(h.d2.application_timestamp(), TS + 1);
    assert_eq!(h.d1.applied_at_version, h.v0);
    assert_eq!(h.d2.applied_at_version, *h.d1.resulting_version());
    assert_eq!(h.d1.resulting_version().version(), 2);
    assert_eq!(h.d2.resulting_version().version(), 4);
    assert_eq!(h.f.wavelet.creator().await.unwrap(), Some(author()));
}

#[tokio::test]
async fn history_is_served_from_memory_and_from_storage() {
    for (settings, wait) in [
        (WaveletSettings::default(), false),
        (WaveletSettings::default(), true),
        (tiny_cache(), true),
    ] {
        let h = history(settings, wait).await;
        let w = &h.f.wavelet;
        let all = [&h.d1, &h.d2, &h.d3];
        // Every contiguous run of deltas.
        for first in 0..3 {
            for last in first..3 {
                let start = &all[first].applied_at_version;
                let end = all[last].resulting_version();
                let records = w.request_history_records(start, end, None).await.unwrap();
                assert_eq!(records.len(), last - first + 1);
                for (record, expected) in records.iter().zip(&all[first..=last]) {
                    assert_eq!(record, *expected);
                }
                let transformed = w
                    .request_transformed_history(start, end, None)
                    .await
                    .unwrap();
                let applied = w.request_history(start, end, None).await.unwrap();
                for (i, expected) in all[first..=last].iter().enumerate() {
                    assert_eq!(transformed[i], expected.transformed);
                    assert_eq!(Some(&applied[i]), expected.applied.as_ref());
                }
                // Interrupted after the first delta.
                let limited = w
                    .request_history_records(start, end, Some(1))
                    .await
                    .unwrap();
                assert_eq!(limited.len(), 1);
                assert_eq!(&limited[0], all[first]);
            }
        }
        // An empty range at any boundary.
        for version in [&h.v0, h.d1.resulting_version(), h.d3.resulting_version()] {
            assert!(
                w.request_history(version, version, None)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }
}

#[tokio::test]
async fn history_requires_delta_boundaries_with_correct_hashes() {
    for (settings, wait) in [(WaveletSettings::default(), false), (tiny_cache(), true)] {
        let h = history(settings, wait).await;
        let w = &h.f.wavelet;
        let end = h.d3.resulting_version();
        let expect = |result: Result<Vec<_>, WaveServerError>, message: &str| match result {
            Err(WaveServerError::AccessControl(actual)) => assert_eq!(actual, message),
            other => panic!("expected access control error, got {other:?}"),
        };
        // Wrong hashes.
        expect(
            w.request_history_records(&HashedVersion::unsigned(0), end, None)
                .await,
            "Unrecognized start version at version 0",
        );
        expect(
            w.request_history_records(&h.v0, &HashedVersion::unsigned(5), None)
                .await,
            "Unrecognized end version at version 5",
        );
        expect(
            w.request_history_records(&HashedVersion::unsigned(2), end, None)
                .await,
            "Unrecognized start version at version 2",
        );
        // Versions inside deltas and beyond the end.
        for version in [1u64, 3, 6] {
            let inside = HashedVersion::of(version, end.history_hash().to_vec());
            expect(
                w.request_history_records(&h.v0, &inside, None).await,
                &format!("Unrecognized end version at version {version}"),
            );
            expect(
                w.request_history_records(&inside, end, None).await,
                &format!("Unrecognized start version at version {version}"),
            );
        }
        // Backwards.
        expect(
            w.request_history_records(end, &h.v0, None).await,
            "Start version 5 is after end version 0",
        );
    }
}

#[tokio::test]
async fn committed_version_follows_persistence() {
    let h = history(WaveletSettings::default(), false).await;
    let end = h.d3.resulting_version().clone();
    assert!(h.f.wavelet.wait_committed(2).await.unwrap().version() >= 2);
    assert_eq!(h.f.wavelet.wait_committed(5).await.unwrap(), end);
    assert_eq!(h.f.wavelet.last_committed_version().await.unwrap(), end);
    let snapshot = h.f.wavelet.get_snapshot().await.unwrap().unwrap();
    assert_eq!(snapshot.committed_version, end);
    // The stored history is what was submitted.
    let stored =
        h.f.db
            .delta_store()
            .open(local_wavelet_name())
            .deltas_in_range(0, u64::MAX, usize::MAX)
            .await
            .unwrap();
    assert_eq!(stored, [(*h.d1).clone(), (*h.d2).clone(), (*h.d3).clone()]);
}

#[tokio::test]
async fn access_permission_follows_participants_and_shared_domain() {
    let f = fixture().await;
    let foo = participant("foo@example.com");
    let stranger = participant("stranger@example.com");
    let shared = participant("@example.com");
    // Anyone logged in may write the first delta.
    assert!(
        f.wavelet
            .check_access_permission(Some(&stranger))
            .await
            .unwrap()
    );
    assert!(!f.wavelet.check_access_permission(None).await.unwrap());

    f.submit(add_participant_ops(), FAKE_SIGNER_1)
        .await
        .unwrap();
    assert!(f.wavelet.check_access_permission(Some(&foo)).await.unwrap());
    assert!(f.wavelet.has_participant(&foo).await.unwrap());
    assert!(
        !f.wavelet
            .check_access_permission(Some(&stranger))
            .await
            .unwrap()
    );
    assert!(!f.wavelet.has_participant(&stranger).await.unwrap());
    assert!(!f.wavelet.check_access_permission(None).await.unwrap());

    // With the shared domain participant on the wavelet, everyone has
    // access.
    assert_eq!(f.wavelet.shared_domain_participant(), Some(&shared));
    f.submit(vec![add_participant(&author(), &shared)], FAKE_SIGNER_1)
        .await
        .unwrap();
    assert!(
        f.wavelet
            .check_access_permission(Some(&stranger))
            .await
            .unwrap()
    );
    assert!(!f.wavelet.has_participant(&stranger).await.unwrap());
    assert!(!f.wavelet.check_access_permission(None).await.unwrap());
}

// Federation requests: the receipt of a submission travels with its delta.

fn receipt(id: &str) -> resonator_server::waveserver::SubmitReceipt {
    resonator_server::waveserver::SubmitReceipt {
        request_id: id.to_owned(),
        domain: "remote.example".to_owned(),
        applied_at: TS,
        response: Box::new(|result| Some(result.operations_applied.to_string().into_bytes())),
    }
}

#[tokio::test]
async fn a_receipt_is_committed_with_its_delta_and_only_then() {
    let f = fixture().await;
    let store = f.db.federation_store();
    let target = f.version().await;
    let mut ops = vec![add_participant(&author(), &author())];
    ops.extend(add_participant_ops());
    let first = signed(delta(&author(), &target, ops), FAKE_SIGNER_1);
    let (record, recorded) = f
        .wavelet
        .submit_request_with_receipt(&author(), first.clone(), Some(receipt("req-1")))
        .await
        .unwrap();
    assert!(recorded);
    f.wavelet
        .wait_committed(record.resulting_version().version())
        .await
        .unwrap();
    let (domain, response) = store.applied_request("req-1").await.unwrap().unwrap();
    assert_eq!(domain, "remote.example");
    assert_eq!(response, Some(b"3".to_vec()));

    // The same delta again is a duplicate: nothing applied, nothing
    // recorded with it.
    let (again, recorded) = f
        .wavelet
        .submit_request_with_receipt(&author(), first, Some(receipt("req-2")))
        .await
        .unwrap();
    assert!(!recorded);
    assert_eq!(again.resulting_version(), record.resulting_version());
    f.db.flush().await.unwrap();
    assert_eq!(store.applied_request("req-2").await.unwrap(), None);

    // A delta that fails to apply records nothing either.
    let target = f.version().await;
    let failing = signed(
        delta(&author(), &target, add_participant_ops()),
        FAKE_SIGNER_1,
    );
    assert!(
        f.wavelet
            .submit_request_with_receipt(&author(), failing, Some(receipt("req-3")))
            .await
            .is_err()
    );
    f.db.flush().await.unwrap();
    assert_eq!(store.applied_request("req-3").await.unwrap(), None);
}
