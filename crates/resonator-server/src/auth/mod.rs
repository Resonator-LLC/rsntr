//! Authentication: accounts, browser sessions and bearer tokens.
//!
//! Everything an authenticated caller does resolves to an [`Identity`]: a
//! participant with scopes. A human gets one by signing in (a session,
//! carried by a cookie) or by presenting a personal access token; a robot
//! or agent by presenting the token whose hash its account holds;
//! in-process agents use [`Identity::internal`]. The HTTP layer, the
//! WebSocket protocol and later the robot and agent APIs all work with
//! identities and never look at credentials themselves.
//!
//! Registration and sign-in follow `UserRegistrationServlet`,
//! `RegistrationUtil`, `AuthenticationServlet` and
//! `AccountStoreLoginModule` of `org.waveprotocol.box.server` in Apache
//! Wave (Apache License 2.0). Sessions replace Jetty's (`SessionManager`),
//! and personal access tokens replace the OAuth 1.0a of the original's
//! Data API.

pub mod rate_limit;
pub mod registration;

use std::fmt;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use resonator_model::id::{InvalidParticipantAddress, ParticipantId};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{Semaphore, broadcast};

use crate::account::{AccountData, HumanAccountData, PasswordDigest};
use crate::clock::{Clock, SystemClock};
use crate::config::Config;
use crate::storage::{
    AccessTokenRecord, AccountStore, AuthStore, Database, SessionRecord, StorageError,
};
use rate_limit::{BackoffSettings, FailureLimiter, WindowLimiter};

/// Name of the session cookie.
pub const SESSION_COOKIE: &str = "WSESSIONID";
/// Prefix of personal access tokens.
pub const ACCESS_TOKEN_PREFIX: &str = "wn_pat_";
/// Prefix of robot and agent tokens.
pub const ROBOT_TOKEN_PREFIX: &str = "wn_bot_";
/// Prefix of the tokens that authenticate a WebSocket for a session.
pub const SOCKET_TOKEN_PREFIX: &str = "wn_ws_";

/// The message of a failed sign-in (`AuthenticationServlet`).
pub const INCORRECT_CREDENTIALS: &str = "The username or password you entered is incorrect.";

/// Longest password accepted, in bytes; hashing longer ones would be a
/// cheap way to keep the server busy.
pub const MAX_PASSWORD_BYTES: usize = 1024;
/// Access tokens one participant may hold.
pub const MAX_TOKENS_PER_PARTICIPANT: usize = 100;
/// A session's or token's use is recorded at most this often.
const TOUCH_INTERVAL_MILLIS: i64 = 60_000;

/// What a credential allows.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Scopes(u8);

impl Scopes {
    pub const NONE: Scopes = Scopes(0);
    /// Open wave views, fetch snapshots and profiles.
    pub const READ: Scopes = Scopes(1);
    /// Submit deltas and change the account's settings.
    pub const WRITE: Scopes = Scopes(2);
    pub const ALL: Scopes = Scopes(3);

    const NAMES: [(Scopes, &'static str); 2] = [(Scopes::READ, "read"), (Scopes::WRITE, "write")];

    pub fn contains(self, other: Scopes) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn union(self, other: Scopes) -> Scopes {
        Scopes(self.0 | other.0)
    }

    pub fn can_read(self) -> bool {
        self.contains(Scopes::READ)
    }

    pub fn can_write(self) -> bool {
        self.contains(Scopes::WRITE)
    }

    pub fn names(self) -> Vec<&'static str> {
        Self::NAMES
            .iter()
            .filter(|(scope, _)| self.contains(*scope))
            .map(|(_, name)| *name)
            .collect()
    }

    /// Parses scope names separated by spaces or commas.
    pub fn parse(text: &str) -> Result<Scopes, String> {
        Self::from_names(text.split([' ', ',']).filter(|name| !name.is_empty()))
    }

    pub fn from_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<Scopes, String> {
        let mut scopes = Scopes::NONE;
        for name in names {
            let (scope, _) = Self::NAMES
                .iter()
                .find(|(_, known)| *known == name)
                .ok_or_else(|| format!("unknown scope '{name}'"))?;
            scopes = scopes.union(*scope);
        }
        Ok(scopes)
    }
}

impl fmt::Display for Scopes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.names().join(" "))
    }
}

impl fmt::Debug for Scopes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Scopes({self})")
    }
}

/// How an identity was established.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Credential {
    /// A browser session, by the hash of its id.
    Session([u8; 32]),
    /// A personal access token, by its id.
    AccessToken(i64),
    /// A robot's or agent's token, by its hash as stored in the account.
    RobotToken(String),
    /// Code running inside the server.
    Internal,
}

/// An authenticated caller: a participant and what it may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub participant: ParticipantId,
    pub scopes: Scopes,
    pub credential: Credential,
}

impl Identity {
    /// The identity of code running inside the server on behalf of
    /// `participant`, such as a built-in agent.
    pub fn internal(participant: ParticipantId) -> Self {
        Identity {
            participant,
            scopes: Scopes::ALL,
            credential: Credential::Internal,
        }
    }

    pub fn can_read(&self) -> bool {
        self.scopes.can_read()
    }

    pub fn can_write(&self) -> bool {
        self.scopes.can_write()
    }

    /// Whether this is a signed-in browser session, which may manage the
    /// account's tokens.
    pub fn is_session(&self) -> bool {
        matches!(self.credential, Credential::Session(_))
    }
}

/// A credential that stopped being valid. Long-lived connections listen
/// for these ([`AuthService::revocations`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Revocation {
    Session([u8; 32]),
    AccessToken(i64),
    /// Every credential of a participant.
    Participant(ParticipantId),
}

impl Revocation {
    pub fn applies_to(&self, identity: &Identity) -> bool {
        match (self, &identity.credential) {
            (Revocation::Session(revoked), Credential::Session(held)) => revoked == held,
            (Revocation::AccessToken(revoked), Credential::AccessToken(held)) => revoked == held,
            (Revocation::Participant(participant), credential) => {
                *participant == identity.participant && *credential != Credential::Internal
            }
            _ => false,
        }
    }
}

/// Why an authentication request failed. The display string is the message
/// for the client.
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("{INCORRECT_CREDENTIALS}")]
    InvalidCredentials,
    #[error("Too many attempts. Try again in {retry_after_secs} seconds.")]
    RateLimited { retry_after_secs: u64 },
    #[error("Registration disabled by administrator")]
    RegistrationDisabled,
    #[error("{0}")]
    InvalidAddress(#[from] InvalidParticipantAddress),
    #[error("Account already exists")]
    AccountExists,
    #[error("Password must be at least {min} characters long")]
    PasswordTooShort { min: usize },
    #[error("Password is too long")]
    PasswordTooLong,
    /// The request is not allowed for this identity.
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    Invalid(String),
    #[error("An unexpected error occurred while trying to retrieve account information!")]
    Storage(#[from] StorageError),
    #[error("An unexpected error occurred while trying to create the account")]
    Internal(String),
}

/// Settings of an [`AuthService`].
#[derive(Debug, Clone)]
pub struct AuthSettings {
    pub domain: String,
    pub registration_enabled: bool,
    /// May register accounts even when registration is disabled.
    pub admin: Option<ParticipantId>,
    pub session_lifetime_millis: i64,
    /// 0 for no absolute limit.
    pub session_absolute_lifetime_millis: i64,
    pub password_min_length: usize,
    pub login_backoff: BackoffSettings,
    pub register_max_per_hour: u32,
    /// Argon2 parameters for new password hashes.
    pub password_params: argon2::Params,
}

impl AuthSettings {
    pub fn new(domain: impl Into<String>) -> Self {
        let mut settings = Self::from_config(&Config::default());
        settings.domain = domain.into();
        settings
    }

    pub fn from_config(config: &Config) -> Self {
        let millis = |secs: u64| i64::try_from(secs.saturating_mul(1000)).unwrap_or(i64::MAX);
        AuthSettings {
            domain: config.domain.clone(),
            registration_enabled: config.registration_open(),
            admin: config.admin(),
            session_lifetime_millis: millis(config.session_lifetime_secs),
            session_absolute_lifetime_millis: millis(config.session_absolute_lifetime_secs),
            password_min_length: config.password_min_length,
            login_backoff: BackoffSettings {
                max_failures: config.login_max_failures,
                base_millis: millis(config.login_backoff_base_secs),
                max_millis: millis(config.login_backoff_max_secs),
            },
            register_max_per_hour: config.register_max_per_hour,
            password_params: argon2::Params::default(),
        }
    }
}

/// A new session: the identity and the value for the session cookie.
#[derive(Debug, Clone)]
pub struct SessionGrant {
    pub identity: Identity,
    /// The session id, to be set as the [`SESSION_COOKIE`] cookie. It is
    /// not stored and cannot be recovered.
    pub session_id: String,
    /// How long the cookie should live.
    pub max_age_secs: u64,
}

/// The result of authenticating a session cookie.
#[derive(Debug, Clone)]
pub struct SessionAuth {
    pub identity: Identity,
    /// Whether the session's expiry was pushed forward, so the cookie
    /// should be set again.
    pub renewed: bool,
    pub max_age_secs: u64,
}

/// A personal access token as shown to its owner (never the token itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessTokenInfo {
    pub id: i64,
    pub name: String,
    pub scopes: Scopes,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub last_used_at: Option<i64>,
}

impl AccessTokenInfo {
    fn of(record: &AccessTokenRecord) -> Self {
        AccessTokenInfo {
            id: record.id,
            name: record.name.clone(),
            // Unknown names (from a newer build) grant nothing.
            scopes: record
                .scopes
                .split(' ')
                .filter_map(|name| Scopes::from_names([name]).ok())
                .fold(Scopes::NONE, Scopes::union),
            created_at: record.created_at,
            expires_at: record.expires_at,
            last_used_at: record.last_used_at,
        }
    }
}

struct Limiters {
    /// Per account and client address.
    sign_in: FailureLimiter,
    /// Per client address across accounts, with ten times the allowance.
    sign_in_client: FailureLimiter,
    register: WindowLimiter,
}

/// Called with the address of a newly registered account.
type RegistrationHook = Box<dyn Fn(&ParticipantId) + Send + Sync>;

struct Inner {
    settings: AuthSettings,
    accounts: AccountStore,
    store: AuthStore,
    clock: Arc<dyn Clock>,
    limiters: Mutex<Limiters>,
    /// Verified against when the account does not exist, so that sign-in
    /// takes as long for unknown names as for known ones.
    dummy_digest: OnceLock<PasswordDigest>,
    /// Bounds concurrent password hashing (memory and CPU).
    hashing: Semaphore,
    revocations: broadcast::Sender<Revocation>,
    /// Called with every newly registered account.
    registered: OnceLock<RegistrationHook>,
}

/// The authentication service; cheap to clone.
#[derive(Clone)]
pub struct AuthService {
    inner: Arc<Inner>,
}

impl fmt::Debug for AuthService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthService")
            .field("domain", &self.inner.settings.domain)
            .finish()
    }
}

fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A token of `prefix` followed by 256 random bits.
fn random_token(prefix: &str) -> Result<String, AuthError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| AuthError::Internal(e.to_string()))?;
    Ok(format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes)))
}

/// The WebSocket token of the session with this id: derived from the id,
/// so that it is the same every time the client asks for it, yet does not
/// reveal the id.
fn socket_token_of(session_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"resonator socket token\0");
    hasher.update(session_id.as_bytes());
    format!(
        "{SOCKET_TOKEN_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(hasher.finalize())
    )
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl AuthService {
    pub fn new(db: &Database, settings: AuthSettings) -> Self {
        Self::with_clock(db, settings, Arc::new(SystemClock))
    }

    pub fn with_clock(db: &Database, settings: AuthSettings, clock: Arc<dyn Clock>) -> Self {
        let (revocations, _) = broadcast::channel(256);
        let hashing = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .clamp(1, 8);
        AuthService {
            inner: Arc::new(Inner {
                limiters: Mutex::new(Limiters {
                    sign_in: FailureLimiter::new(settings.login_backoff),
                    sign_in_client: FailureLimiter::new(BackoffSettings {
                        max_failures: settings.login_backoff.max_failures.saturating_mul(10),
                        ..settings.login_backoff
                    }),
                    register: WindowLimiter::new(settings.register_max_per_hour, 60 * 60 * 1000),
                }),
                settings,
                accounts: db.account_store(),
                store: db.auth_store(),
                clock,
                dummy_digest: OnceLock::new(),
                hashing: Semaphore::new(hashing),
                revocations,
                registered: OnceLock::new(),
            }),
        }
    }

    pub fn settings(&self) -> &AuthSettings {
        &self.inner.settings
    }

    pub fn accounts(&self) -> &AccountStore {
        &self.inner.accounts
    }

    fn now(&self) -> i64 {
        self.inner.clock.now_millis()
    }

    /// Sets what is called with the address of every newly registered
    /// account, once, by the first caller; later calls change nothing. The
    /// function runs within [`AuthService::register`] and must return
    /// quickly: hand real work to a task.
    pub fn on_registration(&self, registered: impl Fn(&ParticipantId) + Send + Sync + 'static) {
        let _ = self.inner.registered.set(Box::new(registered));
    }

    /// The clock the service tells time by.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.inner.clock
    }

    /// Credentials that stop being valid are announced here.
    pub fn revocations(&self) -> broadcast::Receiver<Revocation> {
        self.inner.revocations.subscribe()
    }

    fn revoke(&self, revocation: Revocation) {
        // No receivers is fine.
        let _ = self.inner.revocations.send(revocation);
    }

    /// Runs a password hashing job off the async threads, a bounded number
    /// at a time.
    async fn hash_job<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, AuthError> {
        let _permit = self
            .inner
            .hashing
            .acquire()
            .await
            .map_err(|e| AuthError::Internal(e.to_string()))?;
        tokio::task::spawn_blocking(job)
            .await
            .map_err(|e| AuthError::Internal(e.to_string()))
    }

    // ---- Registration ----

    /// Creates a human account (`UserRegistrationServlet.tryCreateUser`).
    /// `actor` is the caller if authenticated: the admin may register
    /// accounts when registration is disabled.
    pub async fn register(
        &self,
        username: Option<&str>,
        password: &str,
        client: IpAddr,
        actor: Option<&Identity>,
    ) -> Result<ParticipantId, AuthError> {
        let settings = &self.inner.settings;
        let by_admin = matches!(
            (actor, &settings.admin),
            (Some(actor), Some(admin)) if actor.participant == *admin && actor.can_write()
        );
        if !settings.registration_enabled && !by_admin {
            return Err(AuthError::RegistrationDisabled);
        }
        let id = registration::check_new_username(&settings.domain, username)?;
        if password.chars().count() < settings.password_min_length {
            return Err(AuthError::PasswordTooShort {
                min: settings.password_min_length,
            });
        }
        if password.len() > MAX_PASSWORD_BYTES {
            return Err(AuthError::PasswordTooLong);
        }
        if !by_admin {
            let limited = lock(&self.inner.limiters)
                .register
                .acquire(&client.to_string(), self.now());
            if let Err(wait) = limited {
                return Err(AuthError::RateLimited {
                    retry_after_secs: (wait as u64).div_ceil(1000).max(1),
                });
            }
        }
        if self.inner.accounts.get_account(&id).await?.is_some() {
            return Err(AuthError::AccountExists);
        }
        let (password, params) = (password.to_owned(), settings.password_params.clone());
        let digest = self
            .hash_job(move || PasswordDigest::with_params(&password, params))
            .await?
            .map_err(|e| AuthError::Internal(e.to_string()))?;
        let account = AccountData::Human(HumanAccountData::with_password(id.clone(), digest));
        // Two registrations may race for the address.
        if !self.inner.accounts.create_account(account).await? {
            return Err(AuthError::AccountExists);
        }
        tracing::info!(participant = %id, "registered new account");
        // The original has its welcome robot greet the new user here
        // (`WelcomeRobot.greet`).
        if let Some(registered) = self.inner.registered.get() {
            registered(&id);
        }
        Ok(id)
    }

    // ---- Sign-in and sessions ----

    fn dummy_digest(&self) -> PasswordDigest {
        self.inner
            .dummy_digest
            .get_or_init(|| {
                PasswordDigest::with_params(
                    "resonator dummy password",
                    self.inner.settings.password_params.clone(),
                )
                .expect("hashing a constant password")
            })
            .clone()
    }

    /// Checks a name and password (`AccountStoreLoginModule.login`) and
    /// starts a session. Failures are rate limited per account and client
    /// address and per client address.
    pub async fn sign_in(
        &self,
        name: &str,
        password: &str,
        client: IpAddr,
    ) -> Result<SessionGrant, AuthError> {
        let settings = &self.inner.settings;
        let id = registration::sign_in_address(&settings.domain, name);
        // Unparseable names share one rate-limit bucket per client.
        let account_key = format!(
            "a|{}|{client}",
            id.as_ref().map_or("?", ParticipantId::address)
        );
        let client_key = format!("c|{client}");
        {
            let limiters = lock(&self.inner.limiters);
            let now = self.now();
            let wait = limiters
                .sign_in
                .check(&account_key, now)
                .and(limiters.sign_in_client.check(&client_key, now));
            if let Err(wait) = wait {
                return Err(AuthError::RateLimited {
                    retry_after_secs: (wait as u64).div_ceil(1000).max(1),
                });
            }
        }
        let account = match &id {
            Some(id) => self.inner.accounts.get_account(id).await?,
            None => None,
        };
        let digest = account
            .as_ref()
            .and_then(AccountData::as_human)
            .and_then(|human| human.password_digest.clone());
        let known = digest.is_some();
        // Always verify something, so that timing does not tell whether
        // the account exists.
        let digest = match digest {
            Some(digest) => digest,
            None => {
                let this = self.clone();
                self.hash_job(move || this.dummy_digest()).await?
            }
        };
        let candidate = if password.len() > MAX_PASSWORD_BYTES {
            String::new()
        } else {
            password.to_owned()
        };
        let verified = self.hash_job(move || digest.verify(&candidate)).await?;
        let success = known && verified && password.len() <= MAX_PASSWORD_BYTES;
        let now = self.now();
        let Some(id) = id.filter(|_| success) else {
            let mut limiters = lock(&self.inner.limiters);
            limiters.sign_in.record_failure(&account_key, now);
            limiters.sign_in_client.record_failure(&client_key, now);
            return Err(AuthError::InvalidCredentials);
        };
        lock(&self.inner.limiters)
            .sign_in
            .record_success(&account_key);
        let grant = self.create_session(&id).await?;
        tracing::info!(participant = %id, "Authenticated user");
        Ok(grant)
    }

    /// Starts a session for `participant` without checking a password, for
    /// callers that authenticated the user some other way.
    pub async fn create_session(
        &self,
        participant: &ParticipantId,
    ) -> Result<SessionGrant, AuthError> {
        let settings = &self.inner.settings;
        let session_id = random_token("")?;
        let now = self.now();
        let absolute_expires_at = match settings.session_absolute_lifetime_millis {
            0 => i64::MAX,
            lifetime => now.saturating_add(lifetime),
        };
        let expires_at = now
            .saturating_add(settings.session_lifetime_millis)
            .min(absolute_expires_at);
        let id_hash = sha256(session_id.as_bytes());
        self.inner
            .store
            .create_session(SessionRecord {
                id_hash,
                participant: participant.clone(),
                socket_token_hash: sha256(socket_token_of(&session_id).as_bytes()),
                created_at: now,
                last_seen_at: now,
                expires_at,
                absolute_expires_at,
            })
            .await?;
        Ok(SessionGrant {
            identity: Identity {
                participant: participant.clone(),
                scopes: Scopes::ALL,
                credential: Credential::Session(id_hash),
            },
            session_id,
            max_age_secs: ((expires_at - now).max(0) as u64) / 1000,
        })
    }

    /// The identity of a live session record; renews the session if it was
    /// not used for a while.
    async fn session_auth(&self, record: SessionRecord) -> Option<SessionAuth> {
        let now = self.now();
        if now >= record.expires_at || now >= record.absolute_expires_at {
            let store = self.inner.store.clone();
            tokio::spawn(async move {
                let _ = store.delete_session(record.id_hash).await;
            });
            return None;
        }
        let mut expires_at = record.expires_at;
        let renewed = now - record.last_seen_at >= TOUCH_INTERVAL_MILLIS;
        if renewed {
            expires_at = now
                .saturating_add(self.inner.settings.session_lifetime_millis)
                .min(record.absolute_expires_at);
            let store = self.inner.store.clone();
            tokio::spawn(async move {
                if let Err(error) = store.touch_session(record.id_hash, now, expires_at).await {
                    tracing::warn!(%error, "failed to renew session");
                }
            });
        }
        Some(SessionAuth {
            identity: Identity {
                participant: record.participant,
                scopes: Scopes::ALL,
                credential: Credential::Session(record.id_hash),
            },
            renewed,
            max_age_secs: ((expires_at - now).max(0) as u64) / 1000,
        })
    }

    /// Authenticates the value of the session cookie.
    pub async fn authenticate_session(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionAuth>, StorageError> {
        if session_id.is_empty() || session_id.len() > 128 {
            return Ok(None);
        }
        match self
            .inner
            .store
            .session(sha256(session_id.as_bytes()))
            .await?
        {
            Some(record) => Ok(self.session_auth(record).await),
            None => Ok(None),
        }
    }

    /// The token that authenticates a WebSocket (`ProtocolAuthenticate`)
    /// for the session with this id. Valid as long as the session is.
    pub fn socket_token(&self, session_id: &str) -> String {
        socket_token_of(session_id)
    }

    /// Ends a session. Returns whether it existed.
    pub async fn sign_out(&self, identity: &Identity) -> Result<bool, StorageError> {
        let Credential::Session(id_hash) = identity.credential else {
            return Ok(false);
        };
        let existed = self.inner.store.delete_session(id_hash).await?;
        self.revoke(Revocation::Session(id_hash));
        Ok(existed)
    }

    /// Ends every session of a participant and closes all its connections.
    pub async fn sign_out_everywhere(
        &self,
        participant: &ParticipantId,
    ) -> Result<usize, StorageError> {
        let count = self.inner.store.delete_sessions_of(participant).await?;
        self.revoke(Revocation::Participant(participant.clone()));
        Ok(count)
    }

    // ---- Bearer tokens ----

    /// Authenticates a bearer token: a personal access token or a robot's
    /// token.
    pub async fn authenticate_bearer(&self, token: &str) -> Result<Option<Identity>, StorageError> {
        if token.len() > 256 {
            return Ok(None);
        }
        let now = self.now();
        if token.starts_with(ACCESS_TOKEN_PREFIX) {
            let Some(record) = self
                .inner
                .store
                .token_by_hash(sha256(token.as_bytes()))
                .await?
            else {
                return Ok(None);
            };
            if record.expires_at.is_some_and(|expires| now >= expires) {
                return Ok(None);
            }
            if record
                .last_used_at
                .is_none_or(|used| now - used >= TOUCH_INTERVAL_MILLIS)
            {
                let store = self.inner.store.clone();
                let id = record.id;
                tokio::spawn(async move {
                    let _ = store.touch_token(id, now).await;
                });
            }
            let info = AccessTokenInfo::of(&record);
            return Ok(Some(Identity {
                participant: record.participant,
                scopes: info.scopes,
                credential: Credential::AccessToken(record.id),
            }));
        }
        if token.starts_with(ROBOT_TOKEN_PREFIX) {
            let hash = hex(&sha256(token.as_bytes()));
            return Ok(self
                .inner
                .store
                .robot_by_token_hash(hash.clone())
                .await?
                .map(|participant| Identity {
                    participant,
                    scopes: Scopes::ALL,
                    credential: Credential::RobotToken(hash),
                }));
        }
        Ok(None)
    }

    /// Authenticates the token of a `ProtocolAuthenticate` message: a
    /// session's WebSocket token or a bearer token.
    pub async fn authenticate_socket_token(
        &self,
        token: &str,
    ) -> Result<Option<Identity>, StorageError> {
        if token.starts_with(SOCKET_TOKEN_PREFIX) && token.len() <= 128 {
            return Ok(
                match self
                    .inner
                    .store
                    .session_by_socket_token(sha256(token.as_bytes()))
                    .await?
                {
                    Some(record) => self.session_auth(record).await.map(|auth| auth.identity),
                    None => None,
                },
            );
        }
        self.authenticate_bearer(token).await
    }

    /// Whether an identity's credential is still valid. Long-lived
    /// connections call this from time to time; for a session it counts as
    /// a use.
    pub async fn validate(&self, identity: &Identity) -> Result<bool, StorageError> {
        Ok(match &identity.credential {
            Credential::Internal => true,
            Credential::Session(id_hash) => match self.inner.store.session(*id_hash).await? {
                Some(record) => self.session_auth(record).await.is_some(),
                None => false,
            },
            Credential::AccessToken(id) => self
                .inner
                .store
                .token(*id)
                .await?
                .is_some_and(|record| record.expires_at.is_none_or(|e| self.now() < e)),
            Credential::RobotToken(hash) => {
                self.inner.store.robot_by_token_hash(hash.clone()).await?
                    == Some(identity.participant.clone())
            }
        })
    }

    /// Creates a personal access token for the signed-in owner. The token
    /// itself is returned once and never stored.
    pub async fn create_token(
        &self,
        owner: &Identity,
        name: &str,
        scopes: Scopes,
        expires_in_secs: Option<u64>,
    ) -> Result<(String, AccessTokenInfo), AuthError> {
        if !owner.is_session() {
            return Err(AuthError::Forbidden(
                "Access tokens can only be managed from a signed-in session".to_owned(),
            ));
        }
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 100 {
            return Err(AuthError::Invalid(
                "Token name must be between 1 and 100 characters".to_owned(),
            ));
        }
        if scopes == Scopes::NONE {
            return Err(AuthError::Invalid(
                "A token needs at least one scope".to_owned(),
            ));
        }
        let existing = self.inner.store.tokens_of(&owner.participant).await?;
        if existing.len() >= MAX_TOKENS_PER_PARTICIPANT {
            return Err(AuthError::Invalid(format!(
                "At most {MAX_TOKENS_PER_PARTICIPANT} access tokens per account"
            )));
        }
        let now = self.now();
        let expires_at = match expires_in_secs {
            None => None,
            Some(0) => {
                return Err(AuthError::Invalid(
                    "expires_in_secs must be positive".to_owned(),
                ));
            }
            Some(secs) => Some(
                now.saturating_add(i64::try_from(secs.saturating_mul(1000)).unwrap_or(i64::MAX)),
            ),
        };
        let token = random_token(ACCESS_TOKEN_PREFIX)?;
        let record = self
            .inner
            .store
            .create_token(
                sha256(token.as_bytes()),
                &owner.participant,
                name,
                &scopes.to_string(),
                now,
                expires_at,
            )
            .await?;
        tracing::info!(participant = %owner.participant, token = record.id, %scopes, "created access token");
        Ok((token, AccessTokenInfo::of(&record)))
    }

    /// A participant's access tokens, oldest first.
    pub async fn list_tokens(
        &self,
        participant: &ParticipantId,
    ) -> Result<Vec<AccessTokenInfo>, StorageError> {
        Ok(self
            .inner
            .store
            .tokens_of(participant)
            .await?
            .iter()
            .map(AccessTokenInfo::of)
            .collect())
    }

    /// Revokes one of a participant's access tokens; returns whether it
    /// existed.
    pub async fn revoke_token(
        &self,
        participant: &ParticipantId,
        id: i64,
    ) -> Result<bool, StorageError> {
        let existed = self.inner.store.delete_token(participant, id).await?;
        if existed {
            self.revoke(Revocation::AccessToken(id));
            tracing::info!(%participant, token = id, "revoked access token");
        }
        Ok(existed)
    }

    /// Gives a robot or agent account a new token, replacing any earlier
    /// one, and returns it. The account stores only its hash.
    pub async fn issue_robot_token(&self, robot: &ParticipantId) -> Result<String, AuthError> {
        let Some(AccountData::Robot(mut account)) = self.inner.accounts.get_account(robot).await?
        else {
            return Err(AuthError::Invalid(format!(
                "{robot} is not a robot or agent account"
            )));
        };
        let token = random_token(ROBOT_TOKEN_PREFIX)?;
        account.access_token_hash = Some(hex(&sha256(token.as_bytes())));
        self.inner
            .accounts
            .put_account(AccountData::Robot(account))
            .await?;
        // Connections authenticated with the old token must go.
        self.revoke(Revocation::Participant(robot.clone()));
        Ok(token)
    }

    // ---- Housekeeping ----

    /// Deletes expired sessions; returns how many.
    pub async fn purge_expired(&self) -> Result<usize, StorageError> {
        self.inner.store.delete_expired_sessions(self.now()).await
    }

    /// Starts a task that purges expired sessions every `interval`. It
    /// ends when the last handle to the service is dropped.
    pub fn spawn_maintenance(&self, interval: std::time::Duration) -> tokio::task::JoinHandle<()> {
        let inner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(inner) = inner.upgrade() else {
                    break;
                };
                let service = AuthService { inner };
                match service.purge_expired().await {
                    Ok(0) => {}
                    Ok(purged) => tracing::debug!(purged, "purged expired sessions"),
                    Err(StorageError::Closed) => break,
                    Err(error) => tracing::warn!(%error, "purging expired sessions failed"),
                }
            }
        })
    }
}

#[cfg(test)]
mod tests;
