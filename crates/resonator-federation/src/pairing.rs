//! Person mode: pairing a new device with a person by link (or QR code).
//!
//! An admin device (one holding the person key) makes an invitation
//! ([`Inviter::invite`]): the person key, its own device key and
//! addresses, an id, a one-time 128-bit secret and an expiry (ten
//! minutes), encoded as `resonator:pair/<base64url>` ([`encode_link`]).
//! The secret only travels in the link.
//!
//! The new device dials the inviting device on ALPN
//! [`PAIR_ALPN`](crate::endpoint::PAIR_ALPN) and sends `pair`: the
//! invitation id and HMAC-SHA256(secret, `"resonator/pair/1\0"` ‖ inviter
//! key ‖ joiner key ‖ id). The joiner key is the connection's (proved by
//! QUIC's TLS), so the MAC binds the secret to both keys and the secret
//! is never on the wire. The inviting device checks the MAC in constant
//! time (five wrong ones burn the invitation), issues the new device's
//! certificate and answers with it, the person's other certificates, the
//! revocation list, the host lease, co-host grants and device addresses.
//! The invitation is then used up.
//!
//! An admin invitation (the new device gets the person key) waits for
//! confirmation on the inviting device ([`Inviter::confirm`]) after the
//! first valid request; the joiner asks again until then. The person
//! key's BIP39 entropy travels XORed with HKDF-SHA256(secret,
//! `"resonator/pair/1 key"` ‖ inviter ‖ joiner), inside the QUIC stream.
//!
//! The new device ([`join`]) checks that the certificate is for its key
//! and the invitation's person (and, for an admin pairing, that the key
//! derived from the phrase is the person's), then [`install`] writes
//! `device.cert` (and the person key) and [`remember`] stores what it
//! learned; the node restarts as a device of the person and syncs.
//!
//! Every invitation can also be taken by a short code instead of the link
//! (see [`crate::pair_code`]): the joiner finds the inviting device by
//! the code's nameplate, runs SPAKE2 over the code's words
//! (`pair-code`), and the session key the PAKE gives takes the place of
//! the secret above; the inviting device also proves the key
//! ([`PairResult::confirm`]). [`join_code`] is the joiner's side.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use iroh::{EndpointAddr, PublicKey, RelayUrl};
use resonator_model::proto::federation_error::federation_error::Code;
use resonator_model::proto::federation_rpc::{
    FederationFrame, FederationRequest, Pair, PairCode as PairCodeRequest, PairCodeResult,
    PairInvitation, PairResult, SignedCoHost, SignedHostLease, federation_frame,
    federation_request, federation_response,
};
use resonator_model::proto::{decode, encode};
use resonator_server::storage::{FederationStore, PairInvitationRecord};
use sha2::Sha256;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::codec::{read_frame, write_frame};
use crate::endpoint::{PAIR_ALPN, PairService, new_request_id};
use crate::pair_code::{
    MAX_CODE_FAILURES, NAMEPLATE_DIGITS, PairCode, Rendezvous, RendezvousRecord, SessionContext,
    inviter_confirm, pake_answer, pake_finish, pake_start, random_nameplate, session_key,
};
use crate::person::{
    FileKeyStore, KeyStore, PersonKey, RecoveryPhrase, VerifiedCert, cert_to_hex, verify_cert,
    verify_revocations,
};

/// The prefix of pairing links; a QR code carries the same text.
pub const PAIR_LINK_PREFIX: &str = "resonator:pair/";
/// How long an invitation lasts.
pub const PAIR_LIFETIME: Duration = Duration::from_secs(10 * 60);
/// Wrong MACs that burn an invitation.
pub const MAX_PAIR_FAILURES: u32 = 5;

const MAC_CONTEXT: &[u8] = b"resonator/pair/1\0";
const SEAL_INFO: &[u8] = b"resonator/pair/1 key";

/// Why pairing failed.
#[derive(Debug, Error)]
pub enum PairError {
    #[error("not a pairing link")]
    BadLink,
    #[error("the pairing link has expired")]
    Expired,
    #[error("pairing refused: {0}")]
    Refused(String),
    #[error("cannot reach the inviting device: {0}")]
    Connect(String),
    #[error("the inviting device's answer does not check out: {0}")]
    BadAnswer(String),
    #[error("no confirmation on the inviting device in time")]
    NotConfirmed,
    #[error("this device holds no person key, so it cannot invite devices")]
    NotAdmin,
    #[error("{0}")]
    BadCode(String),
    #[error("code not found or expired: check the digits, or make a new code")]
    CodeNotFound,
    #[error("{}", wrong_code_text(*left))]
    WrongCode { left: u32 },
    #[error("{0}")]
    Other(String),
}

fn wrong_code_text(left: u32) -> String {
    match left {
        0 => "wrong code; the code is used up now, make a new one".to_owned(),
        1 => "wrong code (1 try left)".to_owned(),
        left => format!("wrong code ({left} tries left)"),
    }
}

/// How far a joining device has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinStage {
    /// Looking the code's nameplate up (pairing by code only).
    Finding,
    /// Dialled the inviting device; proving the code or link.
    Verifying,
    /// An admin pairing waits for confirmation on the inviting device.
    Waiting,
    /// Paired; writing what makes this device one of the person's.
    Done,
}

impl JoinStage {
    pub fn as_str(self) -> &'static str {
        match self {
            JoinStage::Finding => "finding",
            JoinStage::Verifying => "verifying",
            JoinStage::Waiting => "waiting",
            JoinStage::Done => "done",
        }
    }
}

/// Where a join reports its [`JoinStage`].
pub type Progress<'a> = &'a (dyn Fn(JoinStage) + Send + Sync);

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

fn public_key(bytes: Option<&[u8]>) -> Option<PublicKey> {
    let bytes: &[u8; 32] = bytes?.try_into().ok()?;
    PublicKey::from_bytes(bytes).ok()
}

/// `resonator:pair/<base64url of the invitation>`.
pub fn encode_link(invitation: &PairInvitation) -> String {
    format!(
        "{PAIR_LINK_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(encode(invitation).unwrap_or_default())
    )
}

/// The invitation a pairing link carries (surrounding whitespace is
/// ignored).
pub fn decode_link(link: &str) -> Result<PairInvitation, PairError> {
    let body = link
        .trim()
        .strip_prefix(PAIR_LINK_PREFIX)
        .ok_or(PairError::BadLink)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(body.trim_end_matches('='))
        .map_err(|_| PairError::BadLink)?;
    let invitation: PairInvitation = decode(&bytes).map_err(|_| PairError::BadLink)?;
    if public_key(invitation.person.as_deref()).is_none()
        || public_key(invitation.device.as_deref()).is_none()
        || invitation
            .secret
            .as_ref()
            .is_none_or(|secret| secret.len() != 16)
    {
        return Err(PairError::BadLink);
    }
    Ok(invitation)
}

/// A QR code of `text` as an SVG document.
pub fn qr_svg(text: &str) -> String {
    match qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M) {
        Ok(code) => code
            .render::<qrcode::render::svg::Color<'_>>()
            .min_dimensions(200, 200)
            .quiet_zone(true)
            .build(),
        Err(_) => String::new(),
    }
}

/// The joiner's proof that it was shown the link.
fn pair_mac(secret: &[u8], inviter: &PublicKey, joiner: &PublicKey, id: &str) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret).expect("HMAC takes any key length");
    mac.update(MAC_CONTEXT);
    mac.update(inviter.as_bytes());
    mac.update(joiner.as_bytes());
    mac.update(id.as_bytes());
    mac
}

/// The pad that seals the person key for the joiner.
fn seal_pad(secret: &[u8], inviter: &PublicKey, joiner: &PublicKey) -> Zeroizing<[u8; 32]> {
    let hkdf = Hkdf::<Sha256>::new(None, secret);
    let mut pad = Zeroizing::new([0u8; 32]);
    let info = [SEAL_INFO, inviter.as_bytes(), joiner.as_bytes()].concat();
    hkdf.expand(&info, pad.as_mut())
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    pad
}

fn xor(a: &[u8; 32], b: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    let mut out = Zeroizing::new([0u8; 32]);
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = a[index] ^ b[index];
    }
    out
}

/// A pairing invitation as the inviting device shows it (never with its
/// secret).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPairing {
    pub id: String,
    pub admin: bool,
    pub expires_at: i64,
    /// `open`, `requested` (waits for confirmation) or `confirmed`.
    pub state: String,
    /// The device that asked (`<z32>`), once one did.
    pub joiner: Option<String>,
    pub joiner_name: Option<String>,
}

impl From<&PairInvitationRecord> for PendingPairing {
    fn from(record: &PairInvitationRecord) -> Self {
        PendingPairing {
            id: record.id.clone(),
            admin: record.admin,
            expires_at: record.expires_at,
            state: record.state.clone(),
            joiner: public_key(record.joiner.as_deref()).map(|key| key.to_z32()),
            joiner_name: record.joiner_name.clone(),
        }
    }
}

/// A new invitation: its link (holding the secret: show it only to the
/// person), its code if the rendezvous took it, and how it shows in the
/// list.
#[derive(Clone)]
pub struct NewInvitation {
    pub link: String,
    /// `<nameplate>-<word>-<word>`. Never log it.
    pub code: Option<String>,
    pub pending: PendingPairing,
}

impl std::fmt::Debug for NewInvitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewInvitation")
            .field("pending", &self.pending)
            .field("code", &self.code.is_some())
            .finish_non_exhaustive()
    }
}

/// What the inviting device hands a new device besides its certificate.
#[derive(Debug, Clone, Default)]
pub struct PersonBundle {
    pub certs: Vec<resonator_model::proto::federation_rpc::SignedDeviceCert>,
    pub revocations: Option<resonator_model::proto::federation_rpc::SignedRevocationList>,
    pub leases: Vec<SignedHostLease>,
    pub co_hosts: Vec<SignedCoHost>,
    /// `<z32 key> <host:port>`.
    pub device_addrs: Vec<String>,
}

/// What the inviting side needs from the running node.
#[async_trait]
pub trait InviterHost: Send + Sync {
    /// The federation store.
    fn store(&self) -> FederationStore;
    /// This device's key.
    fn device(&self) -> PublicKey;
    /// This device's address (key, sockets, relay).
    fn addr(&self) -> EndpointAddr;
    /// Records the new device's certificate and addresses (trust, static
    /// devices, dial addresses).
    async fn learn_device(&self, cert: &VerifiedCert, addrs: &[std::net::SocketAddr]);
    /// What the person's devices know, for the new device.
    async fn bundle(&self) -> PersonBundle;
}

/// How long a call to the rendezvous may take.
const RENDEZVOUS_TIMEOUT: Duration = Duration::from_secs(10);
/// Nameplates tried before an invitation goes without a code.
const NAMEPLATE_TRIES: usize = 5;
/// Most PAKE sessions kept at once (one per joining device).
const MAX_CODE_SESSIONS: usize = 64;

/// A joining device that ran the PAKE: the invitation and the session
/// key. Never log the key.
struct CodeSession {
    id: String,
    key: Zeroizing<[u8; 32]>,
    expires_at: i64,
    started: tokio::time::Instant,
}

/// The inviting side of pairing, on an admin device.
pub struct Inviter {
    host: Arc<dyn InviterHost>,
    key_path: std::path::PathBuf,
    account: String,
    /// Pairing by code: where codes are published (none: links only).
    rendezvous: Option<Arc<dyn Rendezvous>>,
    /// Pairing by code: the sessions of joining devices, by their key.
    sessions: std::sync::Mutex<HashMap<PublicKey, CodeSession>>,
    /// Checks of a code's MAC happen one at a time, so that no more than
    /// [`MAX_CODE_FAILURES`] wrong ones are ever tried.
    code_checks: tokio::sync::Mutex<()>,
}

impl Inviter {
    pub fn new(host: Arc<dyn InviterHost>, key_path: std::path::PathBuf, account: String) -> Self {
        Inviter {
            host,
            key_path,
            account,
            rendezvous: None,
            sessions: std::sync::Mutex::new(HashMap::new()),
            code_checks: tokio::sync::Mutex::new(()),
        }
    }

    /// Makes invitations with a code too, published on `rendezvous`.
    pub fn with_rendezvous(mut self, rendezvous: Option<Arc<dyn Rendezvous>>) -> Self {
        self.rendezvous = rendezvous;
        self
    }

    fn person_key(&self) -> Result<(PersonKey, RecoveryPhrase), PairError> {
        let phrase = FileKeyStore::new(&self.key_path)
            .load()
            .map_err(|error| PairError::Other(error.to_string()))?
            .ok_or(PairError::NotAdmin)?;
        Ok((PersonKey::from_phrase(&phrase), phrase))
    }

    /// Makes an invitation; `admin` gives the new device the person key
    /// (after confirmation here). With a rendezvous, it has a code too
    /// (none if the rendezvous cannot be reached).
    pub async fn invite(&self, admin: bool) -> Result<NewInvitation, PairError> {
        let (key, _) = self.person_key()?;
        let store = self.host.store();
        let now = now_millis();
        let _ = store.prune_pair_invitations(now).await;
        let mut secret = Zeroizing::new([0u8; 16]);
        getrandom::fill(secret.as_mut()).map_err(|e| PairError::Other(e.to_string()))?;
        let id = new_request_id();
        let expires_at = now + i64::try_from(PAIR_LIFETIME.as_millis()).unwrap_or(i64::MAX);
        let addr = self.host.addr();
        let code = match &self.rendezvous {
            Some(rendezvous) => {
                self.publish_code(rendezvous.as_ref(), &addr, now, expires_at)
                    .await
            }
            None => None,
        };
        let record = PairInvitationRecord {
            id: id.clone(),
            secret: secret.to_vec(),
            admin,
            created_at: now,
            expires_at,
            state: "open".to_owned(),
            joiner: None,
            joiner_name: None,
            failures: 0,
            nameplate: code.as_ref().map(|code| code.nameplate().to_owned()),
            code: code.as_ref().map(|code| code.password().to_vec()),
            code_failures: 0,
        };
        store
            .put_pair_invitation(record.clone())
            .await
            .map_err(|e| PairError::Other(e.to_string()))?;
        let invitation = PairInvitation {
            person: Some(key.public().as_bytes().to_vec()),
            device: Some(self.host.device().as_bytes().to_vec()),
            addr: addr.ip_addrs().map(ToString::to_string).collect(),
            relay: addr.relay_urls().next().map(ToString::to_string),
            id: Some(id),
            secret: Some(secret.to_vec()),
            expires_at: Some(expires_at),
            admin: Some(admin),
            account: Some(self.account.clone()),
        };
        Ok(NewInvitation {
            link: encode_link(&invitation),
            code: code.map(|code| code.to_string()),
            pending: PendingPairing::from(&record),
        })
    }

    /// Picks a code whose nameplate is free (here and on the rendezvous)
    /// and publishes where this device is under it. `None` if the
    /// rendezvous cannot be reached.
    async fn publish_code(
        &self,
        rendezvous: &dyn Rendezvous,
        addr: &EndpointAddr,
        now: i64,
        expires_at: i64,
    ) -> Option<PairCode> {
        let mut code = PairCode::generate().ok()?;
        let store = self.host.store();
        let record = RendezvousRecord {
            node: self.host.device(),
            relay: addr.relay_urls().next().cloned(),
            addrs: addr.ip_addrs().copied().collect(),
            expires_at,
        };
        for attempt in 0..NAMEPLATE_TRIES {
            if attempt > 0 {
                code = code.with_nameplate(random_nameplate().ok()?);
            }
            let ours = store
                .pair_invitation_by_nameplate(code.nameplate())
                .await
                .ok()
                .flatten()
                .is_some_and(|record| record.expires_at > now);
            if ours {
                continue;
            }
            // Another device's invitation may hold the nameplate now.
            match tokio::time::timeout(
                RENDEZVOUS_TIMEOUT,
                crate::pair_code::lookup(rendezvous, code.nameplate(), now),
            )
            .await
            {
                Ok(Ok(Some(_))) => continue,
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "no pairing code: the rendezvous cannot be read");
                    return None;
                }
                Err(_) => {
                    tracing::warn!("no pairing code: the rendezvous does not answer");
                    return None;
                }
            }
            return match tokio::time::timeout(
                RENDEZVOUS_TIMEOUT,
                crate::pair_code::publish(rendezvous, code.nameplate(), &record),
            )
            .await
            {
                Ok(Ok(())) => Some(code),
                Ok(Err(error)) => {
                    tracing::warn!(%error, "no pairing code: the rendezvous record was not published");
                    None
                }
                Err(_) => {
                    tracing::warn!("no pairing code: the rendezvous does not answer");
                    None
                }
            };
        }
        tracing::warn!("no pairing code: no free nameplate found");
        None
    }

    /// Takes the rendezvous record of an invitation's code down (in the
    /// background).
    fn withdraw_code(&self, nameplate: Option<String>) {
        let (Some(rendezvous), Some(nameplate)) = (self.rendezvous.clone(), nameplate) else {
            return;
        };
        tokio::spawn(async move { withdraw_now(rendezvous.as_ref(), &nameplate).await });
    }

    /// The invitations not used up yet.
    pub async fn invitations(&self) -> Vec<PendingPairing> {
        let now = now_millis();
        let store = self.host.store();
        let _ = store.prune_pair_invitations(now).await;
        store
            .pair_invitations()
            .await
            .unwrap_or_default()
            .iter()
            .map(PendingPairing::from)
            .collect()
    }

    /// Confirms an admin pairing a device asked for.
    pub async fn confirm(&self, id: &str) -> Result<(), PairError> {
        let store = self.host.store();
        match store
            .update_pair_invitation(id, "requested", "confirmed", None, None)
            .await
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(PairError::Refused(
                "no device waits for confirmation of that invitation".to_owned(),
            )),
            Err(error) => Err(PairError::Other(error.to_string())),
        }
    }

    /// Withdraws an invitation (and its code).
    pub async fn cancel(&self, id: &str) -> Result<bool, PairError> {
        let store = self.host.store();
        let nameplate = store
            .pair_invitation(id)
            .await
            .ok()
            .flatten()
            .and_then(|record| record.nameplate);
        let removed = store
            .delete_pair_invitation(id)
            .await
            .map_err(|e| PairError::Other(e.to_string()))?;
        if let (Some(rendezvous), Some(nameplate)) = (&self.rendezvous, nameplate) {
            withdraw_now(rendezvous.as_ref(), &nameplate).await;
        }
        Ok(removed)
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<PublicKey, CodeSession>> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The session key of `joiner` for invitation `id`, if it ran the PAKE.
    fn session_of(&self, joiner: &PublicKey, id: &str) -> Option<Zeroizing<[u8; 32]>> {
        let now = now_millis();
        let mut sessions = self.sessions();
        sessions.retain(|_, session| session.expires_at > now);
        sessions
            .get(joiner)
            .filter(|session| session.id == id)
            .map(|session| session.key.clone())
    }

    fn forget_session(&self, joiner: &PublicKey) {
        self.sessions().remove(joiner);
    }

    fn keep_session(&self, joiner: PublicKey, session: CodeSession) {
        let now = now_millis();
        let mut sessions = self.sessions();
        sessions.retain(|_, session| session.expires_at > now);
        if sessions.len() >= MAX_CODE_SESSIONS
            && !sessions.contains_key(&joiner)
            && let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, session)| session.started)
                .map(|(key, _)| *key)
        {
            sessions.remove(&oldest);
        }
        sessions.insert(joiner, session);
    }

    /// `pair-code`: the inviting device's side of the PAKE. Keeps the
    /// session key for the `pair` request that follows.
    async fn start_code(
        &self,
        joiner: PublicKey,
        request: PairCodeRequest,
    ) -> Result<PairCodeResult, PairError> {
        let nameplate = request.nameplate.unwrap_or_default();
        if nameplate.len() != NAMEPLATE_DIGITS || !nameplate.chars().all(|c| c.is_ascii_digit()) {
            return Err(PairError::CodeNotFound);
        }
        let store = self.host.store();
        let record = store
            .pair_invitation_by_nameplate(&nameplate)
            .await
            .map_err(|e| PairError::Other(e.to_string()))?
            .ok_or(PairError::CodeNotFound)?;
        let Some(password) = record.code.as_deref() else {
            return Err(PairError::CodeNotFound);
        };
        if now_millis() >= record.expires_at || record.code_failures >= MAX_CODE_FAILURES {
            return Err(PairError::CodeNotFound);
        }
        let me = self.host.device();
        let (answer, pake_key) = pake_answer(
            password,
            &nameplate,
            &joiner,
            &me,
            request.pake.as_deref().unwrap_or_default(),
        )
        .map_err(|_| PairError::Refused("not a PAKE message".to_owned()))?;
        let (key, _) = self.person_key()?;
        let person = key.public();
        let session = session_key(
            &pake_key,
            &SessionContext {
                id: &record.id,
                person: &person,
                admin: record.admin,
                account: &self.account,
            },
        );
        self.keep_session(
            joiner,
            CodeSession {
                id: record.id.clone(),
                key: session,
                expires_at: record.expires_at,
                started: tokio::time::Instant::now(),
            },
        );
        Ok(PairCodeResult {
            pake: Some(answer),
            invitation: Some(record.id),
            person: Some(person.as_bytes().to_vec()),
            admin: Some(record.admin),
            account: Some(self.account.clone()),
            expires_at: Some(record.expires_at),
        })
    }

    async fn answer(&self, joiner: PublicKey, request: Pair) -> Result<PairResult, PairError> {
        let refused = |message: &str| PairError::Refused(message.to_owned());
        let store = self.host.store();
        let id = request.invitation.unwrap_or_default();
        let session = self.session_of(&joiner, &id);
        // A code's MAC is checked one at a time (see `code_checks`).
        let _checking = match &session {
            Some(_) => Some(self.code_checks.lock().await),
            None => None,
        };
        let record = store
            .pair_invitation(&id)
            .await
            .map_err(|e| PairError::Other(e.to_string()))?
            .ok_or_else(|| refused("no such invitation (used, expired or withdrawn)"))?;
        let now = now_millis();
        if now >= record.expires_at || record.state == "used" {
            let _ = store.delete_pair_invitation(&id).await;
            self.withdraw_code(record.nameplate.clone());
            return Err(refused("the invitation has expired or was used"));
        }
        let me = self.host.device();
        // Pairing by code: the session key stands in for the link's secret.
        let secret: &[u8] = match &session {
            Some(session) => session.as_slice(),
            None => &record.secret,
        };
        let mac = pair_mac(secret, &me, &joiner, &id);
        if mac
            .verify_slice(request.mac.as_deref().unwrap_or_default())
            .is_err()
        {
            if session.is_some() {
                self.forget_session(&joiner);
                let failures = store.pair_code_failed(&id).await.unwrap_or(u32::MAX);
                let left = MAX_CODE_FAILURES.saturating_sub(failures);
                if left == 0 {
                    let _ = store.delete_pair_invitation(&id).await;
                    self.withdraw_code(record.nameplate.clone());
                    tracing::warn!(invitation = %id, "a pairing invitation was burnt by wrong codes");
                }
                return Err(PairError::WrongCode { left });
            }
            let failures = store.pair_invitation_failed(&id).await.unwrap_or(u32::MAX);
            if failures >= MAX_PAIR_FAILURES {
                let _ = store.delete_pair_invitation(&id).await;
                self.withdraw_code(record.nameplate.clone());
                tracing::warn!(invitation = %id, "a pairing invitation was burnt by wrong secrets");
            }
            return Err(refused("wrong secret"));
        }
        let confirm = session.as_ref().map(|session| {
            inviter_confirm(session.as_slice(), &me, &joiner, &id)
                .finalize()
                .into_bytes()
                .to_vec()
        });
        let waiting = || PairResult {
            waiting: Some(true),
            confirm: confirm.clone(),
            ..PairResult::default()
        };
        let name = request.device_name.unwrap_or_default();
        let joiner_bytes = joiner.as_bytes().to_vec();
        let same_joiner = record.joiner.as_deref() == Some(joiner_bytes.as_slice());
        if record.admin {
            match record.state.as_str() {
                "open" => {
                    let moved = store
                        .update_pair_invitation(
                            &id,
                            "open",
                            "requested",
                            Some(joiner_bytes),
                            Some(name.chars().take(64).collect()),
                        )
                        .await
                        .map_err(|e| PairError::Other(e.to_string()))?;
                    if !moved {
                        return Err(refused("another device asked first"));
                    }
                    tracing::info!(joiner = %joiner.fmt_short(), "an admin pairing waits for confirmation");
                    return Ok(waiting());
                }
                "requested" if same_joiner => return Ok(waiting()),
                "confirmed" if same_joiner => {}
                _ => return Err(refused("another device asked first")),
            }
        }
        let from = if record.admin { "confirmed" } else { "open" };
        if !store
            .update_pair_invitation(&id, from, "used", Some(joiner_bytes), Some(name.clone()))
            .await
            .map_err(|e| PairError::Other(e.to_string()))?
        {
            return Err(refused("the invitation was used"));
        }
        self.forget_session(&joiner);
        self.withdraw_code(record.nameplate.clone());
        let (key, phrase) = self.person_key()?;
        let signed = key.issue_cert(&joiner, &name, record.admin, now);
        let cert = verify_cert(&signed).map_err(|e| PairError::Other(e.to_string()))?;
        let addrs: Vec<std::net::SocketAddr> = request
            .addr
            .iter()
            .filter_map(|addr| addr.parse().ok())
            .take(16)
            .collect();
        self.host.learn_device(&cert, &addrs).await;
        let bundle = self.host.bundle().await;
        let sealed_key = record.admin.then(|| {
            let pad = seal_pad(secret, &me, &joiner);
            xor(&phrase.entropy(), &pad).to_vec()
        });
        let _ = store.delete_pair_invitation(&id).await;
        tracing::info!(
            device = %joiner.fmt_short(),
            admin = record.admin,
            by_code = session.is_some(),
            "paired a new device"
        );
        Ok(PairResult {
            waiting: None,
            cert: Some(signed),
            device_cert: bundle.certs,
            revocations: bundle.revocations,
            host_lease: bundle.leases,
            sealed_key,
            device_addr: bundle.device_addrs,
            co_host: bundle.co_hosts,
            account: Some(self.account.clone()),
            confirm,
            tries_left: None,
        })
    }
}

/// Replaces the rendezvous record of `nameplate` by an empty one; if that
/// fails, the record stays until it expires (it holds its expiry).
async fn withdraw_now(rendezvous: &dyn Rendezvous, nameplate: &str) {
    let withdrawn = tokio::time::timeout(
        RENDEZVOUS_TIMEOUT,
        crate::pair_code::withdraw(rendezvous, nameplate),
    )
    .await;
    if !matches!(withdrawn, Ok(Ok(()))) {
        tracing::debug!("a pairing code's rendezvous record stays until it expires");
    }
}

fn refusal(joiner: &PublicKey, error: &PairError) -> federation_response::Result {
    tracing::info!(joiner = %joiner.fmt_short(), %error, "refused a pairing request");
    let code = match error {
        PairError::Other(_) => Code::InternalServerError,
        PairError::CodeNotFound => Code::ItemNotFound,
        _ => Code::NotAuthorized,
    };
    federation_response::Result::Error(
        resonator_model::proto::federation_error::FederationError::new(code, error.to_string()),
    )
}

#[async_trait]
impl PairService for Inviter {
    async fn pair(&self, joiner: PublicKey, request: Pair) -> federation_response::Result {
        match self.answer(joiner, request).await {
            Ok(result) => federation_response::Result::Pair(result),
            Err(PairError::WrongCode { left }) => {
                tracing::info!(joiner = %joiner.fmt_short(), left, "a wrong pairing code was tried");
                federation_response::Result::Pair(PairResult {
                    tries_left: Some(left),
                    ..PairResult::default()
                })
            }
            Err(error) => refusal(&joiner, &error),
        }
    }

    async fn pair_code(
        &self,
        joiner: PublicKey,
        request: PairCodeRequest,
    ) -> federation_response::Result {
        match self.start_code(joiner, request).await {
            Ok(result) => federation_response::Result::PairCode(result),
            Err(error) => refusal(&joiner, &error),
        }
    }
}

/// What the new device got from pairing.
pub struct Joined {
    pub cert: VerifiedCert,
    /// Admin pairing: the person key's phrase.
    pub phrase: Option<RecoveryPhrase>,
    pub inviter: PublicKey,
    pub inviter_addrs: Vec<std::net::SocketAddr>,
    pub account: Option<String>,
    pub result: PairResult,
}

impl std::fmt::Debug for Joined {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Joined")
            .field("device", &self.cert.device.fmt_short().to_string())
            .field("person", &self.cert.person.fmt_short().to_string())
            .field("admin", &self.cert.admin)
            .finish_non_exhaustive()
    }
}

async fn ask(
    connection: &iroh::endpoint::Connection,
    op: federation_request::Op,
) -> Result<federation_response::Result, PairError> {
    let connect = |e: &dyn std::fmt::Display| PairError::Connect(e.to_string());
    let (mut send, mut recv) = connection.open_bi().await.map_err(|e| connect(&e))?;
    let frame = FederationFrame {
        body: Some(federation_frame::Body::Request(FederationRequest {
            id: Some(new_request_id()),
            op: Some(op),
        })),
    };
    write_frame(&mut send, &frame)
        .await
        .map_err(|e| connect(&e))?;
    send.finish().map_err(|e| connect(&e))?;
    let answer = tokio::time::timeout(Duration::from_secs(30), read_frame(&mut recv))
        .await
        .map_err(|_| PairError::Connect("no answer".to_owned()))?
        .map_err(|e| connect(&e))?;
    match answer.body {
        Some(federation_frame::Body::Response(response)) => response
            .result
            .ok_or_else(|| PairError::BadAnswer("empty answer".to_owned())),
        _ => Err(PairError::BadAnswer("not an answer".to_owned())),
    }
}

/// What the joiner knows of the invitation it takes, by link or by code.
struct Target {
    inviter: PublicKey,
    person: PublicKey,
    admin: bool,
    id: String,
    /// The link's secret, or the session key of a code. Never log it.
    secret: Zeroizing<Vec<u8>>,
    /// Pairing by code: the inviting device must confirm the key.
    by_code: bool,
    inviter_addrs: Vec<std::net::SocketAddr>,
}

async fn dial(
    endpoint: &iroh::Endpoint,
    addr: EndpointAddr,
) -> Result<iroh::endpoint::Connection, PairError> {
    tokio::time::timeout(Duration::from_secs(30), endpoint.connect(addr, PAIR_ALPN))
        .await
        .map_err(|_| PairError::Connect("timed out".to_owned()))?
        .map_err(|e| PairError::Connect(e.to_string()))
}

/// Asks the inviting device for a certificate (again while an admin
/// pairing waits for confirmation, up to `wait`) and checks what comes
/// back.
async fn exchange(
    connection: &iroh::endpoint::Connection,
    me: PublicKey,
    target: Target,
    device_name: &str,
    own_addrs: Vec<String>,
    wait: Duration,
    progress: Progress<'_>,
) -> Result<Joined, PairError> {
    let request = Pair {
        invitation: Some(target.id.clone()),
        mac: Some(
            pair_mac(&target.secret, &target.inviter, &me, &target.id)
                .finalize()
                .into_bytes()
                .to_vec(),
        ),
        device_name: Some(device_name.to_owned()),
        addr: own_addrs,
    };
    let confirmed = |result: &PairResult| {
        !target.by_code
            || result.confirm.as_deref().is_some_and(|tag| {
                inviter_confirm(&target.secret, &target.inviter, &me, &target.id)
                    .verify_slice(tag)
                    .is_ok()
            })
    };
    let unconfirmed = || {
        PairError::BadAnswer("the inviting device did not prove that it knows the code".to_owned())
    };
    let deadline = tokio::time::Instant::now() + wait;
    let mut waiting = false;
    let result = loop {
        match ask(connection, federation_request::Op::Pair(request.clone())).await? {
            federation_response::Result::Pair(PairResult {
                tries_left: Some(left),
                ..
            }) => return Err(PairError::WrongCode { left }),
            federation_response::Result::Pair(result) if !confirmed(&result) => {
                return Err(unconfirmed());
            }
            federation_response::Result::Pair(result) if result.waiting == Some(true) => {
                if !waiting {
                    waiting = true;
                    progress(JoinStage::Waiting);
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(PairError::NotConfirmed);
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            federation_response::Result::Pair(result) => break result,
            federation_response::Result::Error(error) => {
                return Err(PairError::Refused(
                    error.error_message.clone().unwrap_or_default(),
                ));
            }
            _ => {
                return Err(PairError::BadAnswer(
                    "answered with something else".to_owned(),
                ));
            }
        }
    };
    let signed = result
        .cert
        .clone()
        .ok_or_else(|| PairError::BadAnswer("no certificate".to_owned()))?;
    let cert = verify_cert(&signed).map_err(|e| PairError::BadAnswer(e.to_string()))?;
    if cert.device != me || cert.person != target.person {
        return Err(PairError::BadAnswer(
            "the certificate is not for this device and the invitation's person".to_owned(),
        ));
    }
    let phrase = if target.admin {
        let sealed: [u8; 32] = result
            .sealed_key
            .as_deref()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| PairError::BadAnswer("no person key".to_owned()))?;
        let entropy = xor(&sealed, &seal_pad(&target.secret, &target.inviter, &me));
        let phrase = RecoveryPhrase::from_entropy(&entropy);
        if PersonKey::from_phrase(&phrase).public() != target.person {
            return Err(PairError::BadAnswer(
                "the person key is not the invitation's person".to_owned(),
            ));
        }
        Some(phrase)
    } else {
        None
    };
    progress(JoinStage::Done);
    Ok(Joined {
        cert,
        phrase,
        inviter: target.inviter,
        inviter_addrs: target.inviter_addrs,
        account: result.account.clone(),
        result,
    })
}

/// Closes `connection` whatever `result` is.
fn closing<T>(
    connection: &iroh::endpoint::Connection,
    result: Result<T, PairError>,
) -> Result<T, PairError> {
    let reason: &[u8] = if result.is_ok() { b"done" } else { b"refused" };
    connection.close(0u32.into(), reason);
    result
}

fn no_progress(_: JoinStage) {}

/// Pairs this device (the key of `endpoint`) by `link`: dials the
/// inviting device, proves the secret and, for an admin pairing, asks
/// again until the inviting device confirms (up to `wait`). Writes
/// nothing; see [`install`] and [`remember`].
pub async fn join(
    endpoint: &iroh::Endpoint,
    link: &str,
    device_name: &str,
    own_addrs: Vec<String>,
    wait: Duration,
) -> Result<Joined, PairError> {
    join_with_progress(endpoint, link, device_name, own_addrs, wait, &no_progress).await
}

/// [`join`], telling `progress` how far it came.
pub async fn join_with_progress(
    endpoint: &iroh::Endpoint,
    link: &str,
    device_name: &str,
    own_addrs: Vec<String>,
    wait: Duration,
    progress: Progress<'_>,
) -> Result<Joined, PairError> {
    let invitation = decode_link(link)?;
    if invitation.expires_at.unwrap_or(0) <= now_millis() {
        return Err(PairError::Expired);
    }
    let person = public_key(invitation.person.as_deref()).ok_or(PairError::BadLink)?;
    let inviter = public_key(invitation.device.as_deref()).ok_or(PairError::BadLink)?;
    let me = endpoint.id();
    if inviter == me {
        return Err(PairError::Refused(
            "this is the inviting device itself".to_owned(),
        ));
    }
    let mut addr = EndpointAddr::new(inviter);
    let inviter_addrs: Vec<std::net::SocketAddr> = invitation
        .addr
        .iter()
        .filter_map(|addr| addr.parse().ok())
        .collect();
    for socket in &inviter_addrs {
        addr = addr.with_ip_addr(*socket);
    }
    if let Some(relay) = invitation
        .relay
        .as_deref()
        .and_then(|url| url.parse::<RelayUrl>().ok())
    {
        addr = addr.with_relay_url(relay);
    }
    progress(JoinStage::Verifying);
    let connection = dial(endpoint, addr).await?;
    let target = Target {
        inviter,
        person,
        admin: invitation.admin == Some(true),
        id: invitation.id.clone().unwrap_or_default(),
        secret: Zeroizing::new(invitation.secret.clone().unwrap_or_default()),
        by_code: false,
        inviter_addrs,
    };
    let joined = exchange(
        &connection,
        me,
        target,
        device_name,
        own_addrs,
        wait,
        progress,
    )
    .await;
    closing(&connection, joined)
}

/// How often the rendezvous is asked before a code counts as unknown
/// (only failures to reach it are retried; "not there" is final).
const LOOKUP_TRIES: u32 = 3;

/// Pairs this device (the key of `endpoint`) by a code: finds the
/// inviting device through `rendezvous`, runs the PAKE, then asks for a
/// certificate as [`join`] does, with the session key in place of the
/// link's secret; the inviting device has to prove the key too. Refuses
/// an invitation for another account than `account` (device mode).
/// Writes nothing; see [`install`] and [`remember`].
#[allow(clippy::too_many_arguments)]
pub async fn join_code(
    endpoint: &iroh::Endpoint,
    rendezvous: &dyn Rendezvous,
    code: &PairCode,
    device_name: &str,
    own_addrs: Vec<String>,
    wait: Duration,
    account: Option<&str>,
    progress: Progress<'_>,
) -> Result<Joined, PairError> {
    progress(JoinStage::Finding);
    let mut tries = 0;
    let record = loop {
        tries += 1;
        match crate::pair_code::lookup(rendezvous, code.nameplate(), now_millis()).await {
            Ok(Some(record)) => break record,
            Ok(None) => return Err(PairError::CodeNotFound),
            Err(error) if tries >= LOOKUP_TRIES => return Err(PairError::Connect(error)),
            Err(error) => {
                tracing::debug!(%error, "the pairing rendezvous failed; trying again");
                tokio::time::sleep(Duration::from_millis(500 * u64::from(tries))).await;
            }
        }
    };
    let me = endpoint.id();
    let inviter = record.node;
    if inviter == me {
        return Err(PairError::Refused(
            "this is the inviting device itself".to_owned(),
        ));
    }
    let mut addr = EndpointAddr::new(inviter);
    for socket in &record.addrs {
        addr = addr.with_ip_addr(*socket);
    }
    if let Some(relay) = record.relay.clone() {
        addr = addr.with_relay_url(relay);
    }
    progress(JoinStage::Verifying);
    let connection = dial(endpoint, addr).await?;
    let joined = async {
        let (state, message) = pake_start(code, &me, &inviter);
        let started = match ask(
            &connection,
            federation_request::Op::PairCode(PairCodeRequest {
                nameplate: Some(code.nameplate().to_owned()),
                pake: Some(message),
            }),
        )
        .await?
        {
            federation_response::Result::PairCode(started) => started,
            federation_response::Result::Error(error) if error.code() == Code::ItemNotFound => {
                return Err(PairError::CodeNotFound);
            }
            federation_response::Result::Error(error) => {
                return Err(PairError::Refused(error.message().to_owned()));
            }
            _ => {
                return Err(PairError::BadAnswer(
                    "answered with something else".to_owned(),
                ));
            }
        };
        let person = public_key(started.person.as_deref())
            .ok_or_else(|| PairError::BadAnswer("no person key".to_owned()))?;
        let pake_key = pake_finish(state, started.pake.as_deref().unwrap_or_default())
            .map_err(|_| PairError::BadAnswer("not a PAKE message".to_owned()))?;
        let id = started.invitation.clone().unwrap_or_default();
        let admin = started.admin == Some(true);
        let their_account = started.account.clone().unwrap_or_default();
        if let Some(account) = account
            && !their_account.is_empty()
            && their_account != account
        {
            return Err(PairError::Refused(format!(
                "the invitation is for the account '{their_account}', this device's is '{account}'"
            )));
        }
        let session = session_key(
            &pake_key,
            &SessionContext {
                id: &id,
                person: &person,
                admin,
                account: &their_account,
            },
        );
        let target = Target {
            inviter,
            person,
            admin,
            id,
            secret: Zeroizing::new(session.to_vec()),
            by_code: true,
            inviter_addrs: record.addrs.clone(),
        };
        exchange(
            &connection,
            me,
            target,
            device_name,
            own_addrs,
            wait,
            progress,
        )
        .await
    }
    .await;
    closing(&connection, joined)
}

/// Moves `path` aside (`<path>.old-<millis>`) if it exists.
fn set_aside(path: &Path, now: i64) -> std::io::Result<()> {
    if path.exists() {
        let mut aside = path.as_os_str().to_owned();
        aside.push(format!(".old-{now}"));
        std::fs::rename(path, aside)?;
    }
    Ok(())
}

/// Writes what makes this node a device of the joined person:
/// `device_cert_path` (the certificate in hex) and, for an admin pairing,
/// the person key at `person_key_path`. A person key of another person
/// there (made on the first start of a fresh device) is moved aside.
pub fn install(
    joined: &Joined,
    person_key_path: &Path,
    device_cert_path: &Path,
) -> Result<(), PairError> {
    let now = now_millis();
    let io = |e: std::io::Error| PairError::Other(e.to_string());
    let store = FileKeyStore::new(person_key_path);
    let current = store.load().ok().flatten();
    let same = match (&current, &joined.phrase) {
        (Some(current), Some(phrase)) => current.words() == phrase.words(),
        _ => false,
    };
    // Another person's key (or an unreadable file) goes aside.
    if !same && person_key_path.exists() {
        set_aside(person_key_path, now).map_err(io)?;
    }
    if let Some(phrase) = &joined.phrase {
        store
            .store(phrase)
            .map_err(|e| PairError::Other(e.to_string()))?;
    }
    if let Some(parent) = device_cert_path.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    set_aside(device_cert_path, now).map_err(io)?;
    std::fs::write(
        device_cert_path,
        format!("{}\n", cert_to_hex(&joined.cert.signed)),
    )
    .map_err(io)?;
    Ok(())
}

/// Stores what pairing brought: the person's certificates, revocation
/// list, host lease and co-host grants, and the devices' addresses.
pub async fn remember(joined: &Joined, store: &FederationStore) -> Result<(), PairError> {
    let now = now_millis();
    let fail = |e: &dyn std::fmt::Display| PairError::Other(e.to_string());
    crate::person::save_cert(store, &joined.cert, now)
        .await
        .map_err(|e| fail(&e))?;
    for signed in &joined.result.device_cert {
        if let Ok(cert) = verify_cert(signed)
            && cert.person == joined.cert.person
        {
            crate::person::save_cert(store, &cert, now)
                .await
                .map_err(|e| fail(&e))?;
        }
    }
    if let Some(list) = &joined.result.revocations
        && let Ok(list) = verify_revocations(list)
        && list.person == joined.cert.person
    {
        crate::person::save_revocations(store, &list, now)
            .await
            .map_err(|e| fail(&e))?;
    }
    for lease in &joined.result.host_lease {
        if let Ok(verified) = crate::lease::verify_lease(lease)
            && verified.person == joined.cert.person
        {
            store
                .put_host_lease(
                    verified.person.as_bytes().to_vec(),
                    encode(lease).unwrap_or_default(),
                    now,
                )
                .await
                .map_err(|e| fail(&e))?;
        }
    }
    for grant in &joined.result.co_host {
        if let Ok(verified) = crate::cohost::verify_co_host(grant)
            && verified.person == joined.cert.person
        {
            store
                .put_co_host(resonator_server::storage::CoHostRecord {
                    person: verified.person.as_bytes().to_vec(),
                    delegate_domain: verified.delegate_domain.clone(),
                    issued_at: verified.issued_at,
                    until: verified.until,
                    grant: encode(grant).unwrap_or_default(),
                    learned_at: now,
                })
                .await
                .map_err(|e| fail(&e))?;
        }
    }
    for addr in &joined.inviter_addrs {
        store
            .put_device_addr(joined.inviter.as_bytes().to_vec(), addr.to_string(), now)
            .await
            .map_err(|e| fail(&e))?;
    }
    for line in &joined.result.device_addr {
        let Some((key, addr)) = line.split_once(' ') else {
            continue;
        };
        let (Ok(Some(key)), Ok(addr)) = (
            crate::key::parse_key_domain(key),
            addr.parse::<std::net::SocketAddr>(),
        ) else {
            continue;
        };
        if key != joined.cert.device {
            store
                .put_device_addr(key.as_bytes().to_vec(), addr.to_string(), now)
                .await
                .map_err(|e| fail(&e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invitation() -> PairInvitation {
        let person = iroh::SecretKey::from_bytes(&[1; 32]).public();
        let device = iroh::SecretKey::from_bytes(&[2; 32]).public();
        PairInvitation {
            person: Some(person.as_bytes().to_vec()),
            device: Some(device.as_bytes().to_vec()),
            addr: vec!["127.0.0.1:4000".to_owned()],
            relay: None,
            id: Some("01J".to_owned()),
            secret: Some(vec![7; 16]),
            expires_at: Some(1),
            admin: Some(true),
            account: Some("me".to_owned()),
        }
    }

    #[test]
    fn links_round_trip() {
        let link = encode_link(&invitation());
        assert!(link.starts_with(PAIR_LINK_PREFIX));
        assert_eq!(decode_link(&format!("  {link}\n")).unwrap(), invitation());
        assert!(matches!(
            decode_link("resonator:pair/!!"),
            Err(PairError::BadLink)
        ));
        assert!(matches!(
            decode_link("resonator:contact/a@b"),
            Err(PairError::BadLink)
        ));
        let mut short = invitation();
        short.secret = Some(vec![1; 8]);
        assert!(decode_link(&encode_link(&short)).is_err());
    }

    #[test]
    fn the_mac_binds_both_keys_and_the_id() {
        let a = iroh::SecretKey::from_bytes(&[3; 32]).public();
        let b = iroh::SecretKey::from_bytes(&[4; 32]).public();
        let tag = pair_mac(&[9; 16], &a, &b, "x").finalize().into_bytes();
        assert!(pair_mac(&[9; 16], &a, &b, "x").verify_slice(&tag).is_ok());
        assert!(pair_mac(&[9; 16], &b, &a, "x").verify_slice(&tag).is_err());
        assert!(pair_mac(&[9; 16], &a, &b, "y").verify_slice(&tag).is_err());
        assert!(pair_mac(&[8; 16], &a, &b, "x").verify_slice(&tag).is_err());
    }

    #[test]
    fn sealing_round_trips_and_depends_on_the_secret() {
        let a = iroh::SecretKey::from_bytes(&[3; 32]).public();
        let b = iroh::SecretKey::from_bytes(&[4; 32]).public();
        let phrase = RecoveryPhrase::generate();
        let sealed = xor(&phrase.entropy(), &seal_pad(&[5; 16], &a, &b));
        assert_ne!(*sealed, *phrase.entropy());
        let opened = xor(&sealed, &seal_pad(&[5; 16], &a, &b));
        assert_eq!(
            RecoveryPhrase::from_entropy(&opened).words(),
            phrase.words()
        );
        let wrong = xor(&sealed, &seal_pad(&[6; 16], &a, &b));
        assert_ne!(*wrong, *phrase.entropy());
    }

    #[test]
    fn qr_codes_are_svg() {
        let svg = qr_svg(&encode_link(&invitation()));
        assert!(svg.contains("<svg"));
    }
}
