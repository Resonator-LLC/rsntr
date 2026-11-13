//! The durable parts of the wave operations: the outbox coalesces
//! notifications per domain and wavelet and keeps what was sent, and
//! applied request ids are remembered with their answers.

use resonator_server::storage::{Database, DatabaseOptions, OutboxStatus};

const WAVELET: &str = "wave://a.example/w+1/conv+root";

fn db() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("wave.db"), DatabaseOptions::default()).unwrap();
    (dir, db)
}

#[tokio::test]
async fn notifications_coalesce_and_keep_their_progress() {
    let (_dir, db) = db();
    let store = db.federation_store();
    store
        .outbox_enqueue("b.example", WAVELET, 5, "r1", 1_000)
        .await
        .unwrap();
    store
        .outbox_enqueue("b.example", WAVELET, 3, "r2", 1_100)
        .await
        .unwrap();
    store
        .outbox_enqueue("c.example", WAVELET, 3, "r3", 1_100)
        .await
        .unwrap();
    let due = store.outbox_due(2_000, 10).await.unwrap();
    assert_eq!(due.len(), 2);
    let b = due.iter().find(|e| e.domain == "b.example").unwrap();
    assert_eq!(
        (b.from_version, b.request_id.as_str(), b.pending_since),
        (3, "r2", Some(1_000))
    );

    // A failed attempt after some deltas got through: not due before the
    // retry time, and those deltas are not sent again.
    store
        .outbox_retry("b.example", WAVELET, 4, "r4", 5_000)
        .await
        .unwrap();
    assert!(
        store
            .outbox_due(2_000, 10)
            .await
            .unwrap()
            .iter()
            .all(|e| e.domain != "b.example")
    );
    let b = store
        .outbox_due(5_000, 10)
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.domain == "b.example")
        .unwrap();
    assert_eq!(
        (b.from_version, b.attempts, b.request_id.as_str()),
        (4, 1, "r4")
    );

    // Delivered under the id it was sent with.
    store
        .outbox_delivered("b.example", WAVELET, "r4", 9, 5_100)
        .await
        .unwrap();
    // A notification queued while one was on its way is not lost.
    store
        .outbox_enqueue("c.example", WAVELET, 7, "r5", 5_100)
        .await
        .unwrap();
    store
        .outbox_delivered("c.example", WAVELET, "r3", 7, 5_200)
        .await
        .unwrap();
    let entries = store.outbox_entries().await.unwrap();
    let status = |domain: &str| {
        let entry = entries.iter().find(|e| e.domain == domain).unwrap();
        (entry.status, entry.from_version)
    };
    assert_eq!(status("b.example"), (OutboxStatus::Delivered, 9));
    assert_eq!(status("c.example"), (OutboxStatus::Pending, 7));

    // A new notification after delivery starts afresh.
    store
        .outbox_enqueue("b.example", WAVELET, 9, "r6", 6_000)
        .await
        .unwrap();
    let b = store
        .outbox_entries()
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.domain == "b.example")
        .unwrap();
    assert_eq!(
        (b.status, b.attempts, b.pending_since),
        (OutboxStatus::Pending, 0, Some(6_000))
    );

    store.outbox_expire("b.example", WAVELET).await.unwrap();
    assert!(
        store
            .outbox_due(i64::MAX, 10)
            .await
            .unwrap()
            .iter()
            .all(|e| e.domain != "b.example")
    );
}

#[tokio::test]
async fn applied_requests_are_remembered_once_and_pruned() {
    let (_dir, db) = db();
    let store = db.federation_store();
    assert_eq!(store.applied_request("x").await.unwrap(), None);
    store
        .record_applied("x", "b.example", 1_000, Some(vec![1, 2]))
        .await
        .unwrap();
    store
        .record_applied("x", "c.example", 1_500, Some(vec![3]))
        .await
        .unwrap();
    assert_eq!(
        store.applied_request("x").await.unwrap(),
        Some(("b.example".to_owned(), Some(vec![1, 2])))
    );
    assert_eq!(store.prune_applied(1_000).await.unwrap(), 0);
    assert_eq!(store.prune_applied(1_001).await.unwrap(), 1);
    assert_eq!(store.applied_request("x").await.unwrap(), None);
}
