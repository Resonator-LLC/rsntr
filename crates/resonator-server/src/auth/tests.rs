//! Tests of the authentication service.
//!
//! Ports the cases of `AccountStoreLoginModuleTest`, `SessionManagerTest`
//! and `UserRegistrationServletTest` of Apache Wave that apply to this
//! design, and adds tests of sessions, tokens and rate limiting.

use std::net::{IpAddr, Ipv4Addr};

use tempfile::TempDir;

use super::*;
use crate::account::RobotAccountData;
use crate::clock::ManualClock;
use crate::storage::DatabaseOptions;

const DOMAIN: &str = "example.com";
const CLIENT: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
const OTHER_CLIENT: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2));
const START: i64 = 1_000_000_000;

struct Fixture {
    _dir: TempDir,
    db: Database,
    clock: Arc<ManualClock>,
    auth: AuthService,
}

fn settings() -> AuthSettings {
    AuthSettings {
        // Hashing as cheap as argon2 allows: these tests are not about it.
        password_params: argon2::Params::new(8, 1, 1, None).unwrap(),
        session_lifetime_millis: 60 * 60 * 1000,
        session_absolute_lifetime_millis: 10 * 60 * 60 * 1000,
        login_backoff: BackoffSettings {
            max_failures: 3,
            base_millis: 1_000,
            max_millis: 60_000,
        },
        register_max_per_hour: 0,
        ..AuthSettings::new(DOMAIN)
    }
}

impl Fixture {
    fn with(settings: AuthSettings) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("test.db"), DatabaseOptions::default()).unwrap();
        let clock = Arc::new(ManualClock::new(START));
        let auth = AuthService::with_clock(&db, settings, clock.clone());
        Fixture {
            _dir: dir,
            db,
            clock,
            auth,
        }
    }

    fn new() -> Self {
        Self::with(settings())
    }

    async fn register(&self, name: &str, password: &str) -> ParticipantId {
        self.auth
            .register(Some(name), password, CLIENT, None)
            .await
            .unwrap()
    }

    /// Lets spawned bookkeeping writes (session renewal, token use) land.
    async fn settle(&self) {
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        self.db.flush().await.unwrap();
    }
}

fn id(address: &str) -> ParticipantId {
    ParticipantId::of(address).unwrap()
}

// ---- Registration (UserRegistrationServletTest) ----

#[tokio::test]
async fn register_new_user() {
    let fx = Fixture::new();
    let registered = fx.register("foo@example.com", "internet").await;
    assert_eq!(registered, id("foo@example.com"));
    let account = fx.auth.accounts().get_account(&registered).await.unwrap();
    let digest = account.unwrap().as_human().unwrap().password_digest.clone();
    assert!(digest.unwrap().verify("internet"));
}

#[tokio::test]
async fn register_new_user_disabled() {
    let fx = Fixture::with(AuthSettings {
        registration_enabled: false,
        admin: Some(id("admin@example.com")),
        ..settings()
    });
    let error = fx
        .auth
        .register(Some("foo@example.com"), "internet", CLIENT, None)
        .await
        .unwrap_err();
    assert!(matches!(error, AuthError::RegistrationDisabled));
    assert_eq!(fx.auth.accounts().account_count().await.unwrap(), 0);
    // Nor for a signed-in user who is not the admin.
    let somebody = Identity::internal(id("somebody@example.com"));
    assert!(matches!(
        fx.auth
            .register(Some("foo"), "internet", CLIENT, Some(&somebody))
            .await,
        Err(AuthError::RegistrationDisabled)
    ));
    // The admin may register accounts all the same.
    let admin = Identity::internal(id("admin@example.com"));
    let registered = fx
        .auth
        .register(Some("foo"), "internet", CLIENT, Some(&admin))
        .await
        .unwrap();
    assert_eq!(registered, id("foo@example.com"));
    // But not with a read-only credential.
    let read_only = Identity {
        scopes: Scopes::READ,
        ..admin
    };
    assert!(matches!(
        fx.auth
            .register(Some("bar"), "internet", CLIENT, Some(&read_only))
            .await,
        Err(AuthError::RegistrationDisabled)
    ));
}

#[tokio::test]
async fn domain_inserted_automatically_and_username_trimmed() {
    let fx = Fixture::new();
    assert_eq!(fx.register("sam", "fdsa4321").await, id("sam@example.com"));
    assert_eq!(
        fx.register(" ben@example.com ", "beetleguice").await,
        id("ben@example.com")
    );
    assert_eq!(
        fx.register("Frodo", "password").await,
        id("frodo@example.com")
    );
}

#[tokio::test]
async fn register_existing_user_is_an_error() {
    let fx = Fixture::new();
    fx.register("frodo@example.com", "password").await;
    for name in ["frodo@example.com", "frodo", " FRODO "] {
        let error = fx
            .auth
            .register(Some(name), "asdf1234", CLIENT, None)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "Account already exists");
    }
    // The existing account is untouched.
    assert!(fx.auth.sign_in("frodo", "password", CLIENT).await.is_ok());
}

#[tokio::test]
async fn register_user_at_foreign_domain_is_an_error() {
    let fx = Fixture::new();
    let error = fx
        .auth
        .register(Some("bilbo@example2.com"), "fdsa4321", CLIENT, None)
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Invalid address 'bilbo@example2.com': You can only create users at the example.com domain"
    );
    assert_eq!(fx.auth.accounts().account_count().await.unwrap(), 0);
}

#[tokio::test]
async fn invalid_usernames_and_passwords_are_rejected() {
    let fx = Fixture::new();
    for name in [
        None,
        Some(""),
        Some("a b"),
        Some("a+b"),
        Some("@example.com"),
    ] {
        assert!(
            matches!(
                fx.auth.register(name, "password", CLIENT, None).await,
                Err(AuthError::InvalidAddress(_))
            ),
            "{name:?}"
        );
    }
    // The original accepts any password, even none; this server has a
    // configurable minimum.
    assert!(matches!(
        fx.auth.register(Some("zd"), "", CLIENT, None).await,
        Err(AuthError::PasswordTooShort { min: 8 })
    ));
    assert!(matches!(
        fx.auth.register(Some("zd"), "1234567", CLIENT, None).await,
        Err(AuthError::PasswordTooShort { min: 8 })
    ));
    let long = "x".repeat(MAX_PASSWORD_BYTES + 1);
    assert!(matches!(
        fx.auth.register(Some("zd"), &long, CLIENT, None).await,
        Err(AuthError::PasswordTooLong)
    ));
    assert_eq!(fx.auth.accounts().account_count().await.unwrap(), 0);
}

#[tokio::test]
async fn empty_password_works_when_allowed() {
    // `testNullPasswordWorks`.
    let fx = Fixture::with(AuthSettings {
        password_min_length: 0,
        ..settings()
    });
    fx.register("zd@example.com", "").await;
    assert!(fx.auth.sign_in("zd", "", CLIENT).await.is_ok());
}

#[tokio::test]
async fn registrations_are_rate_limited_per_client() {
    let fx = Fixture::with(AuthSettings {
        register_max_per_hour: 2,
        ..settings()
    });
    fx.register("one", "password").await;
    fx.register("two", "password").await;
    assert!(matches!(
        fx.auth
            .register(Some("three"), "password", CLIENT, None)
            .await,
        Err(AuthError::RateLimited { .. })
    ));
    // Another client is not affected, and the window passes.
    assert!(
        fx.auth
            .register(Some("three"), "password", OTHER_CLIENT, None)
            .await
            .is_ok()
    );
    fx.clock.advance(60 * 60 * 1000);
    fx.register("four", "password").await;
}

// ---- Sign-in (AccountStoreLoginModuleTest, AuthenticationServletTest) ----

#[tokio::test]
async fn correct_password_starts_a_session() {
    let fx = Fixture::new();
    fx.register("haspwd", "internet").await;
    let grant = fx
        .auth
        .sign_in("haspwd@example.com", "internet", CLIENT)
        .await
        .unwrap();
    assert_eq!(grant.identity.participant, id("haspwd@example.com"));
    assert_eq!(grant.identity.scopes, Scopes::ALL);
    assert!(grant.identity.is_session());
    assert_eq!(grant.max_age_secs, 60 * 60);
    // 256 random bits, URL-safe.
    assert_eq!(grant.session_id.len(), 43);
    let session = fx
        .auth
        .authenticate_session(&grant.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.identity, grant.identity);
    assert!(!session.renewed);
    // Each sign-in gets its own session.
    let second = fx.auth.sign_in("haspwd", "internet", CLIENT).await.unwrap();
    assert_ne!(second.session_id, grant.session_id);
}

#[tokio::test]
async fn missing_domain_is_added_automatically() {
    let fx = Fixture::new();
    fx.register("haspwd", "internet").await;
    for name in ["haspwd", "HasPwd", " haspwd@Example.com "] {
        let grant = fx.auth.sign_in(name, "internet", CLIENT).await.unwrap();
        assert_eq!(grant.identity.participant, id("haspwd@example.com"));
    }
}

#[tokio::test]
async fn incorrect_credentials_are_refused_alike() {
    let fx = Fixture::new();
    fx.register("haspwd", "internet").await;
    // A human account without a password and a robot account.
    fx.auth
        .accounts()
        .put_account(HumanAccountData::new(id("nopwd@example.com")).into())
        .await
        .unwrap();
    fx.auth
        .accounts()
        .put_account(RobotAccountData::agent(id("robot@example.com")).into())
        .await
        .unwrap();
    for (name, password) in [
        ("haspwd", "wrongpassword"),
        ("haspwd", ""),
        ("madeup@example.com", "internet"),
        ("nopwd", ""),
        ("nopwd", "internet"),
        ("robot", ""),
        ("@example.com", "internet"),
        ("a@b@c", "internet"),
        ("", ""),
        ("haspwd@other.com", "internet"),
    ] {
        let error = fx
            .auth
            .sign_in(name, password, OTHER_CLIENT)
            .await
            .unwrap_err();
        assert!(
            matches!(error, AuthError::InvalidCredentials),
            "{name:?}: {error:?}"
        );
        assert_eq!(
            error.to_string(),
            "The username or password you entered is incorrect."
        );
        // Keep the per-client limiter out of this test.
        fx.clock.advance(120_000);
    }
    // An over-long password is refused without being looked at.
    let long = "x".repeat(MAX_PASSWORD_BYTES + 1);
    assert!(fx.auth.sign_in("haspwd", &long, CLIENT).await.is_err());
}

#[tokio::test]
async fn failed_sign_ins_back_off_per_account_and_client() {
    let fx = Fixture::new();
    fx.register("frodo", "password").await;
    for _ in 0..3 {
        assert!(matches!(
            fx.auth.sign_in("frodo", "wrong", CLIENT).await,
            Err(AuthError::InvalidCredentials)
        ));
    }
    // Now even the right password has to wait.
    let error = fx.auth.sign_in("frodo", "password", CLIENT).await;
    assert!(matches!(
        error,
        Err(AuthError::RateLimited {
            retry_after_secs: 1
        })
    ));
    // Another client, and another account from this client, are not held
    // up.
    assert!(
        fx.auth
            .sign_in("frodo", "password", OTHER_CLIENT)
            .await
            .is_ok()
    );
    fx.register("sam", "password").await;
    assert!(fx.auth.sign_in("sam", "password", CLIENT).await.is_ok());

    // After the delay one attempt is allowed; failing doubles the delay.
    fx.clock.advance(1_000);
    assert!(matches!(
        fx.auth.sign_in("frodo", "wrong", CLIENT).await,
        Err(AuthError::InvalidCredentials)
    ));
    assert!(matches!(
        fx.auth.sign_in("frodo", "password", CLIENT).await,
        Err(AuthError::RateLimited {
            retry_after_secs: 2
        })
    ));
    fx.clock.advance(1_999);
    assert!(matches!(
        fx.auth.sign_in("frodo", "password", CLIENT).await,
        Err(AuthError::RateLimited {
            retry_after_secs: 1
        })
    ));
    // Success clears the count.
    fx.clock.advance(1);
    assert!(fx.auth.sign_in("frodo", "password", CLIENT).await.is_ok());
    for _ in 0..2 {
        assert!(fx.auth.sign_in("frodo", "wrong", CLIENT).await.is_err());
    }
    assert!(fx.auth.sign_in("frodo", "password", CLIENT).await.is_ok());
}

#[tokio::test]
async fn one_client_cannot_try_many_accounts() {
    let fx = Fixture::new();
    // Thirty failures across accounts (ten times the per-account limit).
    for attempt in 0..30 {
        let name = format!("user{attempt}");
        assert!(matches!(
            fx.auth.sign_in(&name, "guess", CLIENT).await,
            Err(AuthError::InvalidCredentials)
        ));
    }
    assert!(matches!(
        fx.auth.sign_in("yet.another", "guess", CLIENT).await,
        Err(AuthError::RateLimited { .. })
    ));
    assert!(matches!(
        fx.auth.sign_in("yet.another", "guess", OTHER_CLIENT).await,
        Err(AuthError::InvalidCredentials)
    ));
}

#[tokio::test]
async fn rate_limiting_can_be_disabled() {
    let fx = Fixture::with(AuthSettings {
        login_backoff: BackoffSettings {
            max_failures: 0,
            base_millis: 1_000,
            max_millis: 60_000,
        },
        ..settings()
    });
    fx.register("frodo", "password").await;
    for _ in 0..40 {
        assert!(matches!(
            fx.auth.sign_in("frodo", "wrong", CLIENT).await,
            Err(AuthError::InvalidCredentials)
        ));
    }
    assert!(fx.auth.sign_in("frodo", "password", CLIENT).await.is_ok());
}

// ---- Sessions (SessionManagerTest) ----

#[tokio::test]
async fn unknown_sessions_are_nobody() {
    let fx = Fixture::new();
    for value in ["", "nonsense", &"x".repeat(43), &"x".repeat(500)] {
        assert!(fx.auth.authenticate_session(value).await.unwrap().is_none());
    }
}

#[tokio::test]
async fn sessions_are_stored_hashed() {
    let fx = Fixture::new();
    fx.register("frodo", "password").await;
    let grant = fx.auth.sign_in("frodo", "password", CLIENT).await.unwrap();
    let Credential::Session(id_hash) = grant.identity.credential else {
        panic!("not a session");
    };
    let record = fx.db.auth_store().session(id_hash).await.unwrap().unwrap();
    assert_eq!(id_hash, sha256(grant.session_id.as_bytes()));
    assert_ne!(record.id_hash.as_slice(), grant.session_id.as_bytes());
    assert_eq!(record.participant, id("frodo@example.com"));
    assert_eq!(record.created_at, START);
    assert_eq!(record.expires_at, START + 60 * 60 * 1000);
    assert_eq!(record.absolute_expires_at, START + 10 * 60 * 60 * 1000);
}

#[tokio::test]
async fn sessions_expire_when_idle_and_renew_when_used() {
    let fx = Fixture::new();
    fx.register("frodo", "password").await;
    let grant = fx.auth.sign_in("frodo", "password", CLIENT).await.unwrap();

    // Used after 50 minutes: renewed for another hour from then.
    fx.clock.advance(50 * 60 * 1000);
    let session = fx
        .auth
        .authenticate_session(&grant.session_id)
        .await
        .unwrap()
        .unwrap();
    assert!(session.renewed);
    assert_eq!(session.max_age_secs, 60 * 60);
    fx.settle().await;
    // 50 more minutes: past the original expiry, alive thanks to renewal.
    fx.clock.advance(50 * 60 * 1000);
    assert!(
        fx.auth
            .authenticate_session(&grant.session_id)
            .await
            .unwrap()
            .is_some()
    );
    fx.settle().await;
    // Idle for the whole lifetime: gone, and deleted.
    fx.clock.advance(60 * 60 * 1000);
    assert!(
        fx.auth
            .authenticate_session(&grant.session_id)
            .await
            .unwrap()
            .is_none()
    );
    fx.settle().await;
    assert_eq!(fx.db.auth_store().session_count().await.unwrap(), 0);
}

#[tokio::test]
async fn sessions_end_at_the_absolute_lifetime_however_busy() {
    let fx = Fixture::new();
    fx.register("frodo", "password").await;
    let grant = fx.auth.sign_in("frodo", "password", CLIENT).await.unwrap();
    // Used every half hour for nine and a half hours.
    for _ in 0..19 {
        fx.clock.advance(30 * 60 * 1000);
        assert!(
            fx.auth
                .authenticate_session(&grant.session_id)
                .await
                .unwrap()
                .is_some()
        );
        fx.settle().await;
    }
    // The renewal does not reach beyond the absolute end.
    fx.clock.advance(15 * 60 * 1000);
    let session = fx
        .auth
        .authenticate_session(&grant.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.max_age_secs, 15 * 60);
    fx.settle().await;
    fx.clock.advance(15 * 60 * 1000);
    assert!(
        fx.auth
            .authenticate_session(&grant.session_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn expired_sessions_are_purged() {
    let fx = Fixture::new();
    fx.register("frodo", "password").await;
    let old = fx.auth.sign_in("frodo", "password", CLIENT).await.unwrap();
    fx.clock.advance(59 * 60 * 1000);
    let fresh = fx.auth.sign_in("frodo", "password", CLIENT).await.unwrap();
    fx.clock.advance(60 * 1000);
    assert_eq!(fx.auth.purge_expired().await.unwrap(), 1);
    assert!(
        fx.auth
            .authenticate_session(&old.session_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        fx.auth
            .authenticate_session(&fresh.session_id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn sign_out_ends_the_session_and_announces_it() {
    let fx = Fixture::new();
    fx.register("frodo", "password").await;
    let grant = fx.auth.sign_in("frodo", "password", CLIENT).await.unwrap();
    let other = fx.auth.sign_in("frodo", "password", CLIENT).await.unwrap();
    let mut revocations = fx.auth.revocations();
    assert!(fx.auth.validate(&grant.identity).await.unwrap());

    assert!(fx.auth.sign_out(&grant.identity).await.unwrap());
    assert!(
        fx.auth
            .authenticate_session(&grant.session_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!fx.auth.validate(&grant.identity).await.unwrap());
    let revocation = revocations.recv().await.unwrap();
    assert!(revocation.applies_to(&grant.identity));
    assert!(!revocation.applies_to(&other.identity));
    // The user's other session lives on.
    assert!(fx.auth.validate(&other.identity).await.unwrap());
    // Signing out twice, or without a session, is harmless.
    assert!(!fx.auth.sign_out(&grant.identity).await.unwrap());
    assert!(
        !fx.auth
            .sign_out(&Identity::internal(id("frodo@example.com")))
            .await
            .unwrap()
    );

    let mut revocations = fx.auth.revocations();
    assert_eq!(
        fx.auth
            .sign_out_everywhere(&id("frodo@example.com"))
            .await
            .unwrap(),
        1
    );
    assert!(!fx.auth.validate(&other.identity).await.unwrap());
    assert!(
        revocations
            .recv()
            .await
            .unwrap()
            .applies_to(&other.identity)
    );
}

#[tokio::test]
async fn removing_an_account_removes_its_credentials() {
    let fx = Fixture::new();
    let frodo = fx.register("frodo", "password").await;
    let grant = fx.auth.sign_in("frodo", "password", CLIENT).await.unwrap();
    let (token, _) = fx
        .auth
        .create_token(&grant.identity, "t", Scopes::ALL, None)
        .await
        .unwrap();
    fx.auth.accounts().remove_account(&frodo).await.unwrap();
    assert!(
        fx.auth
            .authenticate_session(&grant.session_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(fx.auth.authenticate_bearer(&token).await.unwrap().is_none());
}

#[tokio::test]
async fn socket_tokens_authenticate_their_session() {
    // `SessionManager.getSessionFromToken`, with a token that is not the
    // session id itself.
    let fx = Fixture::new();
    fx.register("frodo", "password").await;
    let grant = fx.auth.sign_in("frodo", "password", CLIENT).await.unwrap();
    let token = fx.auth.socket_token(&grant.session_id);
    assert!(token.starts_with(SOCKET_TOKEN_PREFIX));
    assert!(!token.contains(&grant.session_id));
    assert_eq!(token, fx.auth.socket_token(&grant.session_id));
    let identity = fx
        .auth
        .authenticate_socket_token(&token)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(identity, grant.identity);
    // It is no session cookie, and the session id is no socket token.
    assert!(
        fx.auth
            .authenticate_session(&token)
            .await
            .unwrap()
            .is_none()
    );
    for bad in [
        grant.session_id.as_str(),
        "wn_ws_unknown",
        "",
        "wn_pat_nothing",
    ] {
        assert!(
            fx.auth
                .authenticate_socket_token(bad)
                .await
                .unwrap()
                .is_none(),
            "{bad}"
        );
    }
    // It dies with the session.
    fx.auth.sign_out(&grant.identity).await.unwrap();
    assert!(
        fx.auth
            .authenticate_socket_token(&token)
            .await
            .unwrap()
            .is_none()
    );
}

// ---- Access tokens ----

#[tokio::test]
async fn access_tokens_are_created_listed_used_and_revoked() {
    let fx = Fixture::new();
    fx.register("frodo", "password").await;
    fx.register("sam", "password").await;
    let frodo = fx.auth.sign_in("frodo", "password", CLIENT).await.unwrap();
    let sam = fx.auth.sign_in("sam", "password", CLIENT).await.unwrap();

    let (token, info) = fx
        .auth
        .create_token(&frodo.identity, " my agent ", Scopes::READ, Some(3600))
        .await
        .unwrap();
    assert!(token.starts_with(ACCESS_TOKEN_PREFIX));
    assert_eq!(token.len(), ACCESS_TOKEN_PREFIX.len() + 43);
    assert_eq!(info.name, "my agent");
    assert_eq!(info.scopes, Scopes::READ);
    assert_eq!(info.created_at, START);
    assert_eq!(info.expires_at, Some(START + 3_600_000));
    assert_eq!(info.last_used_at, None);
    let (full, full_info) = fx
        .auth
        .create_token(&frodo.identity, "full", Scopes::ALL, None)
        .await
        .unwrap();
    assert_ne!(token, full);

    // Stored hashed: the token is nowhere in the database.
    let record = fx
        .db
        .auth_store()
        .token_by_hash(sha256(token.as_bytes()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.id, info.id);
    assert_eq!(record.scopes, "read");

    let listed = fx.auth.list_tokens(&id("frodo@example.com")).await.unwrap();
    assert_eq!(listed, [info.clone(), full_info.clone()]);
    assert!(
        fx.auth
            .list_tokens(&id("sam@example.com"))
            .await
            .unwrap()
            .is_empty()
    );

    // Using it.
    fx.clock.advance(5_000);
    let identity = fx.auth.authenticate_bearer(&token).await.unwrap().unwrap();
    assert_eq!(identity.participant, id("frodo@example.com"));
    assert_eq!(identity.scopes, Scopes::READ);
    assert!(identity.can_read() && !identity.can_write());
    assert_eq!(identity.credential, Credential::AccessToken(info.id));
    assert!(fx.auth.validate(&identity).await.unwrap());
    fx.settle().await;
    let listed = fx.auth.list_tokens(&id("frodo@example.com")).await.unwrap();
    assert_eq!(listed[0].last_used_at, Some(START + 5_000));
    let full_identity = fx.auth.authenticate_bearer(&full).await.unwrap().unwrap();
    assert!(full_identity.can_write());
    // Also as the token of a `ProtocolAuthenticate`.
    assert_eq!(
        fx.auth.authenticate_socket_token(&token).await.unwrap(),
        Some(identity.clone())
    );

    // Not a token.
    for bad in [
        "",
        "wn_pat_",
        "wn_pat_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        &token[..token.len() - 1],
        &token.to_uppercase(),
        &"x".repeat(1000),
    ] {
        assert!(
            fx.auth.authenticate_bearer(bad).await.unwrap().is_none(),
            "{bad}"
        );
    }

    // Only the owner revokes, and revocation is announced.
    let mut revocations = fx.auth.revocations();
    assert!(
        !fx.auth
            .revoke_token(&id("sam@example.com"), info.id)
            .await
            .unwrap()
    );
    assert!(fx.auth.authenticate_bearer(&token).await.unwrap().is_some());
    assert!(
        fx.auth
            .revoke_token(&id("frodo@example.com"), info.id)
            .await
            .unwrap()
    );
    assert!(fx.auth.authenticate_bearer(&token).await.unwrap().is_none());
    assert!(!fx.auth.validate(&identity).await.unwrap());
    let revocation = revocations.recv().await.unwrap();
    assert!(revocation.applies_to(&identity));
    assert!(!revocation.applies_to(&full_identity));
    assert!(!revocation.applies_to(&sam.identity));
    assert!(
        !fx.auth
            .revoke_token(&id("frodo@example.com"), info.id)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn access_tokens_expire() {
    let fx = Fixture::new();
    fx.register("frodo", "password").await;
    let frodo = fx.auth.sign_in("frodo", "password", CLIENT).await.unwrap();
    let (token, _) = fx
        .auth
        .create_token(&frodo.identity, "short", Scopes::ALL, Some(60))
        .await
        .unwrap();
    fx.clock.advance(59_999);
    let identity = fx.auth.authenticate_bearer(&token).await.unwrap().unwrap();
    fx.clock.advance(1);
    assert!(fx.auth.authenticate_bearer(&token).await.unwrap().is_none());
    assert!(!fx.auth.validate(&identity).await.unwrap());
}

#[tokio::test]
async fn token_requests_are_validated() {
    let fx = Fixture::new();
    fx.register("frodo", "password").await;
    let frodo = fx.auth.sign_in("frodo", "password", CLIENT).await.unwrap();
    let invalid = |result: Result<(String, AccessTokenInfo), AuthError>| {
        matches!(result, Err(AuthError::Invalid(_)))
    };
    let auth = &fx.auth;
    assert!(invalid(
        auth.create_token(&frodo.identity, "  ", Scopes::ALL, None)
            .await
    ));
    assert!(invalid(
        auth.create_token(&frodo.identity, &"n".repeat(101), Scopes::ALL, None)
            .await
    ));
    assert!(invalid(
        auth.create_token(&frodo.identity, "t", Scopes::NONE, None)
            .await
    ));
    assert!(invalid(
        auth.create_token(&frodo.identity, "t", Scopes::ALL, Some(0))
            .await
    ));
    // A token cannot mint tokens.
    let (token, _) = auth
        .create_token(&frodo.identity, "t", Scopes::ALL, None)
        .await
        .unwrap();
    let by_token = auth.authenticate_bearer(&token).await.unwrap().unwrap();
    assert!(matches!(
        auth.create_token(&by_token, "t2", Scopes::ALL, None).await,
        Err(AuthError::Forbidden(_))
    ));
    // There is a cap per account.
    for n in 1..MAX_TOKENS_PER_PARTICIPANT {
        auth.create_token(&frodo.identity, &format!("t{n}"), Scopes::READ, None)
            .await
            .unwrap();
    }
    assert!(invalid(
        auth.create_token(&frodo.identity, "one too many", Scopes::READ, None)
            .await
    ));
}

#[tokio::test]
async fn robots_authenticate_with_their_account_token() {
    let fx = Fixture::new();
    let robot = id("helper@example.com");
    fx.auth
        .accounts()
        .put_account(RobotAccountData::agent(robot.clone()).into())
        .await
        .unwrap();
    let token = fx.auth.issue_robot_token(&robot).await.unwrap();
    assert!(token.starts_with(ROBOT_TOKEN_PREFIX));
    let identity = fx.auth.authenticate_bearer(&token).await.unwrap().unwrap();
    assert_eq!(identity.participant, robot);
    assert_eq!(identity.scopes, Scopes::ALL);
    assert!(!identity.is_session());
    assert!(fx.auth.validate(&identity).await.unwrap());
    // Only the hash is in the account.
    let account = fx
        .auth
        .accounts()
        .get_account(&robot)
        .await
        .unwrap()
        .unwrap();
    let stored = account
        .as_robot()
        .unwrap()
        .access_token_hash
        .clone()
        .unwrap();
    assert_eq!(stored, hex(&sha256(token.as_bytes())));

    // A new token replaces the old one.
    let mut revocations = fx.auth.revocations();
    let second = fx.auth.issue_robot_token(&robot).await.unwrap();
    assert!(fx.auth.authenticate_bearer(&token).await.unwrap().is_none());
    assert!(!fx.auth.validate(&identity).await.unwrap());
    assert!(revocations.recv().await.unwrap().applies_to(&identity));
    assert!(
        fx.auth
            .authenticate_bearer(&second)
            .await
            .unwrap()
            .is_some()
    );

    // Humans have no robot token.
    fx.register("frodo", "password").await;
    assert!(matches!(
        fx.auth.issue_robot_token(&id("frodo@example.com")).await,
        Err(AuthError::Invalid(_))
    ));
    assert!(matches!(
        fx.auth.issue_robot_token(&id("nobody@example.com")).await,
        Err(AuthError::Invalid(_))
    ));
    // A robot's stored hash is not usable as a personal access token.
    assert!(
        fx.auth
            .authenticate_bearer(&format!("{ACCESS_TOKEN_PREFIX}{stored}"))
            .await
            .unwrap()
            .is_none()
    );
}

#[test]
fn scopes_parse_and_print() {
    assert_eq!(Scopes::parse("read write").unwrap(), Scopes::ALL);
    assert_eq!(Scopes::parse("write,read").unwrap(), Scopes::ALL);
    assert_eq!(Scopes::parse("read").unwrap(), Scopes::READ);
    assert_eq!(Scopes::parse("").unwrap(), Scopes::NONE);
    assert_eq!(Scopes::parse("admin").unwrap_err(), "unknown scope 'admin'");
    assert_eq!(Scopes::ALL.to_string(), "read write");
    assert_eq!(Scopes::WRITE.names(), ["write"]);
    assert!(Scopes::ALL.can_read() && Scopes::ALL.can_write());
    assert!(!Scopes::READ.can_write() && !Scopes::WRITE.can_read());
    assert!(!Scopes::NONE.can_read());
}

#[test]
fn internal_identities_are_not_revoked() {
    let agent = Identity::internal(id("agent@example.com"));
    assert!(agent.can_read() && agent.can_write() && !agent.is_session());
    assert!(!Revocation::Participant(id("agent@example.com")).applies_to(&agent));
    assert!(!Revocation::AccessToken(1).applies_to(&agent));
}
