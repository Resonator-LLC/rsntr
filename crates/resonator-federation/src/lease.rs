//! Person mode: the host lease. One device of a person hosts the person's
//! wavelets at a time: the holder of the person's host lease
//! ([`HostLease`]), a statement signed by the holding device (whose
//! certificate must be valid and not revoked) that it holds lease `term`
//! until `expires_at`.
//!
//! - The holder renews its lease within its term: an anchor device
//!   (`[person] anchor`) every [`Tuning::lease_renew_anchor`], others every
//!   [`Tuning::lease_renew`]; a lease lasts three intervals. Renewals go to
//!   every reachable device of the person (and to every connected peer, so
//!   that other people's servers route to the holder); peers also learn
//!   the lease from each Hello.
//! - A new term starts by handover (the holder signs a [`HostHandover`] to
//!   the next device, which claims the next term with it) or after the
//!   lease expired, by a device that is online: it reaches a relay or at
//!   least one peer. A device that reaches nobody never takes over (unless
//!   it is the only device known of its person). Anchors claim an expired
//!   lease at once, other devices one renewal interval later, and a
//!   holder that is not an anchor hands the lease to a reachable anchor.
//! - Leases are ordered by term, then by device key: of two devices that
//!   claimed the same term apart from each other (split brain), the
//!   greater key wins. Everyone takes the greatest lease they see.
//! - A device that claims a term takes over every wavelet of the person
//!   it has ([`crate::epoch::take_over`], host epoch + 1 per wavelet), so
//!   that a former holder steps down through fencing and rewinds (its
//!   orphaned deltas are resubmitted to the new holder, nothing is lost).
//!
//! The lease decides [`PersonHosting::holder`] of the wave server, and the
//! route to the person domain.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::{PublicKey, SecretKey, Signature, Watcher};
use resonator_model::id::WaveletName;
use resonator_model::proto::federation_rpc::{
    HostHandover, HostLease, Lease, LeaseResult, SignedHostHandover, SignedHostLease,
    federation_request, federation_response,
};
use resonator_model::proto::{decode, encode};
use resonator_server::waveserver::{PersonHosting, WaveletProvider};
use thiserror::Error;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::context::{Context, lock};
use crate::endpoint::{Peer, RequestError};

/// What a host lease is signed over: this, then the statement bytes.
pub const LEASE_SIGNATURE_CONTEXT: &[u8] = b"resonator/host-lease/1\0";
/// What a handover is signed over: this, then the statement bytes.
pub const HANDOVER_SIGNATURE_CONTEXT: &[u8] = b"resonator/host-handover/1\0";

/// Why a lease operation failed.
#[derive(Debug, Error)]
pub enum LeaseError {
    #[error("host lease: {0}")]
    Invalid(String),
    #[error("person mode is off")]
    NotPerson,
    #[error("this device does not hold the host lease")]
    NotHolder,
    #[error(transparent)]
    Request(#[from] RequestError),
}

/// A host lease whose signature (and handover, if any) checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedLease {
    pub person: PublicKey,
    pub domain: String,
    pub term: u64,
    pub device: PublicKey,
    pub issued_at: i64,
    pub expires_at: i64,
    pub prev: Option<PublicKey>,
    /// The device that handed the lease over, if the term began so.
    pub handover_from: Option<PublicKey>,
    pub anchor: bool,
    /// Set when the holder is a co-host of this domain
    /// ([`crate::cohost`]), not a device of the person.
    pub co_host_domain: Option<String>,
    pub signed: SignedHostLease,
}

impl VerifiedLease {
    /// Whether this lease replaces `other`: a later term, the greater
    /// device key in the same term, or a renewal (same term and device,
    /// later expiry).
    pub fn beats(&self, other: &VerifiedLease) -> bool {
        if self.term == other.term && self.device == other.device {
            return self.expires_at > other.expires_at;
        }
        (self.term, self.device.as_bytes()) > (other.term, other.device.as_bytes())
    }

    pub fn expired(&self, now: i64) -> bool {
        now >= self.expires_at
    }
}

fn public_key(bytes: Option<&[u8]>) -> Option<PublicKey> {
    let bytes: &[u8; 32] = bytes?.try_into().ok()?;
    PublicKey::from_bytes(bytes).ok()
}

fn verify_signature(
    key: &PublicKey,
    context: &[u8],
    bytes: &[u8],
    signature: Option<&[u8]>,
) -> bool {
    let Some(signature) = signature.and_then(|s| <&[u8; 64]>::try_from(s).ok()) else {
        return false;
    };
    key.verify(
        &[context, bytes].concat(),
        &Signature::from_bytes(signature),
    )
    .is_ok()
}

/// Signs a lease statement with the holding device's key.
pub fn sign_lease(key: &SecretKey, lease: &HostLease) -> SignedHostLease {
    let bytes = encode(lease).unwrap_or_default();
    let signature = key.sign(&[LEASE_SIGNATURE_CONTEXT, &bytes].concat());
    SignedHostLease {
        lease: Some(bytes),
        signature: Some(signature.to_bytes().to_vec()),
    }
}

/// Signs a handover with the holder's key.
pub fn sign_handover(key: &SecretKey, handover: &HostHandover) -> SignedHostHandover {
    let bytes = encode(handover).unwrap_or_default();
    let signature = key.sign(&[HANDOVER_SIGNATURE_CONTEXT, &bytes].concat());
    SignedHostHandover {
        handover: Some(bytes),
        signature: Some(signature.to_bytes().to_vec()),
    }
}

/// A handover whose signature checked out: (statement, from, to).
pub fn verify_handover(
    signed: &SignedHostHandover,
) -> Result<(HostHandover, PublicKey, PublicKey), LeaseError> {
    let bad = |message: &str| LeaseError::Invalid(format!("handover: {message}"));
    let bytes = signed.handover.as_deref().ok_or_else(|| bad("missing"))?;
    let handover: HostHandover = decode(bytes).map_err(|e| bad(&e.to_string()))?;
    let from = public_key(handover.from.as_deref()).ok_or_else(|| bad("no from key"))?;
    let to = public_key(handover.to.as_deref()).ok_or_else(|| bad("no to key"))?;
    if !verify_signature(
        &from,
        HANDOVER_SIGNATURE_CONTEXT,
        bytes,
        signed.signature.as_deref(),
    ) {
        return Err(bad("signature does not verify"));
    }
    Ok((handover, from, to))
}

/// Checks a signed lease's form and signatures (the device's, and the
/// handover's if the term began with one). Whether the device acts for the
/// person is the caller's check ([`crate::trust::Trust::learn_lease`]).
pub fn verify_lease(signed: &SignedHostLease) -> Result<VerifiedLease, LeaseError> {
    let bad = |message: &str| LeaseError::Invalid(message.to_owned());
    let bytes = signed.lease.as_deref().ok_or_else(|| bad("missing"))?;
    let lease: HostLease = decode(bytes).map_err(|e| bad(&e.to_string()))?;
    let person = public_key(lease.person.as_deref()).ok_or_else(|| bad("no person key"))?;
    let device = public_key(lease.device.as_deref()).ok_or_else(|| bad("no device key"))?;
    if !verify_signature(
        &device,
        LEASE_SIGNATURE_CONTEXT,
        bytes,
        signed.signature.as_deref(),
    ) {
        return Err(bad("signature does not verify"));
    }
    let term = lease.term.unwrap_or(0);
    if term == 0 {
        return Err(bad("term 0"));
    }
    let handover_from = match &lease.handover {
        None => None,
        Some(handover) => {
            let (statement, from, to) = verify_handover(handover)?;
            if to != device
                || statement.term.unwrap_or(0) + 1 != term
                || public_key(statement.person.as_deref()) != Some(person)
            {
                return Err(bad("the handover is not to this device and term"));
            }
            Some(from)
        }
    };
    Ok(VerifiedLease {
        person,
        domain: lease.domain.unwrap_or_default(),
        term,
        device,
        issued_at: lease.issued_at.unwrap_or_default(),
        expires_at: lease.expires_at.unwrap_or_default(),
        prev: public_key(lease.prev.as_deref()),
        handover_from,
        anchor: lease.anchor.unwrap_or(false),
        co_host_domain: lease.co_host_domain.filter(|domain| !domain.is_empty()),
        signed: signed.clone(),
    })
}

/// The host lease of this device's person as this device sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseState {
    /// The holder, if a lease is known.
    pub holder: Option<PublicKey>,
    pub term: u64,
    /// Milliseconds since the epoch.
    pub expires_at: i64,
    /// Whether this device holds the lease (and it has not expired).
    pub held_here: bool,
    /// Whether this device is an anchor.
    pub anchor: bool,
    /// Whether this device reached a relay or a peer at its last look.
    pub online: bool,
}

/// What this node knows of host leases: the newest lease of every person
/// seen, and for its own person what the lease task needs.
#[derive(Debug, Default)]
pub struct Leases {
    anchor: AtomicBool,
    /// This device's person and person domain (person mode).
    own: Mutex<Option<(PublicKey, String)>>,
    book: Mutex<HashMap<[u8; 32], VerifiedLease>>,
    /// Own devices that said they are anchors.
    anchors: Mutex<HashSet<PublicKey>>,
    /// Own devices that answered at the last look.
    reachable: Mutex<HashSet<PublicKey>>,
    online: AtomicBool,
    /// Wakes the lease task: a lease was learned, or a handover came.
    pub(crate) changed: Notify,
    /// A handover to this device, to claim the next term with.
    offered: Mutex<Option<(SignedHostHandover, PublicKey)>>,
    /// The term this device claimed last and when.
    term_started: Mutex<Option<(u64, i64)>>,
    /// Wakes whoever waits for peers: a peer said Hello.
    pub(crate) hello_seen: Notify,
    /// Another device claimed the current term too (split brain).
    contested: AtomicBool,
    /// When devices with a certificate last said Hello (milliseconds).
    last_seen: Mutex<HashMap<PublicKey, i64>>,
    /// A recovery is pulling the person's wavelets back: no claim until
    /// it is done, so that nothing is taken over half pulled.
    held_back: AtomicBool,
}

impl Leases {
    pub fn new(anchor: bool) -> Arc<Self> {
        Arc::new(Leases {
            anchor: AtomicBool::new(anchor),
            ..Leases::default()
        })
    }

    pub fn anchor(&self) -> bool {
        self.anchor.load(Ordering::Relaxed)
    }

    /// Keeps this device from claiming the lease (`true`) or lets it
    /// again.
    pub(crate) fn hold_back(&self, held: bool) {
        self.held_back.store(held, Ordering::Relaxed);
        if !held {
            self.changed.notify_one();
        }
    }

    /// Makes this device an anchor or not, from now on.
    pub fn set_anchor(&self, anchor: bool) {
        self.anchor.store(anchor, Ordering::Relaxed);
        self.changed.notify_one();
    }

    /// Makes `person` (of `domain`) this device's person.
    pub fn set_own(&self, person: PublicKey, domain: String) {
        *lock(&self.own) = Some((person, domain));
    }

    pub fn own(&self) -> Option<(PublicKey, String)> {
        lock(&self.own).clone()
    }

    /// The newest lease known of `person`.
    pub fn current(&self, person: &PublicKey) -> Option<VerifiedLease> {
        lock(&self.book).get(person.as_bytes()).cloned()
    }

    /// The newest lease known of this device's person.
    pub fn own_current(&self) -> Option<VerifiedLease> {
        let (person, _) = self.own()?;
        self.current(&person)
    }

    /// Takes `lease` if it beats the one known of its person; returns
    /// whether it did.
    pub(crate) fn offer(&self, lease: VerifiedLease) -> bool {
        let mut book = lock(&self.book);
        let key = *lease.person.as_bytes();
        if let Some(known) = book.get(&key)
            && !lease.beats(known)
        {
            if known.term == lease.term && known.device != lease.device {
                self.contested.store(true, Ordering::Relaxed);
                self.changed.notify_one();
            }
            return false;
        }
        if lease.anchor {
            lock(&self.anchors).insert(lease.device);
        }
        book.insert(key, lease);
        drop(book);
        self.changed.notify_one();
        true
    }

    /// When this device's current term began, if it holds the lease.
    pub(crate) fn term_start(&self, me: &PublicKey) -> Option<i64> {
        let current = self.own_current().filter(|lease| lease.device == *me)?;
        match *lock(&self.term_started) {
            Some((term, at)) if term == current.term => Some(at),
            // Claimed before a restart: no later than this.
            _ => Some(current.issued_at),
        }
    }

    /// Own devices that said at the last look that they are anchors.
    pub fn known_anchors(&self) -> HashSet<PublicKey> {
        lock(&self.anchors).clone()
    }

    /// Own devices that answered at the last look.
    pub fn reachable(&self) -> HashSet<PublicKey> {
        lock(&self.reachable).clone()
    }

    /// Records that `device` (a device with a certificate) said Hello.
    pub(crate) fn note_device_seen(&self, device: PublicKey, at: i64) {
        let mut seen = lock(&self.last_seen);
        let entry = seen.entry(device).or_insert(at);
        *entry = (*entry).max(at);
    }

    /// When `device` last said Hello here, if it did since the start.
    pub fn device_last_seen(&self, device: &PublicKey) -> Option<i64> {
        lock(&self.last_seen).get(device).copied()
    }

    pub fn is_online(&self) -> bool {
        self.online.load(Ordering::Relaxed)
    }

    /// The lease state of this device's person.
    pub fn state(&self, me: &PublicKey, now: i64) -> LeaseState {
        let current = self.own_current();
        LeaseState {
            holder: current.as_ref().map(|lease| lease.device),
            term: current.as_ref().map_or(0, |lease| lease.term),
            expires_at: current.as_ref().map_or(0, |lease| lease.expires_at),
            held_here: current
                .as_ref()
                .is_some_and(|lease| lease.device == *me && !lease.expired(now)),
            anchor: self.anchor(),
            online: self.is_online(),
        }
    }
}

/// How often a device renews a lease it holds.
fn renew_interval(ctx: &Context, anchor: bool) -> Duration {
    if anchor {
        ctx.tuning.lease_renew_anchor
    } else {
        ctx.tuning.lease_renew
    }
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// The other devices of this device's person.
pub(crate) fn own_devices(ctx: &Context) -> Vec<PublicKey> {
    let me = ctx.own_key();
    ctx.trust
        .devices_of(&ctx.domain)
        .into_iter()
        .filter(|device| *device != me)
        .collect()
}

/// This device's lease statement to peers: the newest lease of its person.
pub(crate) fn hello_leases(ctx_leases: &Leases) -> Vec<SignedHostLease> {
    ctx_leases
        .own_current()
        .map(|lease| vec![lease.signed])
        .unwrap_or_default()
}

/// Answers `lease`: learns the leases sent, takes note of a handover to
/// this device, and answers with the lease known of this device's person.
pub(crate) async fn serve(
    ctx: &Context,
    peer: &Peer,
    request: Lease,
) -> federation_response::Result {
    for signed in &request.host_lease {
        if let Err(error) = ctx.trust.learn_lease(signed).await {
            tracing::info!(peer = %peer.key.fmt_short(), %error, "a peer sent a bad host lease");
        }
    }
    if let Some(handover) = request.handover.filter(|_| ctx.person_mode) {
        match verify_handover(&handover) {
            Ok((statement, from, to))
                if to == ctx.own_key()
                    && from == peer.key
                    && ctx.leases.own_current().is_some_and(|lease| {
                        lease.device == from && lease.term == statement.term.unwrap_or(0)
                    }) =>
            {
                tracing::info!(from = %from.fmt_short(), "the host lease is handed over to this device");
                *lock(&ctx.leases.offered) = Some((handover, from));
                ctx.leases.changed.notify_one();
            }
            Ok(_) => {
                tracing::info!(peer = %peer.key.fmt_short(), "ignored a handover not for this device's lease");
            }
            Err(error) => {
                tracing::info!(peer = %peer.key.fmt_short(), %error, "a peer sent a bad handover");
            }
        }
    }
    let mut host_lease = hello_leases(&ctx.leases);
    // The leases of the asking device's person (a co-host's, say).
    let own = ctx.trust.persons().own();
    for cert in ctx.trust.persons().certs_of(&peer.key) {
        if Some(cert.person) != own
            && let Some(lease) = ctx.leases.current(&cert.person)
        {
            host_lease.push(lease.signed);
        }
    }
    federation_response::Result::Lease(LeaseResult {
        host_lease,
        anchor: Some(ctx.leases.anchor()),
    })
}

/// Sends `lease` to `key` and learns from the answer; whether it answered.
async fn tell(ctx: &Context, key: PublicKey, domain: Option<&str>, request: Lease) -> bool {
    let Ok(endpoint) = ctx.endpoint() else {
        return false;
    };
    // A peer that does not answer must not hold renewals up.
    let patience = (ctx.tuning.lease_renew.min(ctx.tuning.lease_renew_anchor) / 2)
        .min(Duration::from_secs(10));
    let Ok(answer) = tokio::time::timeout(
        patience,
        endpoint.request(key, domain, federation_request::Op::Lease(request)),
    )
    .await
    else {
        tracing::trace!(peer = %key.fmt_short(), "lease not answered in time");
        return false;
    };
    match answer {
        Ok(federation_response::Result::Lease(result)) => {
            for signed in &result.host_lease {
                if let Err(error) = ctx.trust.learn_lease(signed).await {
                    tracing::debug!(%error, "a peer answered with a bad host lease");
                }
            }
            if result.anchor == Some(true) {
                lock(&ctx.leases.anchors).insert(key);
            } else {
                lock(&ctx.leases.anchors).remove(&key);
            }
            true
        }
        Ok(_) => true,
        Err(error) => {
            tracing::trace!(peer = %key.fmt_short(), %error, "lease not delivered");
            false
        }
    }
}

/// Tells every own device and connected peer of this person's lease;
/// notes which own devices answered and whether this device is online.
async fn spread(ctx: &Arc<Context>) {
    let leases = hello_leases(&ctx.leases);
    let own = own_devices(ctx);
    let mut keys: BTreeMap<[u8; 32], (PublicKey, bool)> = own
        .iter()
        .map(|key| (*key.as_bytes(), (*key, true)))
        .collect();
    if let Ok(endpoint) = ctx.endpoint() {
        for key in endpoint.connected_peers() {
            keys.entry(*key.as_bytes()).or_insert((key, false));
        }
    }
    for key in lock(&ctx.static_peers).iter() {
        keys.entry(*key.as_bytes()).or_insert((*key, false));
    }
    let mut asks = tokio::task::JoinSet::new();
    for (key, own_device) in keys.into_values() {
        let ctx = Arc::clone(ctx);
        let request = Lease {
            host_lease: leases.clone(),
            handover: None,
        };
        asks.spawn(async move {
            let domain = own_device.then(|| ctx.domain.clone());
            let answered = tell(&ctx, key, domain.as_deref(), request).await;
            (key, own_device, answered)
        });
    }
    let mut reachable = HashSet::new();
    let mut any = false;
    while let Some(joined) = asks.join_next().await {
        if let Ok((key, own_device, answered)) = joined {
            any |= answered;
            if answered && own_device {
                reachable.insert(key);
            }
        }
    }
    let relay = ctx.endpoint().is_ok_and(|endpoint| {
        endpoint
            .endpoint()
            .home_relay_status()
            .get()
            .iter()
            .any(|status| status.is_connected())
    });
    ctx.leases.online.store(any || relay, Ordering::Relaxed);
    *lock(&ctx.leases.reachable) = reachable;
}

/// Makes the wave server and the routes follow the lease.
fn follow(ctx: &Context, current: Option<&VerifiedLease>, fallback: Option<&PersonHosting>) {
    let me = ctx.own_key();
    let device = me.as_bytes().to_vec();
    let hosting = match current {
        Some(lease) if lease.device == me => PersonHosting {
            device,
            holder: None,
        },
        Some(lease) => {
            ctx.trust.persons().set_route(&ctx.domain, lease.device);
            PersonHosting {
                device,
                holder: Some(lease.device.as_bytes().to_vec()),
            }
        }
        None => fallback
            .cloned()
            .unwrap_or_else(|| PersonHosting::unknown(device)),
    };
    if ctx.server.person_hosting().as_ref() != Some(&hosting) {
        match &hosting.holder {
            None => tracing::info!("this device hosts its person's wavelets"),
            Some(holder) if holder.is_empty() => {}
            Some(_) => tracing::info!(
                holder = %current.map(|lease| lease.device.fmt_short().to_string()).unwrap_or_default(),
                "another device hosts this person's wavelets"
            ),
        }
        ctx.server.set_person_hosting(Some(hosting));
        ctx.pending_wake.notify_one();
    }
}

/// The wavelets of this device's person that it has.
pub(crate) async fn own_wavelets(ctx: &Context) -> Vec<WaveletName> {
    let mut names = Vec::new();
    let Ok(waves) = ctx.server.get_wave_ids().await else {
        return names;
    };
    for wave in waves {
        let Ok(wavelets) = ctx.server.get_wavelet_ids(&wave).await else {
            continue;
        };
        for wavelet in wavelets {
            if wavelet.domain() == ctx.domain {
                names.push(WaveletName::of(wave.clone(), wavelet));
            }
        }
    }
    names
}

/// Takes over the person's wavelets whose newest host epoch is another
/// device's (or that have none) and that began before `since` (the start
/// of this device's term): host epoch + 1 each. With `at_least`, the epoch
/// is above that too (to beat a competing holder's epoch).
pub(crate) async fn take_over_wavelets(ctx: &Context, names: &[WaveletName], since: i64) {
    let me = ctx.own_key();
    for name in names {
        let epochs = ctx.server.epochs(name);
        let latest = epochs.last();
        let mine = latest.is_some_and(|record| record.holder == me.as_bytes());
        if mine {
            continue;
        }
        if let Some(record) = latest
            && epoch_issued_at(record).is_some_and(|issued| issued >= since)
        {
            // Moved to another device on purpose during this term.
            continue;
        }
        let epoch = latest.map_or(0, |record| record.epoch) + 1;
        take_over_at(ctx, name, epoch).await;
    }
}

/// Takes over `name` at `epoch`.
pub(crate) async fn take_over_at(ctx: &Context, name: &WaveletName, epoch: u64) {
    match crate::epoch::take_over(ctx, name, epoch).await {
        Ok(version) => tracing::info!(
            wavelet = %name,
            epoch,
            version = version.version(),
            "took over a wavelet with the host lease"
        ),
        Err(error) => tracing::warn!(wavelet = %name, epoch, %error, "cannot take over a wavelet"),
    }
}

pub(crate) fn epoch_issued_at(record: &resonator_server::waveserver::EpochRecord) -> Option<i64> {
    let signed: resonator_model::proto::federation_rpc::SignedEpochStart =
        decode(&record.statement).ok()?;
    let statement: resonator_model::proto::federation_rpc::EpochStart =
        decode(signed.statement.as_deref()?).ok()?;
    statement.issued_at
}

/// Claims lease `term` for this device (with `handover` from the
/// previous holder, if given), takes the person's wavelets over and tells
/// the other devices.
async fn claim(
    ctx: &Arc<Context>,
    previous: Option<&VerifiedLease>,
    handover: Option<SignedHostHandover>,
) {
    let Some((person, domain)) = ctx.leases.own() else {
        return;
    };
    let now = ctx.now();
    let term = previous.map_or(0, |lease| lease.term) + 1;
    let lease = HostLease {
        person: Some(person.as_bytes().to_vec()),
        domain: Some(domain),
        term: Some(term),
        device: Some(ctx.own_key().as_bytes().to_vec()),
        issued_at: Some(now),
        expires_at: Some(now + 3 * millis(renew_interval(ctx, ctx.leases.anchor()))),
        prev: previous.map(|lease| lease.device.as_bytes().to_vec()),
        handover,
        anchor: Some(ctx.leases.anchor()),
        co_host_domain: None,
    };
    let signed = sign_lease(&ctx.secret, &lease);
    match ctx.trust.learn_lease(&signed).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            tracing::debug!(term, "a newer lease came first; not claiming");
            return;
        }
        Err(error) => {
            tracing::warn!(%error, "cannot claim the host lease");
            return;
        }
    }
    tracing::info!(
        term,
        by_handover = lease.handover.is_some(),
        "claimed the host lease"
    );
    *lock(&ctx.leases.term_started) = Some((term, now));
    follow(ctx, ctx.leases.own_current().as_ref(), None);
    let names = own_wavelets(ctx).await;
    take_over_wavelets(ctx, &names, now).await;
    spread(ctx).await;
}

/// Renews the lease this device holds (same term, later expiry).
async fn renew(ctx: &Arc<Context>, current: &VerifiedLease) {
    let now = ctx.now();
    let lease = HostLease {
        person: Some(current.person.as_bytes().to_vec()),
        domain: Some(current.domain.clone()),
        term: Some(current.term),
        device: Some(ctx.own_key().as_bytes().to_vec()),
        issued_at: Some(now),
        expires_at: Some(now + 3 * millis(renew_interval(ctx, ctx.leases.anchor()))),
        prev: current.prev.map(|key| key.as_bytes().to_vec()),
        handover: decode::<HostLease>(current.signed.lease.as_deref().unwrap_or_default())
            .ok()
            .and_then(|lease| lease.handover),
        anchor: Some(ctx.leases.anchor()),
        co_host_domain: None,
    };
    let signed = sign_lease(&ctx.secret, &lease);
    if let Err(error) = ctx.trust.learn_lease(&signed).await {
        tracing::warn!(%error, "cannot renew the host lease");
    }
}

/// Hands the lease this device holds over to `to` (another device of the
/// person): signs the handover and sends it; `to` claims the next term
/// with it.
pub(crate) async fn hand_over(ctx: &Context, to: PublicKey) -> Result<(), LeaseError> {
    let Some((person, _)) = ctx.leases.own() else {
        return Err(LeaseError::NotPerson);
    };
    let me = ctx.own_key();
    let current = ctx
        .leases
        .own_current()
        .filter(|lease| lease.device == me)
        .ok_or(LeaseError::NotHolder)?;
    let handover = sign_handover(
        &ctx.secret,
        &HostHandover {
            person: Some(person.as_bytes().to_vec()),
            term: Some(current.term),
            from: Some(me.as_bytes().to_vec()),
            to: Some(to.as_bytes().to_vec()),
            issued_at: Some(ctx.now()),
        },
    );
    let endpoint = ctx.endpoint()?;
    let op = federation_request::Op::Lease(Lease {
        host_lease: vec![current.signed.clone()],
        handover: Some(handover),
    });
    if let Err(error) = endpoint.request(to, Some(&ctx.domain), op.clone()).await {
        // Push: a device asleep is woken and tried again for a while.
        let woken = ctx
            .push
            .as_ref()
            .is_some_and(|push| push.request_wake(&to, "lease handover").woken());
        if !woken {
            return Err(error.into());
        }
        let deadline = tokio::time::Instant::now() + ctx.tuning.wake_wait;
        loop {
            tokio::time::sleep(ctx.tuning.wake_retry).await;
            match endpoint.request(to, Some(&ctx.domain), op.clone()).await {
                Ok(_) => break,
                Err(error) if tokio::time::Instant::now() >= deadline => return Err(error.into()),
                Err(_) => {}
            }
        }
    }
    tracing::info!(to = %to.fmt_short(), term = current.term, "handed the host lease over");
    Ok(())
}

/// The lease task of a device: follows, renews, claims and hands over the
/// host lease of its person.
pub(crate) async fn run(
    ctx: Arc<Context>,
    fallback: Option<PersonHosting>,
    cancel: CancellationToken,
) {
    let me = ctx.own_key();
    let started = ctx.now();
    let mut last_renewal = 0i64;
    let mut last_handover = 0i64;
    let tick = (ctx.tuning.lease_renew.min(ctx.tuning.lease_renew_anchor) / 4)
        .max(Duration::from_millis(20));
    follow(&ctx, ctx.leases.own_current().as_ref(), fallback.as_ref());
    loop {
        spread(&ctx).await;
        let now = ctx.now();
        let current = ctx.leases.own_current();
        follow(&ctx, current.as_ref(), fallback.as_ref());
        let offered = lock(&ctx.leases.offered).take();
        match &current {
            Some(lease) if lease.device == me && !ctx.trust.is_revoked(&me) => {
                let renew_every = millis(renew_interval(&ctx, ctx.leases.anchor()));
                if now - last_renewal.max(lease.issued_at) >= renew_every {
                    renew(&ctx, lease).await;
                    last_renewal = now;
                    spread(&ctx).await;
                }
                if ctx.leases.contested.swap(false, Ordering::Relaxed) {
                    // Another device claimed this term as well and took
                    // wavelets over at the same epochs: go above them.
                    tracing::info!(term = lease.term, "another device claimed this term too");
                    for name in own_wavelets(&ctx).await {
                        let epoch = ctx.server.epochs(&name).last().map_or(0, |r| r.epoch) + 1;
                        take_over_at(&ctx, &name, epoch).await;
                    }
                }
                // Prefer an anchor: hand the lease to a reachable one.
                if !ctx.leases.anchor() && now - last_handover >= renew_every {
                    let reachable = lock(&ctx.leases.reachable).clone();
                    let anchor = lock(&ctx.leases.anchors)
                        .iter()
                        .copied()
                        .find(|key| reachable.contains(key) && *key != me);
                    if let Some(anchor) = anchor {
                        last_handover = now;
                        if let Err(error) = hand_over(&ctx, anchor).await {
                            tracing::info!(%error, "cannot hand the host lease to an anchor");
                        }
                    }
                }
            }
            _ => {
                ctx.leases.contested.store(false, Ordering::Relaxed);
                if let Some((handover, from)) = offered {
                    // Catch up with the holder first, so that little of its
                    // history ends up rewound.
                    if let Err(error) = crate::sync::exchange(&ctx, from).await {
                        tracing::info!(%error, "cannot sync with the handing-over device");
                    }
                    claim(&ctx, current.as_ref(), Some(handover)).await;
                } else if may_claim(&ctx, current.as_ref(), now, started) {
                    claim(&ctx, current.as_ref(), None).await;
                }
            }
        }
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(tick) => {}
            () = ctx.leases.changed.notified() => {}
        }
    }
}

/// Whether this device may claim the lease now: the lease (if any) has
/// expired, this device is online (or the only one of its person), and
/// it has waited its turn (anchors at once, others one renewal interval).
fn may_claim(ctx: &Context, current: Option<&VerifiedLease>, now: i64, started: i64) -> bool {
    if ctx.trust.is_revoked(&ctx.own_key()) || ctx.leases.held_back.load(Ordering::Relaxed) {
        return false;
    }
    let alone = own_devices(ctx).is_empty();
    if !ctx.leases.is_online() && !alone {
        return false;
    }
    let grace = if ctx.leases.anchor() || alone {
        0
    } else {
        millis(ctx.tuning.lease_renew)
    };
    match current {
        // A holder its person revoked (lost, or recovered from) holds
        // nothing.
        Some(lease)
            if ctx
                .trust
                .persons()
                .is_revoked_by(&lease.person, &lease.device) =>
        {
            true
        }
        Some(lease) => now >= lease.expires_at + grace,
        // No lease seen yet: anchors claim at once; others wait a lease
        // lifetime for an anchor to show up.
        None => {
            ctx.leases.anchor()
                || alone
                || now >= started + 3 * millis(ctx.tuning.lease_renew) + grace
        }
    }
}
