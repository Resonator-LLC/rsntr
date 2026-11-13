//! `resonator federation peers`: listing keys and the admin's decisions.

use iroh::SecretKey;
use resonator_federation::admin::{AdminError, Decision, decide, format_peers};
use resonator_federation::key_domain;
use resonator_server::storage::{Database, DatabaseOptions, PeerStatus};

#[tokio::test]
async fn admins_approve_and_deny_keys_by_key_or_domain() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("wave.db"), DatabaseOptions::default()).unwrap();
    let store = db.federation_store();
    let old = SecretKey::generate().public();
    let new = SecretKey::generate().public();
    let stranger = SecretKey::generate().public();
    store
        .set_peer("b.example", old.as_bytes(), PeerStatus::Trusted, 1_000)
        .await
        .unwrap();
    store
        .set_peer("b.example", new.as_bytes(), PeerStatus::Quarantined, 2_000)
        .await
        .unwrap();
    let listed = format_peers(&store.peers().await.unwrap());
    assert!(
        listed.contains("quarantined") && listed.contains(&key_domain(&new)),
        "{listed}"
    );
    assert!(listed.contains("1970-01-01 00:00:01Z"), "{listed}");

    // Approving the domain trusts its quarantined key and retires the old.
    let changed = decide(&store, "B.example.", Decision::Approve, 3_000)
        .await
        .unwrap();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].key, new.as_bytes().to_vec());
    assert_eq!(changed[0].status, PeerStatus::Trusted);
    let status = |key: iroh::PublicKey| {
        let store = store.clone();
        async move {
            store
                .peers()
                .await
                .unwrap()
                .into_iter()
                .find(|p| p.key == key.as_bytes())
                .map(|p| p.status)
        }
    };
    assert_eq!(status(old).await, Some(PeerStatus::Retired));
    // Nothing is quarantined any more.
    assert!(matches!(
        decide(&store, "b.example", Decision::Approve, 4_000).await,
        Err(AdminError::Usage(_))
    ));
    // Denying the domain denies its trusted key.
    decide(&store, "b.example", Decision::Deny, 5_000)
        .await
        .unwrap();
    assert_eq!(status(new).await, Some(PeerStatus::Denied));
    assert_eq!(status(old).await, Some(PeerStatus::Retired));
    // A key never seen is denied for its own key domain; approved again.
    decide(&store, &key_domain(&stranger), Decision::Deny, 6_000)
        .await
        .unwrap();
    assert_eq!(status(stranger).await, Some(PeerStatus::Denied));
    decide(&store, &key_domain(&stranger), Decision::Approve, 7_000)
        .await
        .unwrap();
    assert_eq!(status(stranger).await, Some(PeerStatus::Trusted));
    // Unknown domains are an error, not a new row.
    assert!(matches!(
        decide(&store, "nobody.example", Decision::Deny, 8_000).await,
        Err(AdminError::Usage(_))
    ));
    db.close().await;
}
