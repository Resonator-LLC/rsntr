//! Tests of the account store, porting `AccountStoreTestBase` of Apache
//! Wave.

mod common;

use argon2::Params;
use common::{TestDb, participant};
use resonator_model::id::ParticipantId;
use resonator_server::account::{
    AccountData, AccountError, Capability, Context, EventType, HumanAccountData, PasswordDigest,
    ProtocolVersion, RobotAccountData, RobotCapabilities, RobotKind,
};
use resonator_server::storage::StorageError;

fn human_id() -> ParticipantId {
    participant("human@example.com")
}

fn robot_id() -> ParticipantId {
    participant("robot@example.com")
}

fn robot_account() -> AccountData {
    RobotAccountData::robot(
        robot_id(),
        "example.com",
        Some("tokenhash".to_owned()),
        None,
        false,
    )
    .unwrap()
    .into()
}

/// A few capabilities with a mix of field values.
fn updated_robot_account() -> AccountData {
    let capabilities = RobotCapabilities::new(
        [
            Capability::new(EventType::WaveletBlipCreated),
            Capability::with_contexts(EventType::DocumentChanged, vec![Context::Siblings]),
            Capability::with_filter(
                EventType::BlipSubmitted,
                vec![Context::Siblings, Context::Parent],
                "blah",
            ),
        ],
        "FAKEHASH",
        ProtocolVersion::DEFAULT,
    );
    RobotAccountData::robot(
        robot_id(),
        "example.com",
        Some("tokenhash".to_owned()),
        Some(capabilities),
        true,
    )
    .unwrap()
    .into()
}

/// Cheap parameters so the tests do not spend their time hashing.
fn quick_digest(password: &str) -> PasswordDigest {
    PasswordDigest::with_params(password, Params::new(64, 1, 1, None).unwrap()).unwrap()
}

#[tokio::test]
async fn roundtrip_human_account() {
    let test_db = TestDb::new();
    let store = test_db.open().account_store();
    let account: AccountData = HumanAccountData::new(human_id()).into();
    store.put_account(account.clone()).await.unwrap();
    assert_eq!(store.get_account(&human_id()).await.unwrap(), Some(account));
}

#[tokio::test]
async fn roundtrip_human_account_with_password() {
    let test_db = TestDb::new();
    let store = test_db.open().account_store();
    let mut account = HumanAccountData::with_password(human_id(), quick_digest("internet"));
    account.locale = Some("de".to_owned());
    store.put_account(account.clone().into()).await.unwrap();
    let retrieved = store.get_account(&human_id()).await.unwrap().unwrap();
    assert!(retrieved.is_human() && !retrieved.is_robot());
    let human = retrieved.as_human().unwrap();
    let digest = human.password_digest.as_ref().unwrap();
    assert!(digest.verify("internet"));
    assert!(!digest.verify("intranet"));
    assert!(digest.as_phc().starts_with("$argon2id$"));
    assert_eq!(human.locale.as_deref(), Some("de"));
    assert_eq!(human, &account);
}

#[tokio::test]
async fn roundtrip_robot_account() {
    let test_db = TestDb::new();
    let store = test_db.open().account_store();
    store.put_account(robot_account()).await.unwrap();
    let retrieved = store.get_account(&robot_id()).await.unwrap().unwrap();
    assert_eq!(retrieved, robot_account());
    assert!(retrieved.is_robot() && !retrieved.is_agent());
    assert_eq!(retrieved.as_robot().unwrap().url(), Some("example.com"));
}

#[tokio::test]
async fn get_missing_account_returns_none() {
    let test_db = TestDb::new();
    let store = test_db.open().account_store();
    assert_eq!(store.get_account(&human_id()).await.unwrap(), None);
}

#[tokio::test]
async fn put_account_overrides() {
    let test_db = TestDb::new();
    let store = test_db.open().account_store();
    store.put_account(robot_account()).await.unwrap();
    assert_eq!(
        store.get_account(&robot_id()).await.unwrap(),
        Some(robot_account())
    );
    store.put_account(updated_robot_account()).await.unwrap();
    let updated = store.get_account(&robot_id()).await.unwrap().unwrap();
    assert_eq!(updated, updated_robot_account());
    let capabilities = updated.as_robot().unwrap().capabilities.as_ref().unwrap();
    assert_eq!(capabilities.capabilities_hash, "FAKEHASH");
    assert_eq!(capabilities.protocol_version, ProtocolVersion::V2_2);
    assert_eq!(
        capabilities.capabilities[&EventType::WaveletBlipCreated].contexts,
        Capability::DEFAULT_CONTEXTS
    );
    assert_eq!(
        capabilities.capabilities[&EventType::BlipSubmitted].filter,
        "blah"
    );
    // And back to an account without capabilities.
    store.put_account(robot_account()).await.unwrap();
    assert_eq!(
        store.get_account(&robot_id()).await.unwrap(),
        Some(robot_account())
    );
    assert_eq!(store.account_count().await.unwrap(), 1);
}

#[tokio::test]
async fn put_account_can_change_type() {
    let test_db = TestDb::new();
    let store = test_db.open().account_store();
    store.put_account(updated_robot_account()).await.unwrap();
    let converted: AccountData = HumanAccountData::new(robot_id()).into();
    store.put_account(converted.clone()).await.unwrap();
    assert_eq!(
        store.get_account(&robot_id()).await.unwrap(),
        Some(converted)
    );
}

#[tokio::test]
async fn remove_account() {
    let test_db = TestDb::new();
    let store = test_db.open().account_store();
    store.put_account(updated_robot_account()).await.unwrap();
    store.remove_account(&robot_id()).await.unwrap();
    assert_eq!(store.get_account(&robot_id()).await.unwrap(), None);
    // Removing a missing account is not an error.
    store.remove_account(&robot_id()).await.unwrap();
    assert_eq!(store.account_count().await.unwrap(), 0);
}

// Beyond the original test base.

#[tokio::test]
async fn agent_accounts_have_no_url() {
    let test_db = TestDb::new();
    let store = test_db.open().account_store();
    let id = participant("helper@example.com");
    let mut agent = RobotAccountData::agent(id.clone());
    agent.access_token_hash = Some("hash".to_owned());
    store.put_account(agent.clone().into()).await.unwrap();
    let retrieved = store.get_account(&id).await.unwrap().unwrap();
    assert!(retrieved.is_robot() && retrieved.is_agent());
    let robot = retrieved.as_robot().unwrap();
    assert_eq!(robot.kind, RobotKind::Agent);
    assert_eq!(robot.url(), None);
    assert!(robot.is_verified);
    assert_eq!(robot, &agent);
}

#[tokio::test]
async fn create_account_does_not_replace() {
    let test_db = TestDb::new();
    let store = test_db.open().account_store();
    assert!(store.create_account(robot_account()).await.unwrap());
    let other: AccountData = HumanAccountData::new(robot_id()).into();
    assert!(!store.create_account(other).await.unwrap());
    assert_eq!(
        store.get_account(&robot_id()).await.unwrap(),
        Some(robot_account())
    );
}

#[tokio::test]
async fn accounts_survive_reopening() {
    let test_db = TestDb::new();
    let db = test_db.open();
    db.account_store()
        .put_account(updated_robot_account())
        .await
        .unwrap();
    db.close().await;
    let db = test_db.open();
    assert_eq!(
        db.account_store().get_account(&robot_id()).await.unwrap(),
        Some(updated_robot_account())
    );
}

#[tokio::test]
async fn corrupt_account_rows_are_errors() {
    let test_db = TestDb::new();
    let db = test_db.open();
    db.account_store()
        .put_account(HumanAccountData::new(human_id()).into())
        .await
        .unwrap();
    rusqlite::Connection::open(&test_db.path)
        .unwrap()
        .execute_batch("UPDATE accounts SET password_hash = 'not a hash'")
        .unwrap();
    assert!(matches!(
        db.account_store().get_account(&human_id()).await,
        Err(StorageError::CorruptAccount { .. })
    ));
}

#[test]
fn robot_url_must_not_end_with_slash() {
    assert_eq!(
        RobotAccountData::robot(robot_id(), "http://example.com/", None, None, false),
        Err(AccountError::UrlEndsWithSlash)
    );
}

#[test]
fn default_password_digest_is_argon2id_and_salted() {
    let first = PasswordDigest::new("internet").unwrap();
    let second = PasswordDigest::new("internet").unwrap();
    assert!(first.as_phc().starts_with("$argon2id$v=19$"));
    assert_ne!(first.as_phc(), second.as_phc());
    assert!(first.verify("internet") && second.verify("internet"));
    assert!(!first.verify(""));
    assert_eq!(PasswordDigest::from_phc(first.as_phc()).unwrap(), first);
    assert!(PasswordDigest::from_phc("plaintext").is_err());
    assert!(!format!("{first:?}").contains("argon2"));
}

#[test]
fn enum_names_follow_the_original() {
    assert_eq!(EventType::WaveletSelfAdded.as_str(), "WAVELET_SELF_ADDED");
    assert_eq!("SELF".parse::<Context>().unwrap(), Context::SelfOnly);
    assert_eq!(
        "0.21".parse::<ProtocolVersion>().unwrap(),
        ProtocolVersion::V2_1
    );
    assert!("NOPE".parse::<EventType>().is_err());
}
