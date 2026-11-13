//! Waking sleeping devices through a push relay (`[push] relay`, see
//! `docs/push-relay.md`).
//!
//! A device whose app gave it a push token seals the token with its relay
//! (`POST /seal`) and tells its own devices and the hosts of the waves it
//! is in how it is woken: a [`PushTarget`] (device key, relay, sealed
//! token) signed by the device key. A node that cannot reach a device it
//! has something for (an update, a sync, the host lease) asks the device's
//! relay to wake it (`POST /wake`, signed by this node's key), at most once
//! per device per `wake_interval`; the relay sends a silent push with no
//! content, and the woken app reconnects. A relay answering `gone` (the
//! token is dead) makes the node forget the target.
//!
//! Raw tokens stay on the device's own node and the relay: what is sent,
//! stored and logged elsewhere is the sealed token, which only the relay
//! opens. Nothing here runs while `[push] relay` is unset.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use iroh::{PublicKey, SecretKey, Signature};
use prost::Message as _;
use resonator_model::id::WaveletName;
use resonator_model::proto::federation_error::federation_error::Code;
use resonator_model::proto::federation_rpc::{
    PushTarget, PushTargets, SignedPushTarget, federation_request, federation_response,
};
use resonator_server::clock::Clock;
use resonator_server::config::PushConfig;
use resonator_server::storage::{FederationStore, PushTargetRecord};
use resonator_server::waveserver::WaveletProvider;
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use crate::context::{Context, Tuning, error, lock};
use crate::endpoint::Peer;

/// What a push target's signature covers, before the statement bytes.
pub const PUSH_TARGET_CONTEXT: &[u8] = b"resonator/push-target/1\0";
/// What a wake request's signature covers, before the sealed token and
/// the time (`i64`, big-endian seconds).
pub const WAKE_CONTEXT: &[u8] = b"resonator/wake/1\0";

/// Targets with an unanswered wake are woken less and less often, up to
/// this.
const MAX_WAKE_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Hosts and devices told of the targets are told again after this long.
const RETELL: Duration = Duration::from_secs(6 * 60 * 60);
/// Longest relay URL and sealed token accepted from peers.
const MAX_RELAY_LEN: usize = 512;
const MAX_SEALED_LEN: usize = 8 * 1024;

/// A push token as the app got it from its platform. Never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct RawPushToken {
    /// `apns`, `apns-sandbox` or `fcm`.
    pub platform: String,
    pub token: String,
}

impl std::fmt::Debug for RawPushToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawPushToken")
            .field("platform", &self.platform)
            .finish_non_exhaustive()
    }
}

/// What the app said of this device's push token.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenState {
    /// Not told yet: the stored target is kept as it is.
    Unknown,
    None,
    Some(RawPushToken),
}

/// A verified push target.
#[derive(Debug, Clone)]
struct Known {
    relay: String,
    /// Empty: withdrawn.
    sealed: String,
    issued_at: i64,
    signed: SignedPushTarget,
}

#[derive(Debug, Default)]
struct WakeState {
    /// When the last wake was asked for (milliseconds).
    last: Option<i64>,
    /// Wakes since the device was last reached.
    unanswered: u32,
}

#[derive(Default)]
struct State {
    targets: HashMap<PublicKey, Known>,
    /// SHA-256 of the relay and token this device's own target seals.
    own_hash: Option<Vec<u8>>,
    wakes: HashMap<PublicKey, WakeState>,
    /// Device sync: the fingerprint of this device's summary at the last
    /// sync with each device (or the last wake for it).
    synced: HashMap<PublicKey, [u8; 32]>,
    /// Outbox targets told of the targets: the fingerprint told and when.
    told: HashMap<String, ([u8; 32], i64)>,
}

/// The outcome of [`Push::request_wake`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeRequest {
    /// No (callable) target is known for the device.
    NoTarget,
    /// A wake is on its way.
    Sent,
    /// The device was woken recently; `in_window` while it is expected up.
    Recent { in_window: bool },
}

impl WakeRequest {
    /// Whether the device was just woken (worth retrying it soon).
    pub fn woken(self) -> bool {
        matches!(
            self,
            WakeRequest::Sent | WakeRequest::Recent { in_window: true }
        )
    }
}

/// Push on this node: its own target and the targets it learned.
pub struct Push {
    config: PushConfig,
    secret: SecretKey,
    store: FederationStore,
    clock: Arc<dyn Clock>,
    tuning: Tuning,
    http: reqwest::Client,
    state: Mutex<State>,
    token: watch::Sender<TokenState>,
    /// A target of another device of this person was learned.
    learned: Notify,
    wakes_sent: AtomicU64,
}

impl std::fmt::Debug for Push {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Push")
            .field("relay", &self.config.relay)
            .finish_non_exhaustive()
    }
}

impl Push {
    pub(crate) fn new(
        config: PushConfig,
        secret: SecretKey,
        store: FederationStore,
        clock: Arc<dyn Clock>,
        tuning: Tuning,
    ) -> Arc<Push> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        Arc::new(Push {
            config,
            secret,
            store,
            clock,
            tuning,
            http,
            state: Mutex::new(State::default()),
            token: watch::channel(TokenState::Unknown).0,
            learned: Notify::new(),
            wakes_sent: AtomicU64::new(0),
        })
    }

    /// Reads the stored targets.
    pub(crate) async fn load(&self) -> Result<(), resonator_server::storage::StorageError> {
        let own = self.secret.public();
        let records = self.store.push_targets().await?;
        let mut state = lock(&self.state);
        for record in records {
            let Ok(signed) = SignedPushTarget::decode(record.signed.as_slice()) else {
                continue;
            };
            let Ok((device, known)) = verify(&signed) else {
                continue;
            };
            if device == own {
                state.own_hash = record.token_hash.clone();
            }
            state.targets.insert(device, known);
        }
        Ok(())
    }

    fn now(&self) -> i64 {
        self.clock.now_millis()
    }

    /// The relay this device seals its token with.
    pub fn relay(&self) -> &str {
        &self.config.relay
    }

    /// Sets this device's push token (`None`: it has none, and its target
    /// is withdrawn); it is sealed and advertised in the background.
    pub fn set_token(&self, token: Option<RawPushToken>) {
        let state = match token {
            Some(token) => TokenState::Some(token),
            None => TokenState::None,
        };
        self.token.send_if_modified(|current| {
            if *current == state {
                false
            } else {
                *current = state;
                true
            }
        });
    }

    /// The wakes asked of relays since start.
    pub fn wakes_sent(&self) -> u64 {
        self.wakes_sent.load(Ordering::Relaxed)
    }

    /// The devices with a (not withdrawn) target, and their relays.
    pub fn targets(&self) -> Vec<(PublicKey, String)> {
        let mut targets: Vec<_> = lock(&self.state)
            .targets
            .iter()
            .filter(|(_, known)| !known.sealed.is_empty())
            .map(|(device, known)| (*device, known.relay.clone()))
            .collect();
        targets.sort_by_key(|(device, _)| *device.as_bytes());
        targets
    }

    /// Notes that `device` answered: wakes for it are no longer backed
    /// off.
    pub(crate) fn note_reached(&self, device: &PublicKey) {
        if let Some(wake) = lock(&self.state).wakes.get_mut(device) {
            wake.unanswered = 0;
        }
    }

    /// Asks `device`'s relay to wake it, unless it was woken within the
    /// wake interval (longer while earlier wakes went unanswered).
    pub fn request_wake(self: &Arc<Self>, device: &PublicKey, reason: &'static str) -> WakeRequest {
        let now = self.now();
        let mut state = lock(&self.state);
        let Some(known) = state
            .targets
            .get(device)
            .filter(|known| !known.sealed.is_empty() && self.config.may_call(&known.relay))
            .cloned()
        else {
            return WakeRequest::NoTarget;
        };
        let wake = state.wakes.entry(*device).or_default();
        let interval = self
            .tuning
            .wake_interval
            .saturating_mul(1u32 << wake.unanswered.min(10))
            .min(MAX_WAKE_INTERVAL.max(self.tuning.wake_interval));
        if let Some(last) = wake.last {
            let since = now.saturating_sub(last);
            if since < millis(interval) {
                return WakeRequest::Recent {
                    in_window: since < millis(self.tuning.wake_window),
                };
            }
        }
        wake.last = Some(now);
        wake.unanswered = wake.unanswered.saturating_add(1);
        drop(state);
        self.wakes_sent.fetch_add(1, Ordering::Relaxed);
        let push = Arc::clone(self);
        let device = *device;
        tokio::spawn(async move {
            push.send_wake(device, known, reason).await;
        });
        WakeRequest::Sent
    }

    async fn send_wake(&self, device: PublicKey, known: Known, reason: &'static str) {
        let ts = self.now().div_euclid(1000);
        let message = [WAKE_CONTEXT, known.sealed.as_bytes(), &ts.to_be_bytes()].concat();
        let signature = self.secret.sign(&message);
        let body = serde_json::json!({
            "sealed": known.sealed,
            "sender": self.secret.public().to_z32(),
            "ts": ts,
            "sig": URL_SAFE_NO_PAD.encode(signature.to_bytes()),
        });
        let url = format!("{}/wake", known.relay.trim_end_matches('/'));
        let response = self
            .http
            .post(url)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => {
                tracing::debug!(device = %device.fmt_short(), reason, "woke a device");
            }
            Ok(response) if response.status().as_u16() == 410 => {
                tracing::info!(device = %device.fmt_short(), "the push relay says the device's token is gone");
                self.forget(device, known.issued_at).await;
            }
            Ok(response) => {
                tracing::info!(device = %device.fmt_short(), reason, status = response.status().as_u16(), "the push relay did not wake a device");
            }
            Err(error) => {
                tracing::info!(device = %device.fmt_short(), reason, error = %error.without_url(), "cannot reach the push relay");
            }
        }
    }

    /// Forgets the target of `device` issued at `issued_at`.
    async fn forget(&self, device: PublicKey, issued_at: i64) {
        {
            let mut state = lock(&self.state);
            if state
                .targets
                .get(&device)
                .is_some_and(|known| known.issued_at == issued_at)
            {
                state.targets.remove(&device);
                if device == self.secret.public() {
                    state.own_hash = None;
                }
            }
        }
        if let Err(error) = self
            .store
            .delete_push_target(device.as_bytes().to_vec(), issued_at)
            .await
        {
            tracing::warn!(%error, "cannot forget a push target");
        }
    }

    /// Stores a verified target if it is newer than the known one.
    async fn learn(&self, device: PublicKey, known: Known, token_hash: Option<Vec<u8>>) -> bool {
        if lock(&self.state)
            .targets
            .get(&device)
            .is_some_and(|current| current.issued_at >= known.issued_at)
        {
            return false;
        }
        let record = PushTargetRecord {
            device: device.as_bytes().to_vec(),
            issued_at: known.issued_at,
            relay: known.relay.clone(),
            signed: known.signed.encode_to_vec(),
            token_hash: token_hash.clone(),
            learned_at: self.now(),
        };
        match self.store.put_push_target(record).await {
            Ok(true) => {}
            Ok(false) => return false,
            Err(error) => {
                tracing::warn!(%error, "cannot store a push target");
                return false;
            }
        }
        let mut state = lock(&self.state);
        if device == self.secret.public() {
            state.own_hash = token_hash;
        }
        state.targets.insert(device, known);
        true
    }

    /// Signs and stores a new target of this device.
    async fn issue(&self, sealed: String, token_hash: Option<Vec<u8>>) {
        let own = self.secret.public();
        let previous = lock(&self.state)
            .targets
            .get(&own)
            .map_or(0, |known| known.issued_at);
        let target = PushTarget {
            device: Some(own.as_bytes().to_vec()),
            relay: Some(self.config.relay.clone()),
            sealed: Some(sealed.clone()),
            issued_at: Some(self.now().max(previous + 1)),
        };
        let signed = sign(&self.secret, &target);
        let known = Known {
            relay: self.config.relay.clone(),
            sealed,
            issued_at: target.issued_at.unwrap_or_default(),
            signed,
        };
        self.learn(own, known, token_hash).await;
    }

    /// Brings this device's own target in line with its token: seals a
    /// new token (or the token with a new relay), withdraws the target of
    /// a token that is gone. Errs if the relay could not be reached.
    async fn ensure_own(&self, wanted: &TokenState) -> Result<(), ()> {
        let own = self.secret.public();
        let (current, own_hash) = {
            let state = lock(&self.state);
            (state.targets.get(&own).cloned(), state.own_hash.clone())
        };
        match wanted {
            TokenState::Unknown => Ok(()),
            TokenState::None => {
                if current.is_some_and(|known| !known.sealed.is_empty()) {
                    tracing::info!("withdrawing this device's push target");
                    self.issue(String::new(), None).await;
                }
                Ok(())
            }
            TokenState::Some(token) => {
                let hash = token_hash(&self.config.relay, token);
                let fresh = current.is_some_and(|known| {
                    !known.sealed.is_empty() && known.relay == self.config.relay
                }) && own_hash.as_deref() == Some(hash.as_slice());
                if fresh {
                    return Ok(());
                }
                let sealed = self.seal(token).await?;
                tracing::info!(platform = %token.platform, "sealed this device's push token");
                self.issue(sealed, Some(hash)).await;
                Ok(())
            }
        }
    }

    async fn seal(&self, token: &RawPushToken) -> Result<String, ()> {
        let url = format!("{}/seal", self.config.relay.trim_end_matches('/'));
        let body = serde_json::json!({"platform": token.platform, "token": token.token});
        let response = self
            .http
            .post(url)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|error| {
                tracing::info!(error = %error.without_url(), "cannot reach the push relay to seal the push token");
            })?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            tracing::warn!(
                status = status.as_u16(),
                "the push relay refused to seal the push token"
            );
            return Err(());
        }
        let sealed = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| value["sealed"].as_str().map(str::to_owned))
            .filter(|sealed| !sealed.is_empty() && sealed.len() <= MAX_SEALED_LEN);
        sealed.ok_or_else(|| tracing::warn!("the push relay answered no sealed token"))
    }

    /// The targets this node tells: its own and those of the other
    /// devices of its person.
    fn adverts(&self, ctx: &Context) -> Vec<SignedPushTarget> {
        let own = self.secret.public();
        let mine: BTreeSet<[u8; 32]> = std::iter::once(own)
            .chain(if ctx.person_mode {
                ctx.trust.devices_of(&ctx.domain)
            } else {
                Vec::new()
            })
            .map(|key| *key.as_bytes())
            .collect();
        let state = lock(&self.state);
        let mut adverts: BTreeMap<[u8; 32], SignedPushTarget> = BTreeMap::new();
        for (device, known) in &state.targets {
            if mine.contains(device.as_bytes()) {
                adverts.insert(*device.as_bytes(), known.signed.clone());
            }
        }
        adverts.into_values().collect()
    }

    /// Tells the other devices of this person and the hosts of the waves
    /// this node is in of the targets ([`Push::adverts`]): those not told
    /// these yet, or not for six hours.
    async fn advertise(&self, ctx: &Context) {
        let adverts = self.adverts(ctx);
        if adverts.is_empty() {
            return;
        }
        let mut hasher = Sha256::new();
        for advert in &adverts {
            hasher.update(advert.encode_to_vec());
        }
        let fingerprint: [u8; 32] = hasher.finalize().into();
        let mut recipients: BTreeSet<String> = BTreeSet::new();
        if ctx.person_mode {
            recipients.extend(ctx.targets(&ctx.domain));
        }
        for domain in host_domains(ctx).await {
            recipients.extend(ctx.targets(&domain));
        }
        let own = format!("/{}", self.secret.public().to_z32());
        let now = self.now();
        for recipient in recipients {
            if recipient.ends_with(&own) {
                continue;
            }
            let due = lock(&self.state)
                .told
                .get(&recipient)
                .is_none_or(|(told, at)| *told != fingerprint || now - at > millis(RETELL));
            if !due {
                continue;
            }
            let op = federation_request::Op::PushTargets(PushTargets {
                push_target: adverts.clone(),
            });
            match ctx.request_target(&recipient, None, op).await {
                Ok(answer) => {
                    if let federation_response::Result::Error(error) = &answer {
                        tracing::debug!(%recipient, message = %error.message(), "a peer did not take push targets");
                    }
                    lock(&self.state).told.insert(recipient, (fingerprint, now));
                }
                Err(error) => {
                    tracing::trace!(%recipient, %error, "push targets not delivered");
                }
            }
        }
    }

    /// Device sync with `device` worked, with this device's summary at
    /// `fingerprint`.
    pub(crate) fn synced(&self, device: &PublicKey, fingerprint: [u8; 32]) {
        let mut state = lock(&self.state);
        state.synced.insert(*device, fingerprint);
        if let Some(wake) = state.wakes.get_mut(device) {
            wake.unanswered = 0;
        }
    }

    /// Device sync with `device` failed: it is woken if this device has
    /// changed since they last synced.
    pub(crate) fn sync_failed(self: &Arc<Self>, device: &PublicKey, fingerprint: [u8; 32]) {
        let changed = {
            let mut state = lock(&self.state);
            match state.synced.get(device) {
                // Nothing to compare with yet (since start): the device
                // learns of what changes from now on.
                None => {
                    state.synced.insert(*device, fingerprint);
                    false
                }
                Some(previous) => *previous != fingerprint,
            }
        };
        if changed && self.request_wake(device, "device sync") == WakeRequest::Sent {
            lock(&self.state).synced.insert(*device, fingerprint);
        }
    }
}

/// The SHA-256 of a relay and token: whether the sealed one is current.
fn token_hash(relay: &str, token: &RawPushToken) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(relay.trim_end_matches('/').as_bytes());
    hasher.update([0]);
    hasher.update(token.platform.as_bytes());
    hasher.update([0]);
    hasher.update(token.token.as_bytes());
    hasher.finalize().to_vec()
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// Signs a push target with a device key.
pub fn sign(key: &SecretKey, target: &PushTarget) -> SignedPushTarget {
    let bytes = target.encode_to_vec();
    let signature = key.sign(&[PUSH_TARGET_CONTEXT, &bytes].concat());
    SignedPushTarget {
        target: Some(bytes),
        signature: Some(signature.to_bytes().to_vec()),
    }
}

/// A push target whose signature checks out, with its device.
fn verify(signed: &SignedPushTarget) -> Result<(PublicKey, Known), String> {
    let bytes = signed.target.as_deref().ok_or("no target")?;
    let target = PushTarget::decode(bytes).map_err(|e| e.to_string())?;
    let device = target
        .device
        .as_deref()
        .and_then(|bytes| <&[u8; 32]>::try_from(bytes).ok())
        .and_then(|bytes| PublicKey::from_bytes(bytes).ok())
        .ok_or("bad device key")?;
    let signature = signed
        .signature
        .as_deref()
        .and_then(|bytes| <&[u8; 64]>::try_from(bytes).ok())
        .ok_or("bad signature")?;
    device
        .verify(
            &[PUSH_TARGET_CONTEXT, bytes].concat(),
            &Signature::from_bytes(signature),
        )
        .map_err(|_| "bad signature")?;
    let relay = target.relay.unwrap_or_default();
    let sealed = target.sealed.unwrap_or_default();
    let url = relay
        .strip_prefix("https://")
        .or_else(|| relay.strip_prefix("http://"))
        .is_some_and(|rest| !rest.is_empty());
    if !url || relay.len() > MAX_RELAY_LEN || sealed.len() > MAX_SEALED_LEN {
        return Err("bad relay or sealed token".to_owned());
    }
    Ok((
        device,
        Known {
            relay,
            sealed,
            issued_at: target.issued_at.ok_or("no issue time")?,
            signed: signed.clone(),
        },
    ))
}

/// The domains hosting wavelets this node has (other than its own).
async fn host_domains(ctx: &Context) -> BTreeSet<String> {
    let mut domains = BTreeSet::new();
    let Ok(waves) = ctx.server.get_wave_ids().await else {
        return domains;
    };
    for wave in waves {
        let Ok(wavelets) = ctx.server.get_wavelet_ids(&wave).await else {
            continue;
        };
        for wavelet in wavelets {
            let host = ctx
                .server
                .host_domain(&WaveletName::of(wave.clone(), wavelet));
            if host != ctx.domain {
                domains.insert(host);
            }
        }
    }
    domains
}

/// Answers `push-targets`: keeps the newer targets of the peer and of
/// other devices of its person.
pub(crate) async fn serve(
    ctx: &Context,
    push: &Push,
    peer: &Peer,
    request: PushTargets,
) -> federation_response::Result {
    if request.push_target.len() > 64 {
        return error(Code::ResourceConstraint, "too many push targets");
    }
    let own = ctx.own_key();
    let mut learned = false;
    for signed in &request.push_target {
        let (device, known) = match verify(signed) {
            Ok(verified) => verified,
            Err(message) => {
                tracing::info!(peer = %peer.key.fmt_short(), %message, "a peer sent a bad push target");
                continue;
            }
        };
        let same_person = || {
            peer.domains.iter().any(|domain| {
                let devices = ctx.trust.devices_of(domain);
                devices.contains(&peer.key) && devices.contains(&device)
            })
        };
        if device == own || !(device == peer.key || same_person()) {
            continue;
        }
        if push.learn(device, known, None).await {
            tracing::debug!(device = %device.fmt_short(), "learned a push target");
            learned = true;
        }
    }
    if learned {
        push.learned.notify_one();
    }
    federation_response::Result::Ack(Default::default())
}

/// The fingerprint of a device sync summary.
pub(crate) fn summary_fingerprint(
    entries: &[resonator_model::proto::federation_rpc::SyncEntry],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for entry in entries {
        hasher.update(entry.encode_to_vec());
    }
    hasher.finalize().into()
}

/// The push task: keeps this device's target in line with its token and
/// tells devices and hosts of the targets.
pub(crate) async fn run(ctx: Arc<Context>, push: Arc<Push>, cancel: CancellationToken) {
    let mut token = push.token.subscribe();
    let mut seal_retry = Duration::from_secs(5);
    loop {
        let wanted = token.borrow_and_update().clone();
        let mut wait = ctx.tuning.push_advertise;
        match push.ensure_own(&wanted).await {
            Ok(()) => seal_retry = Duration::from_secs(5),
            Err(()) => {
                wait = wait.min(seal_retry);
                seal_retry = (seal_retry * 2).min(Duration::from_secs(600));
            }
        }
        push.advertise(&ctx).await;
        tokio::select! {
            () = cancel.cancelled() => return,
            changed = token.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            () = push.learned.notified() => {}
            () = tokio::time::sleep(wait) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_verify_and_tampering_shows() {
        let key = SecretKey::generate();
        let target = PushTarget {
            device: Some(key.public().as_bytes().to_vec()),
            relay: Some("http://mothership:9900".to_owned()),
            sealed: Some("c2VhbGVk".to_owned()),
            issued_at: Some(7),
        };
        let signed = sign(&key, &target);
        let (device, known) = verify(&signed).unwrap();
        assert_eq!(device, key.public());
        assert_eq!((known.issued_at, known.sealed.as_str()), (7, "c2VhbGVk"));
        let mut bad = signed.clone();
        bad.target.as_mut().unwrap()[3] ^= 1;
        assert!(verify(&bad).is_err());
        let other = SecretKey::generate();
        let forged = sign(&other, &target);
        assert!(verify(&forged).is_err());
        let mut not_url = target.clone();
        not_url.relay = Some("file:///etc".to_owned());
        assert!(verify(&sign(&key, &not_url)).is_err());
    }

    #[test]
    fn a_token_is_resealed_for_another_relay_or_token() {
        let token = RawPushToken {
            platform: "apns".to_owned(),
            token: "ab".repeat(32),
        };
        let hash = token_hash("http://r", &token);
        assert_eq!(hash, token_hash("http://r/", &token));
        assert_ne!(hash, token_hash("http://s", &token));
        let other = RawPushToken {
            token: "cd".repeat(32),
            ..token.clone()
        };
        assert_ne!(hash, token_hash("http://r", &other));
        assert!(!format!("{token:?}").contains("abab"));
    }
}
