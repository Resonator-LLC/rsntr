//! The binding rules over a real database: key domains, trust on first
//! use of TXT records, key rotation with and without a handover statement,
//! quarantine, admin decisions and the allowlist.

use std::sync::Arc;

use resonator_federation::dns::StaticTxtResolver;
use resonator_federation::rotation::sign_rotation;
use resonator_federation::{Authorisation, Policy, SecretKey, Trust, key_domain};
use resonator_server::clock::ManualClock;
use resonator_server::storage::{Database, DatabaseOptions, PeerStatus};

struct Fixture {
    _dir: tempfile::TempDir,
    db: Database,
    dns: StaticTxtResolver,
    trust: Trust,
}

fn fixture(policy: Policy) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("wave.db"), DatabaseOptions::default()).unwrap();
    let dns = StaticTxtResolver::new();
    let trust = Trust::new(
        db.federation_store(),
        Arc::new(dns.clone()),
        policy,
        ["local.example".to_owned()],
        Arc::new(ManualClock::new(1_000)),
    );
    Fixture {
        _dir: dir,
        db,
        dns,
        trust,
    }
}

#[tokio::test]
async fn key_domains_bind_to_their_own_key_only() {
    let f = fixture(Policy::open());
    let key = SecretKey::generate().public();
    let other = SecretKey::generate().public();
    assert!(f.trust.authorise(&key, &key_domain(&key)).await);
    assert!(!f.trust.authorise(&other, &key_domain(&key)).await);
    assert_eq!(f.trust.resolve(&key_domain(&key)).await.unwrap(), Some(key));
    assert!(
        f.trust
            .is_signer_bound(&key, &key_domain(&key))
            .await
            .unwrap()
    );
    assert!(
        !f.trust
            .is_signer_bound(&other, &key_domain(&key))
            .await
            .unwrap()
    );
    // Nothing is recorded for key domains.
    assert!(f.db.federation_store().peers().await.unwrap().is_empty());
    f.db.close().await;
}

#[tokio::test]
async fn dns_domains_are_pinned_on_first_use() {
    let f = fixture(Policy::allowlist(["b.example"]));
    let key = SecretKey::generate().public();
    // No record: not bound, nothing pinned.
    assert!(!f.trust.authorise(&key, "b.example").await);
    assert_eq!(f.trust.resolve("b.example").await.unwrap(), None);
    f.dns.set_keys("b.example", [&key]);
    assert_eq!(f.trust.resolve("b.example").await.unwrap(), Some(key));
    assert!(f.trust.authorise(&key, "b.example").await);
    let pinned =
        f.db.federation_store()
            .trusted_key("b.example")
            .await
            .unwrap()
            .unwrap();
    assert_eq!(pinned.key, key.as_bytes());
    assert_eq!(pinned.status, PeerStatus::Trusted);
    // Once pinned, the binding no longer depends on DNS.
    f.dns.set_records("b.example", Vec::new());
    f.dns.set_failing(true);
    assert!(f.trust.authorise(&key, "b.example").await);
    assert_eq!(f.trust.resolve("b.example").await.unwrap(), Some(key));
    assert!(f.trust.is_signer_bound(&key, "b.example").await.unwrap());
    f.db.close().await;
}

#[tokio::test]
async fn open_mode_pins_a_domain_without_a_record_to_its_first_key() {
    let f = fixture(Policy::open());
    let first = SecretKey::generate().public();
    let second = SecretKey::generate().public();
    // A domain that publishes a record is held to it.
    f.dns.set_keys("b.example", [&second]);
    assert_eq!(
        f.trust.check(&first, "b.example", &[]).await.unwrap(),
        Authorisation::NotBound
    );
    // No record: the first key that claims the domain is pinned.
    assert_eq!(
        f.trust.check(&first, "relayhost", &[]).await.unwrap(),
        Authorisation::Authorised
    );
    let pinned =
        f.db.federation_store()
            .trusted_key("relayhost")
            .await
            .unwrap()
            .unwrap();
    assert_eq!(pinned.key, first.as_bytes());
    assert_eq!(f.trust.resolve("relayhost").await.unwrap(), Some(first));
    assert!(f.trust.is_signer_bound(&first, "relayhost").await.unwrap());
    // Another key claiming it later is quarantined; the first keeps it.
    assert_eq!(
        f.trust.check(&second, "relayhost", &[]).await.unwrap(),
        Authorisation::Quarantined
    );
    assert!(f.trust.authorise(&first, "relayhost").await);
    // Never this server's own domain.
    assert_eq!(
        f.trust.check(&first, "local.example", &[]).await.unwrap(),
        Authorisation::NotAllowed
    );
    f.db.close().await;
}

#[tokio::test]
async fn a_rotation_needs_the_record_and_the_old_keys_statement() {
    let f = fixture(Policy::open());
    let old = SecretKey::generate();
    let new = SecretKey::generate().public();
    f.dns.set_keys("b.example", [&old.public()]);
    assert!(f.trust.authorise(&old.public(), "b.example").await);

    // The new key is published but no statement is offered: quarantined.
    f.dns.set_keys("b.example", [&old.public(), &new]);
    assert_eq!(
        f.trust.check(&new, "b.example", &[]).await.unwrap(),
        Authorisation::Quarantined
    );
    // A statement for another domain, or signed by the wrong key, does not
    // help.
    let wrong_domain = sign_rotation(&old, "c.example", &new, 2_000);
    let wrong_signer = sign_rotation(&SecretKey::generate(), "b.example", &new, 2_000);
    assert_eq!(
        f.trust
            .check(&new, "b.example", &[wrong_domain, wrong_signer])
            .await
            .unwrap(),
        Authorisation::Quarantined
    );
    // The old key still works meanwhile.
    assert!(f.trust.authorise(&old.public(), "b.example").await);

    // With the statement but without the record: still quarantined.
    let statement = sign_rotation(&old, "b.example", &new, 2_000);
    f.dns.set_keys("b.example", [&old.public()]);
    assert_eq!(
        f.trust
            .check(&new, "b.example", std::slice::from_ref(&statement))
            .await
            .unwrap(),
        Authorisation::Quarantined
    );

    // Record and statement: accepted, the old key retired.
    f.dns.set_keys("b.example", [&new]);
    assert_eq!(
        f.trust
            .check(&new, "b.example", &[statement])
            .await
            .unwrap(),
        Authorisation::Authorised
    );
    let rows =
        f.db.federation_store()
            .peer_keys("b.example")
            .await
            .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].key, new.as_bytes());
    assert_eq!(rows[0].status, PeerStatus::Trusted);
    assert_eq!(rows[1].status, PeerStatus::Retired);
    assert!(!f.trust.authorise(&old.public(), "b.example").await);
    // Deltas the old key signed before the rotation still verify.
    assert!(
        f.trust
            .is_signer_bound(&old.public(), "b.example")
            .await
            .unwrap()
    );
    f.db.close().await;
}

#[tokio::test]
async fn admins_decide_on_quarantined_keys() {
    let f = fixture(Policy::open());
    let old = SecretKey::generate().public();
    let new = SecretKey::generate().public();
    f.dns.set_keys("b.example", [&old]);
    assert!(f.trust.authorise(&old, "b.example").await);
    f.dns.set_keys("b.example", [&new]);
    assert!(!f.trust.authorise(&new, "b.example").await);
    f.trust.decide("b.example", &new, true).await.unwrap();
    assert!(f.trust.authorise(&new, "b.example").await);
    assert!(!f.trust.authorise(&old, "b.example").await);
    f.trust.decide("b.example", &new, false).await.unwrap();
    assert_eq!(
        f.trust.check(&new, "b.example", &[]).await.unwrap(),
        Authorisation::Denied
    );
    f.db.close().await;
}

#[tokio::test]
async fn the_allowlist_and_own_domains_are_enforced() {
    let key = SecretKey::generate().public();
    let listed = SecretKey::generate().public();
    let f = fixture(Policy::allowlist([
        "b.example".to_owned(),
        key_domain(&listed),
    ]));
    f.dns.set_keys("b.example", [&key]);
    f.dns.set_keys("c.example", [&key]);
    assert!(f.trust.authorise(&key, "b.example").await);
    assert_eq!(
        f.trust.check(&key, "c.example", &[]).await.unwrap(),
        Authorisation::NotAllowed
    );
    assert!(f.trust.authorise(&listed, &key_domain(&listed)).await);
    assert!(!f.trust.authorise(&key, &key_domain(&key)).await);
    // Nobody speaks for this server's own domain.
    f.dns.set_keys("local.example", [&key]);
    let open = fixture(Policy::open());
    open.dns.set_keys("local.example", [&key]);
    assert_eq!(
        open.trust.check(&key, "local.example", &[]).await.unwrap(),
        Authorisation::NotAllowed
    );
    // A DNS failure is no verdict; the binding check says no.
    open.dns.set_failing(true);
    assert!(open.trust.check(&key, "d.example", &[]).await.is_err());
    assert!(!open.trust.authorise(&key, "d.example").await);
    f.db.close().await;
    open.db.close().await;
}
