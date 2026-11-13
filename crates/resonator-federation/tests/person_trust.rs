//! Person identity in the binding rules, over a real database: device
//! certificates bind device keys to a person's key domain or to a DNS
//! domain listing the person key, not to other domains; revocation lists
//! withdraw them (no rollback) and survive a restart; own devices speak
//! for the own domain.

use std::sync::Arc;

use resonator_federation::dns::StaticTxtResolver;
use resonator_federation::person::{PersonBook, PersonKey};
use resonator_federation::{Authorisation, Policy, SecretKey, Trust};
use resonator_server::clock::ManualClock;
use resonator_server::storage::{Database, DatabaseOptions};

struct Fixture {
    dir: tempfile::TempDir,
    db: Database,
    dns: StaticTxtResolver,
    trust: Trust,
}

fn trust(db: &Database, dns: &StaticTxtResolver, local: &str) -> Trust {
    Trust::new(
        db.federation_store(),
        Arc::new(dns.clone()),
        Policy::open(),
        [local.to_owned()],
        Arc::new(ManualClock::new(1_000)),
    )
}

fn fixture(local: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("wave.db"), DatabaseOptions::default()).unwrap();
    let dns = StaticTxtResolver::new();
    let trust = trust(&db, &dns, local);
    Fixture {
        dir,
        db,
        dns,
        trust,
    }
}

#[tokio::test]
async fn certified_devices_speak_for_their_person_only() {
    let f = fixture("local.example");
    let (person, _) = PersonKey::generate();
    let (other, _) = PersonKey::generate();
    let device = SecretKey::generate().public();
    let stranger = SecretKey::generate().public();
    let domain = person.domain();

    // Unknown device: not bound.
    assert_eq!(
        f.trust.check(&device, &domain, &[]).await.unwrap(),
        Authorisation::NotBound
    );
    f.trust
        .learn_cert(&person.issue_cert(&device, "laptop", false, 1))
        .await
        .unwrap();
    assert_eq!(
        f.trust.check(&device, &domain, &[]).await.unwrap(),
        Authorisation::Authorised
    );
    assert!(f.trust.verify_signer(&device, &domain).await.unwrap());
    // The wrong domain: another person's.
    assert_eq!(
        f.trust.check(&device, &other.domain(), &[]).await.unwrap(),
        Authorisation::NotBound
    );
    assert!(
        !f.trust
            .verify_signer(&device, &other.domain())
            .await
            .unwrap()
    );
    // A certificate for another device does not help this one.
    assert_eq!(
        f.trust.check(&stranger, &domain, &[]).await.unwrap(),
        Authorisation::NotBound
    );
    // Not for the local domain either (not this node's person).
    assert_eq!(
        f.trust.check(&device, "local.example", &[]).await.unwrap(),
        Authorisation::NotAllowed
    );
    // A certificate whose signature is not the person key's is refused.
    let mut forged = other.issue_cert(&stranger, "x", true, 1);
    forged.cert = person.issue_cert(&stranger, "x", true, 1).cert;
    assert!(f.trust.learn_cert(&forged).await.is_err());
    assert_eq!(
        f.trust.check(&stranger, &domain, &[]).await.unwrap(),
        Authorisation::NotBound
    );
    f.db.close().await;
}

#[tokio::test]
async fn dns_domains_bind_devices_through_the_person_key() {
    let f = fixture("local.example");
    let (person, _) = PersonKey::generate();
    let device = SecretKey::generate().public();
    f.trust
        .learn_cert(&person.issue_cert(&device, "", false, 1))
        .await
        .unwrap();
    // The domain's record lists another key: not bound. (Without any
    // record, open mode would pin the domain to the first key.)
    f.dns
        .set_keys("alice.example", [&SecretKey::generate().public()]);
    assert_eq!(
        f.trust.check(&device, "alice.example", &[]).await.unwrap(),
        Authorisation::NotBound
    );
    f.dns.set_keys("alice.example", [&person.public()]);
    assert_eq!(
        f.trust.check(&device, "alice.example", &[]).await.unwrap(),
        Authorisation::Authorised
    );
    assert_eq!(f.trust.devices_of("alice.example"), vec![device]);
    // Nothing pinned: pins are for server keys.
    assert!(f.db.federation_store().peers().await.unwrap().is_empty());
    f.db.close().await;
}

#[tokio::test]
async fn revocations_withdraw_devices_but_not_their_history() {
    let f = fixture("local.example");
    let (person, _) = PersonKey::generate();
    let laptop = SecretKey::generate().public();
    let phone = SecretKey::generate().public();
    let domain = person.domain();
    for (device, name) in [(laptop, "laptop"), (phone, "phone")] {
        f.trust
            .learn_cert(&person.issue_cert(&device, name, false, 1))
            .await
            .unwrap();
    }
    let first = person.revoke(None, [phone], 2);
    let learned = f.trust.learn_revocations(&first).await.unwrap().unwrap();
    assert_eq!(learned.seq, 1);
    assert_eq!(
        f.trust.check(&phone, &domain, &[]).await.unwrap(),
        Authorisation::Revoked
    );
    assert!(!f.trust.verify_signer(&phone, &domain).await.unwrap());
    assert!(
        f.trust
            .verify_historical_signer(&phone, &domain)
            .await
            .unwrap()
    );
    assert!(f.trust.is_revoked(&phone));
    assert_eq!(
        f.trust.check(&laptop, &domain, &[]).await.unwrap(),
        Authorisation::Authorised
    );
    // Same seq again: nothing new.
    assert!(f.trust.learn_revocations(&first).await.unwrap().is_none());
    let second = person.revoke(Some(&learned), [laptop], 3);
    assert_eq!(
        f.trust
            .learn_revocations(&second)
            .await
            .unwrap()
            .unwrap()
            .seq,
        2
    );
    // Rollback to seq 1 changes nothing: the laptop stays revoked.
    assert!(f.trust.learn_revocations(&first).await.unwrap().is_none());
    assert!(f.trust.is_revoked(&laptop));

    // After a restart, storage knows it all.
    let again = trust(&f.db, &f.dns, "local.example");
    let loaded = again.load_persons().await.unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].seq, 2);
    assert!(again.is_revoked(&phone) && again.is_revoked(&laptop));
    assert_eq!(
        again.check(&phone, &domain, &[]).await.unwrap(),
        Authorisation::Revoked
    );
    drop(f.dir);
}

#[tokio::test]
async fn own_devices_speak_for_the_own_domain() {
    let (person, _) = PersonKey::generate();
    let f = fixture(&person.domain());
    let persons = PersonBook::new();
    persons.set_own(person.public());
    let trust = f.trust.clone().with_persons(persons);
    let device = SecretKey::generate().public();
    let (other, _) = PersonKey::generate();
    let impostor = SecretKey::generate().public();
    trust
        .learn_cert(&person.issue_cert(&device, "", false, 1))
        .await
        .unwrap();
    trust
        .learn_cert(&other.issue_cert(&impostor, "", false, 1))
        .await
        .unwrap();
    assert_eq!(
        trust.check(&device, &person.domain(), &[]).await.unwrap(),
        Authorisation::Authorised
    );
    // Another person's device claiming this person's domain.
    assert_eq!(
        trust.check(&impostor, &person.domain(), &[]).await.unwrap(),
        Authorisation::NotAllowed
    );
    f.db.close().await;
}
