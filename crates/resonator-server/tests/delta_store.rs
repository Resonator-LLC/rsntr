//! Tests of the SQLite database and delta store. The first group ports
//! `DeltaStoreTestBase` of Apache Wave.

mod common;

use std::collections::BTreeSet;

use common::{TestDb, add_participant, no_op, participant};
use resonator_model::id::{WaveId, WaveletId, WaveletName};
use resonator_model::operation::TransformedWaveletDelta;
use resonator_model::proto::federation::protocol_signature::SignatureAlgorithm;
use resonator_model::proto::federation::{
    ProtocolAppliedWaveletDelta, ProtocolSignature, ProtocolSignedDelta,
};
use resonator_model::proto::{
    ByteStringMessage, encode, serialize_hashed_version, serialize_transformed_delta,
};
use resonator_model::version::HashedVersion;
use resonator_server::storage::{
    Database, DatabaseOptions, DeltaStore, SCHEMA_VERSION, StorageError, StoredSnapshot,
    Synchronous, WaveletDeltaRecord,
};

fn wave1_wavelet1() -> WaveletName {
    WaveletName::of(
        WaveId::of("example.com", "wave1").unwrap(),
        WaveletId::of("example.com", "wavelet1").unwrap(),
    )
}

fn wave2_wavelet1() -> WaveletName {
    WaveletName::of(
        WaveId::of("example.com", "wave2").unwrap(),
        WaveletId::of("example.com", "wavelet1").unwrap(),
    )
}

/// A record of two operations applied at `applied_at`, with made-up hashes
/// (`createRecord`); the store does not check hashes.
fn record_at(applied_at: u64, timestamp: i64) -> WaveletDeltaRecord {
    let author = participant("author@example.com");
    let other = participant("other@example.com");
    let target_version = HashedVersion::of(applied_at, [3u8, 2, 1]);
    let resulting_version = HashedVersion::of(applied_at + 2, [1u8, 2, 3]);
    let ops = [no_op(&author), add_participant(&author, &other)];
    let transformed = TransformedWaveletDelta::clone_operations(
        author.clone(),
        resulting_version,
        timestamp,
        &ops,
    )
    .unwrap();
    let serialized = serialize_transformed_delta(&transformed).unwrap();
    let signature = ProtocolSignature {
        signature_bytes: Some(vec![1, 2, 3]),
        signer_id: Some(b"somebody".to_vec()),
        signature_algorithm: Some(SignatureAlgorithm::Sha1Rsa as i32),
    };
    let signed = ProtocolSignedDelta {
        delta: Some(encode(&serialized).unwrap()),
        signature: vec![signature],
    };
    let applied = ProtocolAppliedWaveletDelta {
        signed_original_delta: Some(signed),
        hashed_version_applied_at: Some(serialize_hashed_version(&target_version).unwrap()),
        operations_applied: Some(2),
        application_timestamp: Some(timestamp),
        epoch: None,
        rebased_operations: None,
    };
    WaveletDeltaRecord::new(
        target_version,
        Some(ByteStringMessage::serialize_message(applied).unwrap()),
        transformed,
    )
}

fn create_record() -> WaveletDeltaRecord {
    record_at(0, 1234567890)
}

async fn store_with_record(name: &WaveletName) -> (TestDb, DeltaStore, WaveletDeltaRecord) {
    let test_db = TestDb::new();
    let store = test_db.open().delta_store();
    let record = create_record();
    store
        .open(name.clone())
        .append(std::slice::from_ref(&record))
        .await
        .unwrap();
    (test_db, store, record)
}

// DeltaStoreTestBase.

#[tokio::test]
async fn open_nonexistent_wavelet() {
    let test_db = TestDb::new();
    let store = test_db.open().delta_store();
    let wavelet = store.open(wave1_wavelet1());
    assert!(wavelet.is_empty().await.unwrap());
    assert_eq!(wavelet.wavelet_name(), &wave1_wavelet1());
    assert_eq!(wavelet.end_version().await.unwrap(), None);
    assert_eq!(wavelet.delta(0).await.unwrap(), None);
    assert_eq!(wavelet.delta_by_end_version(0).await.unwrap(), None);
    assert_eq!(wavelet.applied_at_version(0).await.unwrap(), None);
    assert_eq!(wavelet.resulting_version(0).await.unwrap(), None);
    assert_eq!(wavelet.applied_delta(0).await.unwrap(), None);
    assert_eq!(wavelet.transformed_delta(0).await.unwrap(), None);
    assert_eq!(wavelet.last_delta().await.unwrap(), None);
}

#[tokio::test]
async fn write_to_new_wavelet() {
    let (_db, store, record) = store_with_record(&wave1_wavelet1()).await;
    let wavelet = store.open(wave1_wavelet1());
    assert!(!wavelet.is_empty().await.unwrap());
    assert_eq!(
        wavelet.end_version().await.unwrap().as_ref(),
        Some(record.resulting_version())
    );
    assert_eq!(wavelet.delta(0).await.unwrap().as_ref(), Some(&record));
    assert_eq!(
        wavelet
            .delta_by_end_version(record.resulting_version().version())
            .await
            .unwrap()
            .as_ref(),
        Some(&record)
    );
    assert_eq!(
        wavelet.applied_at_version(0).await.unwrap().as_ref(),
        Some(&record.applied_at_version)
    );
    assert_eq!(
        wavelet.resulting_version(0).await.unwrap().as_ref(),
        Some(record.resulting_version())
    );
    assert_eq!(wavelet.applied_delta(0).await.unwrap(), record.applied);
    assert_eq!(
        wavelet.transformed_delta(0).await.unwrap().as_ref(),
        Some(&record.transformed)
    );
    assert_eq!(wavelet.last_delta().await.unwrap().as_ref(), Some(&record));
    // No delta is applied at, or ends at, a version inside the delta.
    assert_eq!(wavelet.delta(1).await.unwrap(), None);
    assert_eq!(wavelet.delta_by_end_version(1).await.unwrap(), None);
    assert_eq!(wavelet.delta_by_end_version(3).await.unwrap(), None);
}

#[tokio::test]
async fn applied_bytes_are_stored_exactly() {
    let (_db, store, record) = store_with_record(&wave1_wavelet1()).await;
    let stored = store.open(wave1_wavelet1()).applied_delta(0).await.unwrap();
    assert_eq!(
        stored.unwrap().bytes(),
        record.applied.as_ref().unwrap().bytes()
    );
}

#[tokio::test]
async fn delete_wavelet_removes_deltas() {
    let (_db, store, _) = store_with_record(&wave1_wavelet1()).await;
    store.delete(&wave1_wavelet1()).await.unwrap();
    let wavelet = store.open(wave1_wavelet1());
    assert!(wavelet.is_empty().await.unwrap());
    assert_eq!(wavelet.delta(0).await.unwrap(), None);
    assert_eq!(store.delta_count().await.unwrap(), 0);
}

#[tokio::test]
async fn delete_of_missing_wavelet_fails() {
    let test_db = TestDb::new();
    let store = test_db.open().delta_store();
    assert!(matches!(
        store.delete(&wave1_wavelet1()).await,
        Err(StorageError::WaveletNotFound(_))
    ));
}

#[tokio::test]
async fn lookup_returns_wavelets() {
    let (_db, store, _) = store_with_record(&wave1_wavelet1()).await;
    assert_eq!(
        store.lookup(&wave1_wavelet1().wave_id).await.unwrap(),
        BTreeSet::from([wave1_wavelet1().wavelet_id])
    );
}

#[tokio::test]
async fn lookup_does_not_return_empty_wavelets() {
    let test_db = TestDb::new();
    let store = test_db.open().delta_store();
    let _wavelet = store.open(wave1_wavelet1());
    assert!(
        store
            .lookup(&wave1_wavelet1().wave_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn lookup_does_not_return_deleted_wavelets() {
    let (_db, store, _) = store_with_record(&wave1_wavelet1()).await;
    store.delete(&wave1_wavelet1()).await.unwrap();
    assert!(
        store
            .lookup(&wave1_wavelet1().wave_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn wave_ids_returns_wave_ids() {
    let (_db, store, _) = store_with_record(&wave1_wavelet1()).await;
    assert_eq!(store.wave_ids().await.unwrap(), [wave1_wavelet1().wave_id]);
}

#[tokio::test]
async fn wave_ids_does_not_return_empty_wavelets() {
    let test_db = TestDb::new();
    let store = test_db.open().delta_store();
    let _wavelet = store.open(wave1_wavelet1());
    assert!(store.wave_ids().await.unwrap().is_empty());
}

#[tokio::test]
async fn wave_ids_does_not_return_deleted_wavelets() {
    let (_db, store, _) = store_with_record(&wave1_wavelet1()).await;
    store.delete(&wave1_wavelet1()).await.unwrap();
    assert!(store.wave_ids().await.unwrap().is_empty());
}

#[tokio::test]
async fn wave_ids_lists_each_wave_once() {
    let (_db, store, _) = store_with_record(&wave1_wavelet1()).await;
    store
        .open(wave2_wavelet1())
        .append(&[create_record()])
        .await
        .unwrap();
    let other_wavelet = WaveletName::of(
        wave1_wavelet1().wave_id,
        WaveletId::of("example.com", "wavelet2").unwrap(),
    );
    store
        .open(other_wavelet)
        .append(&[create_record()])
        .await
        .unwrap();
    assert_eq!(
        store.wave_ids().await.unwrap(),
        [wave1_wavelet1().wave_id, wave2_wavelet1().wave_id]
    );
    assert_eq!(
        store.lookup(&wave1_wavelet1().wave_id).await.unwrap().len(),
        2
    );
}

// Beyond the original test base.

#[tokio::test]
async fn ranged_history_and_lookups_by_either_end() {
    let test_db = TestDb::new();
    let store = test_db.open().delta_store();
    let wavelet = store.open(wave1_wavelet1());
    let records: Vec<_> = (0..5).map(|i| record_at(i * 2, 1000 + i as i64)).collect();
    wavelet.append(&records[..2]).await.unwrap();
    wavelet.append(&records[2..]).await.unwrap();

    assert_eq!(
        wavelet.end_version().await.unwrap().as_ref(),
        Some(records[4].resulting_version())
    );
    assert_eq!(
        wavelet.last_delta().await.unwrap().as_ref(),
        Some(&records[4])
    );
    assert_eq!(wavelet.delta(4).await.unwrap().as_ref(), Some(&records[2]));
    assert_eq!(
        wavelet.delta_by_end_version(4).await.unwrap().as_ref(),
        Some(&records[1])
    );
    assert_eq!(
        wavelet.hashed_version_ending_at(4).await.unwrap().as_ref(),
        Some(records[1].resulting_version())
    );
    assert_eq!(wavelet.hashed_version_ending_at(5).await.unwrap(), None);
    assert_eq!(wavelet.hashed_version_ending_at(12).await.unwrap(), None);

    assert_eq!(
        wavelet.deltas_in_range(0, 10, usize::MAX).await.unwrap(),
        records
    );
    assert_eq!(
        wavelet.deltas_in_range(2, 8, usize::MAX).await.unwrap(),
        records[1..4]
    );
    assert_eq!(
        wavelet.deltas_in_range(2, u64::MAX, 2).await.unwrap(),
        records[1..3]
    );
    // A start that is not a delta boundary yields nothing.
    assert!(
        wavelet
            .deltas_in_range(3, 10, usize::MAX)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        wavelet
            .deltas_in_range(10, 20, usize::MAX)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.delta_count().await.unwrap(), 5);
}

#[tokio::test]
async fn append_must_continue_the_history() {
    let (_db, store, _) = store_with_record(&wave1_wavelet1()).await;
    let wavelet = store.open(wave1_wavelet1());
    // Same version again, and a gap.
    for bad in [record_at(0, 1), record_at(4, 1)] {
        let error = wavelet.append(&[bad]).await.unwrap_err();
        assert!(matches!(error, StorageError::Invalid(_)), "{error}");
    }
    // A fresh wavelet must start at version zero.
    let error = store
        .open(wave2_wavelet1())
        .append(&[record_at(2, 1)])
        .await
        .unwrap_err();
    assert!(matches!(error, StorageError::Invalid(_)), "{error}");
    // The failed first append left no wavelet behind.
    assert!(store.wave_ids().await.unwrap() == [wave1_wavelet1().wave_id]);
    assert_eq!(store.delta_count().await.unwrap(), 1);
    wavelet.append(&[record_at(2, 1)]).await.unwrap();
}

#[tokio::test]
async fn snapshots_are_kept_newest_first_and_pruned() {
    let test_db = TestDb::new();
    let store = test_db.open().delta_store();
    let wavelet = store.open(wave1_wavelet1());
    let records: Vec<_> = (0..4).map(|i| record_at(i * 2, 1)).collect();
    wavelet.append(&records).await.unwrap();
    assert!(wavelet.snapshots().await.unwrap().is_empty());

    let snapshot_at = |index: usize| StoredSnapshot {
        version: records[index].resulting_version().clone(),
        snapshot: vec![index as u8; 3],
    };
    for index in 0..3 {
        wavelet.put_snapshot(snapshot_at(index), 2).await.unwrap();
    }
    assert_eq!(
        wavelet.snapshots().await.unwrap(),
        [snapshot_at(2), snapshot_at(1)]
    );

    // Not at a delta boundary, or with the wrong hash.
    for version in [
        HashedVersion::of(3, [1u8, 2, 3]),
        HashedVersion::of(2, [9u8]),
    ] {
        let bad = StoredSnapshot {
            version,
            snapshot: vec![],
        };
        assert!(matches!(
            wavelet.put_snapshot(bad, 2).await,
            Err(StorageError::Invalid(_))
        ));
    }

    // Snapshots go with their wavelet.
    store.delete(&wave1_wavelet1()).await.unwrap();
    assert!(wavelet.snapshots().await.unwrap().is_empty());
}

#[tokio::test]
async fn data_survives_reopening() {
    let test_db = TestDb::new();
    let db = test_db.open();
    let record = create_record();
    db.delta_store()
        .open(wave1_wavelet1())
        .append(std::slice::from_ref(&record))
        .await
        .unwrap();
    db.set_meta("answer", "42").await.unwrap();
    db.close().await;
    assert!(matches!(db.flush().await, Err(StorageError::Closed)));
    assert!(matches!(db.meta("answer").await, Err(StorageError::Closed)));

    let db = Database::open(
        &test_db.path,
        DatabaseOptions {
            synchronous: Synchronous::Full,
            read_connections: 1,
            ..DatabaseOptions::default()
        },
    )
    .unwrap();
    assert_eq!(db.schema_version().await.unwrap(), SCHEMA_VERSION);
    assert_eq!(db.meta("answer").await.unwrap().as_deref(), Some("42"));
    assert_eq!(db.meta("question").await.unwrap(), None);
    assert_eq!(
        db.delta_store()
            .open(wave1_wavelet1())
            .delta(0)
            .await
            .unwrap(),
        Some(record)
    );
    db.close().await;
}

#[tokio::test]
async fn newer_schema_is_refused() {
    let test_db = TestDb::new();
    test_db.open().close().await;
    tamper(
        &test_db,
        &format!("PRAGMA user_version = {}", SCHEMA_VERSION + 1),
    );
    assert!(matches!(
        Database::open(&test_db.path, DatabaseOptions::default()),
        Err(StorageError::SchemaTooNew { .. })
    ));
}

/// Changes the database behind the server's back.
fn tamper(test_db: &TestDb, sql: &str) {
    rusqlite::Connection::open(&test_db.path)
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writes_are_group_committed_in_order() {
    let test_db = TestDb::new();
    let db = test_db.open();
    let store = db.delta_store();
    // Many wavelets appended concurrently, each strictly in order.
    let mut tasks = Vec::new();
    for w in 0..16 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            let name = WaveletName::of(
                WaveId::of("example.com", &format!("wave{w}")).unwrap(),
                WaveletId::of("example.com", "wavelet").unwrap(),
            );
            let wavelet = store.open(name);
            for i in 0..50u64 {
                wavelet.append(&[record_at(i * 2, i as i64)]).await.unwrap();
            }
            assert_eq!(wavelet.end_version().await.unwrap().unwrap().version(), 100);
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(store.delta_count().await.unwrap(), 16 * 50);
    assert_eq!(store.wave_ids().await.unwrap().len(), 16);
}

#[tokio::test]
async fn backup_is_a_consistent_copy() {
    let (test_db, store, record) = store_with_record(&wave1_wavelet1()).await;
    let db = test_db.open();
    let destination = test_db.dir.path().join("backup.db");
    db.backup_to(&destination).await.unwrap();
    // Refuses to overwrite.
    assert!(matches!(
        db.backup_to(&destination).await,
        Err(StorageError::Invalid(_))
    ));
    // Later writes are not in the copy.
    store
        .open(wave2_wavelet1())
        .append(&[create_record()])
        .await
        .unwrap();

    let copy = Database::open(&destination, DatabaseOptions::default()).unwrap();
    let copied = copy.delta_store();
    assert_eq!(copied.wave_ids().await.unwrap(), [wave1_wavelet1().wave_id]);
    assert_eq!(
        copied.open(wave1_wavelet1()).delta(0).await.unwrap(),
        Some(record)
    );
    copy.close().await;
}

#[tokio::test]
async fn corrupt_rows_surface_as_errors_naming_the_wavelet() {
    let (test_db, store, _) = store_with_record(&wave1_wavelet1()).await;
    tamper(&test_db, "UPDATE deltas SET transformed = x'ff00ff'");
    let error = store.open(wave1_wavelet1()).delta(0).await.unwrap_err();
    match error {
        StorageError::Corrupt { wavelet, .. } => assert!(wavelet.contains("wave1"), "{wavelet}"),
        other => panic!("unexpected error {other}"),
    }
    // Cheap version lookups do not decode the body.
    assert!(
        store
            .open(wave1_wavelet1())
            .end_version()
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn passive_checkpoint_moves_the_log_into_the_database() {
    let test_db = TestDb::new();
    let db = test_db.open();
    for i in 0..50 {
        db.set_meta(&format!("key{i}"), "value").await.unwrap();
    }
    let (log, checkpointed) = db.checkpoint().await.unwrap();
    assert!(log > 0);
    // Nothing reads or writes right now, so the whole log is moved.
    assert_eq!(checkpointed, log);
    // The database stays usable, before and after another checkpoint.
    assert_eq!(db.meta("key7").await.unwrap().as_deref(), Some("value"));
    db.checkpoint().await.unwrap();
    db.set_meta("after", "checkpoint").await.unwrap();
    assert_eq!(
        db.meta("after").await.unwrap().as_deref(),
        Some("checkpoint")
    );
    db.close().await;
}

#[tokio::test]
async fn truncation_drops_deltas_and_snapshots_from_a_version_on() {
    let test_db = TestDb::new();
    let store = test_db.open().delta_store();
    let wavelet = store.open(wave1_wavelet1());
    let records = [record_at(0, 1), record_at(2, 2), record_at(4, 3)];
    wavelet.append(&records).await.unwrap();
    for version in [2, 4] {
        let snapshot = StoredSnapshot {
            version: HashedVersion::of(version, [1u8, 2, 3]),
            snapshot: vec![version as u8],
        };
        wavelet.put_snapshot(snapshot, 5).await.unwrap();
    }
    assert_eq!(store.truncate_from(&wave1_wavelet1(), 4).await.unwrap(), 1);
    let wavelet = store.open(wave1_wavelet1());
    assert_eq!(
        wavelet.end_version().await.unwrap().as_ref(),
        Some(records[1].resulting_version())
    );
    assert_eq!(wavelet.delta(4).await.unwrap(), None);
    let snapshots = wavelet.snapshots().await.unwrap();
    assert_eq!(
        snapshots
            .iter()
            .map(|s| s.version.version())
            .collect::<Vec<_>>(),
        [2]
    );
    // The other wavelets and a missing one are left alone.
    assert_eq!(store.truncate_from(&wave2_wavelet1(), 0).await.unwrap(), 0);
    assert_eq!(store.truncate_from(&wave1_wavelet1(), 0).await.unwrap(), 2);
    assert!(store.open(wave1_wavelet1()).is_empty().await.unwrap());
}
