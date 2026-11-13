//! Accounts of humans, robots and built-in agents.
//!
//! Derived from `org.waveprotocol.box.server.account`,
//! `org.waveprotocol.box.server.robots.RobotCapabilities`,
//! `com.google.wave.api.robot.Capability` and the `EventType`, `Context`
//! and `ProtocolVersion` enums of `com.google.wave.api` of Apache Wave,
//! licensed under the Apache License, Version 2.0.
//!
//! Differences from the original: passwords are hashed with argon2id and
//! kept as PHC strings instead of salted SHA-512 digests; a robot has the
//! hash of an access token instead of an OAuth consumer secret; and a robot
//! account can be a built-in agent without a URL.

use std::collections::BTreeMap;
use std::fmt;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use resonator_model::id::ParticipantId;
use thiserror::Error;

/// Invalid account data.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AccountError {
    #[error("password hashing failed: {0}")]
    Hash(String),
    #[error("not an argon2id PHC string: {0}")]
    InvalidDigest(String),
    #[error("Url must not end with /")]
    UrlEndsWithSlash,
    #[error("unknown {what}: {value}")]
    UnknownName { what: &'static str, value: String },
}

/// An argon2id password hash in PHC string form
/// (`$argon2id$v=19$m=...,t=...,p=...$salt$hash`).
#[derive(Clone, PartialEq, Eq)]
pub struct PasswordDigest {
    phc: String,
}

impl PasswordDigest {
    /// Hashes a password with a fresh random salt and the default argon2id
    /// parameters. CPU-heavy (tens of milliseconds): call it from
    /// `spawn_blocking` in async code.
    pub fn new(password: &str) -> Result<Self, AccountError> {
        Self::with_params(password, Params::default())
    }

    /// [`PasswordDigest::new`] with explicit argon2 parameters.
    pub fn with_params(password: &str, params: Params) -> Result<Self, AccountError> {
        let mut salt = [0u8; 16];
        getrandom::fill(&mut salt).map_err(|e| AccountError::Hash(e.to_string()))?;
        let salt = SaltString::encode_b64(&salt).map_err(|e| AccountError::Hash(e.to_string()))?;
        let phc = Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| AccountError::Hash(e.to_string()))?
            .to_string();
        Ok(PasswordDigest { phc })
    }

    /// Wraps a stored PHC string after checking that it is an argon2id one.
    pub fn from_phc(phc: &str) -> Result<Self, AccountError> {
        let parsed =
            PasswordHash::new(phc).map_err(|e| AccountError::InvalidDigest(e.to_string()))?;
        if parsed.algorithm.as_str() != "argon2id" {
            return Err(AccountError::InvalidDigest(format!(
                "algorithm {}",
                parsed.algorithm
            )));
        }
        Ok(PasswordDigest {
            phc: phc.to_owned(),
        })
    }

    /// Whether `password` is the hashed one. CPU-heavy like hashing.
    pub fn verify(&self, password: &str) -> bool {
        PasswordHash::new(&self.phc)
            .map(|hash| {
                Argon2::default()
                    .verify_password(password.as_bytes(), &hash)
                    .is_ok()
            })
            .unwrap_or(false)
    }

    pub fn as_phc(&self) -> &str {
        &self.phc
    }
}

/// Does not print the hash.
impl fmt::Debug for PasswordDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PasswordDigest(..)")
    }
}

/// The account of a human (`HumanAccountData`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanAccountData {
    pub id: ParticipantId,
    /// Absent if password login is disabled or no password is set.
    pub password_digest: Option<PasswordDigest>,
    pub locale: Option<String>,
}

impl HumanAccountData {
    pub fn new(id: ParticipantId) -> Self {
        HumanAccountData {
            id,
            password_digest: None,
            locale: None,
        }
    }

    pub fn with_password(id: ParticipantId, password_digest: PasswordDigest) -> Self {
        HumanAccountData {
            id,
            password_digest: Some(password_digest),
            locale: None,
        }
    }
}

macro_rules! named_enum {
    ($(#[$doc:meta])* $name:ident, $what:literal, { $($variant:ident => $text:literal),* $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $($variant),*
        }

        impl $name {
            /// The name used by the original enum.
            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),*
                }
            }
        }

        impl std::str::FromStr for $name {
            type Err = AccountError;

            fn from_str(s: &str) -> Result<Self, AccountError> {
                match s {
                    $($text => Ok($name::$variant),)*
                    _ => Err(AccountError::UnknownName { what: $what, value: s.to_owned() }),
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

named_enum!(
    /// The events a robot can subscribe to (`EventType`).
    EventType, "event type", {
        WaveletBlipCreated => "WAVELET_BLIP_CREATED",
        WaveletBlipRemoved => "WAVELET_BLIP_REMOVED",
        WaveletParticipantsChanged => "WAVELET_PARTICIPANTS_CHANGED",
        WaveletSelfAdded => "WAVELET_SELF_ADDED",
        WaveletSelfRemoved => "WAVELET_SELF_REMOVED",
        WaveletTitleChanged => "WAVELET_TITLE_CHANGED",
        WaveletCreated => "WAVELET_CREATED",
        WaveletFetched => "WAVELET_FETCHED",
        WaveletTagsChanged => "WAVELET_TAGS_CHANGED",
        BlipContributorsChanged => "BLIP_CONTRIBUTORS_CHANGED",
        BlipSubmitted => "BLIP_SUBMITTED",
        DocumentChanged => "DOCUMENT_CHANGED",
        FormButtonClicked => "FORM_BUTTON_CLICKED",
        GadgetStateChanged => "GADGET_STATE_CHANGED",
        AnnotatedTextChanged => "ANNOTATED_TEXT_CHANGED",
        OperationError => "OPERATION_ERROR",
        Unknown => "UNKNOWN",
    }
);

named_enum!(
    /// Which blips around an event's blip are sent to the robot
    /// (`Context`).
    Context, "context", {
        Root => "ROOT",
        Parent => "PARENT",
        Siblings => "SIBLINGS",
        Children => "CHILDREN",
        SelfOnly => "SELF",
        All => "ALL",
    }
);

named_enum!(
    /// The robot wire protocol version (`ProtocolVersion`).
    ProtocolVersion, "protocol version", {
        V2 => "0.2",
        V2_1 => "0.21",
        V2_2 => "0.22",
    }
);

impl ProtocolVersion {
    pub const DEFAULT: ProtocolVersion = ProtocolVersion::V2_2;
}

/// A robot's subscription to one event type (`Capability`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    pub event_type: EventType,
    pub contexts: Vec<Context>,
    /// A regular expression the event must match; empty for no filter.
    pub filter: String,
}

impl Capability {
    /// The contexts used when a capability declares none.
    pub const DEFAULT_CONTEXTS: [Context; 3] = [Context::Root, Context::Parent, Context::Children];

    /// A capability with the default contexts and no filter.
    pub fn new(event_type: EventType) -> Self {
        Self::with_contexts(event_type, Self::DEFAULT_CONTEXTS.to_vec())
    }

    pub fn with_contexts(event_type: EventType, contexts: Vec<Context>) -> Self {
        Self::with_filter(event_type, contexts, "")
    }

    pub fn with_filter(
        event_type: EventType,
        contexts: Vec<Context>,
        filter: impl Into<String>,
    ) -> Self {
        Capability {
            event_type,
            contexts,
            filter: filter.into(),
        }
    }
}

/// What a robot declared in its `capabilities.xml` (`RobotCapabilities`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RobotCapabilities {
    pub capabilities: BTreeMap<EventType, Capability>,
    pub capabilities_hash: String,
    pub protocol_version: ProtocolVersion,
}

impl RobotCapabilities {
    pub fn new(
        capabilities: impl IntoIterator<Item = Capability>,
        capabilities_hash: impl Into<String>,
        protocol_version: ProtocolVersion,
    ) -> Self {
        RobotCapabilities {
            capabilities: capabilities
                .into_iter()
                .map(|capability| (capability.event_type, capability))
                .collect(),
            capabilities_hash: capabilities_hash.into(),
            protocol_version,
        }
    }
}

/// Whether a robot account is an external robot reached over HTTP or an
/// agent running inside the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RobotKind {
    /// An external robot with a URL.
    Robot,
    /// A built-in AI agent: a participant like any other, without a URL.
    Agent,
}

/// The account of a robot or agent (`RobotAccountData`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RobotAccountData {
    pub id: ParticipantId,
    pub kind: RobotKind,
    /// Where the robot is reached; never ends with `/`. Agents have none.
    url: Option<String>,
    /// Hash of the access token the robot authenticates with. Replaces the
    /// OAuth consumer secret of the original; the token scheme is defined
    /// by the authentication layer.
    pub access_token_hash: Option<String>,
    /// Absent until fetched from the robot.
    pub capabilities: Option<RobotCapabilities>,
    /// Whether ownership of the robot has been verified.
    pub is_verified: bool,
}

impl RobotAccountData {
    /// An external robot at `url`, which must not end with `/`.
    pub fn robot(
        id: ParticipantId,
        url: impl Into<String>,
        access_token_hash: Option<String>,
        capabilities: Option<RobotCapabilities>,
        is_verified: bool,
    ) -> Result<Self, AccountError> {
        let url = url.into();
        if url.ends_with('/') {
            return Err(AccountError::UrlEndsWithSlash);
        }
        Ok(RobotAccountData {
            id,
            kind: RobotKind::Robot,
            url: Some(url),
            access_token_hash,
            capabilities,
            is_verified,
        })
    }

    /// A built-in agent. Agents are trusted, so they count as verified.
    /// A robot without a URL: it is not called but listens to the
    /// server's event stream.
    pub fn streaming_robot(
        id: ParticipantId,
        access_token_hash: Option<String>,
        capabilities: Option<RobotCapabilities>,
    ) -> Self {
        RobotAccountData {
            id,
            kind: RobotKind::Robot,
            url: None,
            access_token_hash,
            capabilities,
            is_verified: true,
        }
    }

    /// Sets or removes the URL the robot is called at.
    pub fn set_url(&mut self, url: Option<String>) -> Result<(), AccountError> {
        if url.as_deref().is_some_and(|url| url.ends_with('/')) {
            return Err(AccountError::UrlEndsWithSlash);
        }
        self.url = url;
        Ok(())
    }

    pub fn agent(id: ParticipantId) -> Self {
        RobotAccountData {
            id,
            kind: RobotKind::Agent,
            url: None,
            access_token_hash: None,
            capabilities: None,
            is_verified: true,
        }
    }

    pub(crate) fn from_parts(
        id: ParticipantId,
        kind: RobotKind,
        url: Option<String>,
        access_token_hash: Option<String>,
        capabilities: Option<RobotCapabilities>,
        is_verified: bool,
    ) -> Result<Self, AccountError> {
        if url.as_deref().is_some_and(|url| url.ends_with('/')) {
            return Err(AccountError::UrlEndsWithSlash);
        }
        Ok(RobotAccountData {
            id,
            kind,
            url,
            access_token_hash,
            capabilities,
            is_verified,
        })
    }

    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    pub fn is_agent(&self) -> bool {
        self.kind == RobotKind::Agent
    }
}

/// An account, identified by its participant id (`AccountData`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountData {
    Human(HumanAccountData),
    Robot(RobotAccountData),
}

impl AccountData {
    pub fn id(&self) -> &ParticipantId {
        match self {
            AccountData::Human(account) => &account.id,
            AccountData::Robot(account) => &account.id,
        }
    }

    pub fn is_human(&self) -> bool {
        matches!(self, AccountData::Human(_))
    }

    pub fn is_robot(&self) -> bool {
        matches!(self, AccountData::Robot(_))
    }

    /// Whether this is a built-in agent.
    pub fn is_agent(&self) -> bool {
        matches!(self, AccountData::Robot(account) if account.is_agent())
    }

    pub fn as_human(&self) -> Option<&HumanAccountData> {
        match self {
            AccountData::Human(account) => Some(account),
            AccountData::Robot(_) => None,
        }
    }

    pub fn as_robot(&self) -> Option<&RobotAccountData> {
        match self {
            AccountData::Robot(account) => Some(account),
            AccountData::Human(_) => None,
        }
    }
}

impl From<HumanAccountData> for AccountData {
    fn from(account: HumanAccountData) -> Self {
        AccountData::Human(account)
    }
}

impl From<RobotAccountData> for AccountData {
    fn from(account: RobotAccountData) -> Self {
        AccountData::Robot(account)
    }
}
