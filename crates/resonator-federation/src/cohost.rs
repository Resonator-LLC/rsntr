//! Co-hosts: a person lets the devices of another domain (a friend's
//! node, an always-on server) hold the person's host lease while none of
//! the person's own devices is around.
//!
//! A grant ([`CoHost`]: person, delegate domain, until) is signed by the
//! person key and spread like certificates: in every Hello, by the
//! `co-hosts` push and in `person-info` answers. The newest grant of a
//! person for a domain replaces the older ones, so a grant with `until`
//! 0 withdraws it.
//!
//! The co-host task of a node whose domain is a delegate
//! ([`run`]) claims the person's lease only when the lease it knows has
//! expired and the node is online (it reaches a relay or a peer), with
//! [`HostLease::co_host_domain`] set to its domain. It then takes over
//! the person's wavelets it has (host epoch + 1, holder domain its own,
//! allowed by [`CoHostAware`]) and renews the lease only as long as no
//! device of the person has been seen since the claim: once one is back,
//! the lease runs out and that device claims the next term and takes the
//! wavelets back.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use iroh::{PublicKey, Signature};
use resonator_model::id::WaveletName;
use resonator_model::proto::decode;
use resonator_model::proto::federation_rpc::{
    CoHost, CoHosts, EpochStart, HostLease, Lease, SignedCoHost, federation_request,
};
use tokio_util::sync::CancellationToken;

use resonator_server::waveserver::WaveletProvider;

use crate::context::{Context, lock};
use crate::epoch::EpochAuthority;
use crate::key::parse_key_domain;
use crate::lease::{VerifiedLease, sign_lease};
use crate::person::PersonBook;

/// What a co-host grant is signed over: this, then the statement bytes.
pub const CO_HOST_CONTEXT: &[u8] = b"resonator/co-host/1\0";

/// A co-host grant whose signature checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCoHost {
    pub person: PublicKey,
    pub delegate_domain: String,
    pub until: i64,
    pub issued_at: i64,
    pub signed: SignedCoHost,
}

impl VerifiedCoHost {
    /// Whether the grant lets its delegate host at `at`.
    pub fn active(&self, at: i64) -> bool {
        at < self.until
    }
}

/// Checks a grant's form and its signature by the person key it names.
pub fn verify_co_host(signed: &SignedCoHost) -> Result<VerifiedCoHost, String> {
    let bytes = signed.grant.as_deref().ok_or("co-host grant: missing")?;
    let grant: CoHost = decode(bytes).map_err(|e| format!("co-host grant: {e}"))?;
    let person = grant
        .person
        .as_deref()
        .and_then(|bytes| <&[u8; 32]>::try_from(bytes).ok())
        .and_then(|bytes| PublicKey::from_bytes(bytes).ok())
        .ok_or("co-host grant: no person key")?;
    let signature = signed
        .signature
        .as_deref()
        .and_then(|bytes| <&[u8; 64]>::try_from(bytes).ok())
        .ok_or("co-host grant: no signature")?;
    person
        .verify(
            &[CO_HOST_CONTEXT, bytes].concat(),
            &Signature::from_bytes(signature),
        )
        .map_err(|_| "co-host grant: the signature does not verify".to_owned())?;
    let delegate_domain = grant.delegate_domain.unwrap_or_default();
    if delegate_domain.is_empty() {
        return Err("co-host grant: no delegate domain".to_owned());
    }
    Ok(VerifiedCoHost {
        person,
        delegate_domain,
        until: grant.until.unwrap_or(0),
        issued_at: grant.issued_at.unwrap_or(0),
        signed: signed.clone(),
    })
}

/// The co-host grants a node knows (the newest of each person for each
/// delegate domain), and when devices of each person were last seen.
#[derive(Debug, Default)]
pub struct CoHostBook {
    grants: RwLock<HashMap<([u8; 32], String), VerifiedCoHost>>,
    seen: std::sync::Mutex<HashMap<[u8; 32], i64>>,
}

impl CoHostBook {
    pub fn new() -> Arc<Self> {
        Arc::new(CoHostBook::default())
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<([u8; 32], String), VerifiedCoHost>> {
        self.grants
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Keeps `grant` if it is newer than the one known; returns whether
    /// it did.
    pub fn offer(&self, grant: VerifiedCoHost) -> bool {
        let mut grants = self
            .grants
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = (*grant.person.as_bytes(), grant.delegate_domain.clone());
        if grants
            .get(&key)
            .is_some_and(|known| known.issued_at >= grant.issued_at)
        {
            return false;
        }
        grants.insert(key, grant);
        true
    }

    /// The grant of `person` for `domain`, if any (active or not).
    pub fn grant(&self, person: &PublicKey, domain: &str) -> Option<VerifiedCoHost> {
        self.read()
            .get(&(*person.as_bytes(), domain.to_owned()))
            .cloned()
    }

    /// Whether `person` lets `domain` host at `at`.
    pub fn active(&self, person: &PublicKey, domain: &str, at: i64) -> bool {
        self.grant(person, domain)
            .is_some_and(|grant| grant.active(at))
    }

    /// Every grant known.
    pub fn all(&self) -> Vec<VerifiedCoHost> {
        let mut all: Vec<_> = self.read().values().cloned().collect();
        all.sort_by(|a, b| {
            (a.person.as_bytes(), &a.delegate_domain)
                .cmp(&(b.person.as_bytes(), &b.delegate_domain))
        });
        all
    }

    /// The grants of `person`.
    pub fn of_person(&self, person: &PublicKey) -> Vec<VerifiedCoHost> {
        self.all()
            .into_iter()
            .filter(|grant| grant.person == *person)
            .collect()
    }

    /// The grants naming `domain`.
    pub fn for_delegate(&self, domain: &str) -> Vec<VerifiedCoHost> {
        self.all()
            .into_iter()
            .filter(|grant| grant.delegate_domain == domain)
            .collect()
    }

    /// A device of `person` said Hello at `at`.
    pub fn note_seen(&self, person: &PublicKey, at: i64) {
        lock(&self.seen).insert(*person.as_bytes(), at);
    }

    /// When a device of `person` last said Hello.
    pub fn last_seen(&self, person: &PublicKey) -> Option<i64> {
        lock(&self.seen).get(person.as_bytes()).copied()
    }
}

/// An [`EpochAuthority`] that also lets a person's co-hosts hold the
/// person's wavelets (while their grant is active at the statement's
/// issue time). Without grants it is the authority it wraps.
pub struct CoHostAware {
    pub inner: Arc<dyn EpochAuthority>,
    pub grants: Arc<CoHostBook>,
    pub persons: Arc<PersonBook>,
}

impl EpochAuthority for CoHostAware {
    fn may_host(&self, wavelet: &WaveletName, holder_domain: &str, statement: &EpochStart) -> bool {
        if self.inner.may_host(wavelet, holder_domain, statement) {
            return true;
        }
        let domain = wavelet.wavelet_id.domain();
        let at = statement.issued_at.unwrap_or(0);
        persons_of(&self.persons, domain)
            .iter()
            .any(|person| self.grants.active(person, holder_domain, at))
    }
}

/// The persons that may speak for `domain`: the key of a key domain, or
/// the persons its DNS record lists.
pub(crate) fn persons_of(persons: &PersonBook, domain: &str) -> Vec<PublicKey> {
    match parse_key_domain(domain) {
        Ok(Some(key)) => vec![key],
        _ => persons.persons_of(domain, None),
    }
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// Answers `co-hosts`: learns the grants sent.
pub(crate) async fn serve(ctx: &Context, request: CoHosts) {
    for grant in &request.co_host {
        if let Err(error) = ctx.trust.learn_co_host(grant).await {
            tracing::info!(%error, "a peer sent a bad co-host grant");
        }
    }
}

/// Tells `keys` of the grants of this node's person (best effort).
pub(crate) async fn push(ctx: &Context, keys: &[PublicKey]) {
    let Some(person) = ctx.trust.persons().own() else {
        return;
    };
    let grants: Vec<SignedCoHost> = ctx
        .trust
        .co_hosts()
        .of_person(&person)
        .into_iter()
        .map(|grant| grant.signed)
        .collect();
    if grants.is_empty() {
        return;
    }
    let Ok(endpoint) = ctx.endpoint() else {
        return;
    };
    for key in keys {
        let request = federation_request::Op::CoHosts(CoHosts {
            co_host: grants.clone(),
        });
        if let Err(error) = endpoint.request(*key, None, request).await {
            tracing::debug!(peer = %key.fmt_short(), %error, "co-host grants not delivered");
        }
    }
}

/// Whether this node reaches a relay or a peer now.
async fn online(ctx: &Context) -> bool {
    let Ok(endpoint) = ctx.endpoint() else {
        return false;
    };
    use iroh::Watcher;
    if endpoint
        .endpoint()
        .home_relay_status()
        .get()
        .iter()
        .any(|status| status.is_connected())
    {
        return true;
    }
    let mut keys = endpoint.connected_peers();
    keys.extend(lock(&ctx.static_peers).iter().copied());
    for key in keys {
        let echo = federation_request::Op::Echo(Default::default());
        if let Ok(Ok(_)) =
            tokio::time::timeout(Duration::from_secs(2), endpoint.request(key, None, echo)).await
        {
            return true;
        }
    }
    false
}

/// The wavelets of `domain` this node has.
async fn wavelets_of(ctx: &Context, domain: &str) -> Vec<WaveletName> {
    let mut names = Vec::new();
    let Ok(waves) = ctx.server.get_wave_ids().await else {
        return names;
    };
    for wave in waves {
        let Ok(wavelets) = ctx.server.get_wavelet_ids(&wave).await else {
            continue;
        };
        for wavelet in wavelets {
            if wavelet.domain() == domain {
                names.push(WaveletName::of(wave.clone(), wavelet));
            }
        }
    }
    names
}

/// Sends `lease` to the devices of its person and to connected peers.
async fn spread(ctx: &Context, lease: &VerifiedLease) {
    let Ok(endpoint) = ctx.endpoint() else {
        return;
    };
    let mut keys = ctx.trust.devices_of(&lease.domain);
    for key in endpoint.connected_peers() {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    let me = ctx.own_key();
    keys.retain(|key| *key != me);
    for key in keys {
        let request = federation_request::Op::Lease(Lease {
            host_lease: vec![lease.signed.clone()],
            handover: None,
        });
        let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.request(key, None, request))
            .await;
    }
}

/// Signs a lease of `person` (of `domain`) for this node as co-host.
fn co_host_lease(
    ctx: &Context,
    person: &PublicKey,
    domain: &str,
    term: u64,
    prev: Option<PublicKey>,
) -> HostLease {
    let now = ctx.now();
    HostLease {
        person: Some(person.as_bytes().to_vec()),
        domain: Some(domain.to_owned()),
        term: Some(term),
        device: Some(ctx.own_key().as_bytes().to_vec()),
        issued_at: Some(now),
        expires_at: Some(now + 3 * millis(ctx.tuning.lease_renew)),
        prev: prev.map(|key| key.as_bytes().to_vec()),
        handover: None,
        anchor: Some(false),
        co_host_domain: Some(ctx.domain.clone()),
    }
}

/// The co-host task: for every active grant naming this node's domain,
/// claims the person's lease once it has expired (while online), renews
/// it while no device of the person is back, and lets it run out then.
pub(crate) async fn run(ctx: Arc<Context>, cancel: CancellationToken) {
    let tick = (ctx.tuning.lease_renew / 4).max(Duration::from_millis(20));
    // When this node claimed each person's lease.
    let mut claimed: HashMap<[u8; 32], i64> = HashMap::new();
    let mut renewed: HashMap<[u8; 32], i64> = HashMap::new();
    let mut last_load = ctx.now();
    loop {
        let me = ctx.own_key();
        let now = ctx.now();
        let own_person = ctx.trust.persons().own();
        for grant in ctx.trust.co_hosts().for_delegate(&ctx.domain) {
            if Some(grant.person) == own_person {
                continue;
            }
            let person = grant.person;
            let Some(current) = ctx.leases.current(&person) else {
                // Only a lease seen to expire is taken over.
                continue;
            };
            let mine =
                current.device == me && current.co_host_domain.as_deref() == Some(&ctx.domain);
            if mine {
                let since = claimed
                    .get(person.as_bytes())
                    .copied()
                    .unwrap_or(current.issued_at);
                let person_back = ctx
                    .trust
                    .co_hosts()
                    .last_seen(&person)
                    .is_some_and(|seen| seen > since);
                if person_back || !grant.active(now) || current.expired(now) {
                    // Let it run out: a device of the person takes over.
                    continue;
                }
                let last = renewed
                    .get(person.as_bytes())
                    .copied()
                    .unwrap_or(current.issued_at);
                if now - last >= millis(ctx.tuning.lease_renew) {
                    let lease =
                        co_host_lease(&ctx, &person, &current.domain, current.term, current.prev);
                    let signed = sign_lease(&ctx.secret, &lease);
                    match ctx.trust.learn_lease(&signed).await {
                        Ok(Some(lease)) => {
                            renewed.insert(*person.as_bytes(), now);
                            spread(&ctx, &lease).await;
                        }
                        Ok(None) => {}
                        Err(error) => tracing::info!(%error, "cannot renew a co-host lease"),
                    }
                }
                continue;
            }
            let grace = millis(ctx.tuning.lease_renew);
            if !grant.active(now) || now < current.expires_at + grace {
                continue;
            }
            // A device of the person said Hello since the lease ran out:
            // it is about to claim.
            if ctx
                .trust
                .co_hosts()
                .last_seen(&person)
                .is_some_and(|seen| seen >= current.expires_at)
            {
                continue;
            }
            if !online(&ctx).await {
                continue;
            }
            let lease = co_host_lease(
                &ctx,
                &person,
                &current.domain,
                current.term + 1,
                Some(current.device),
            );
            let signed = sign_lease(&ctx.secret, &lease);
            let lease = match ctx.trust.learn_lease(&signed).await {
                Ok(Some(lease)) => lease,
                Ok(None) => continue,
                Err(error) => {
                    tracing::info!(%error, "cannot claim a co-host lease");
                    continue;
                }
            };
            tracing::info!(
                person = %person.fmt_short(),
                term = lease.term,
                "claimed a person's host lease as co-host"
            );
            claimed.insert(*person.as_bytes(), now);
            renewed.insert(*person.as_bytes(), now);
            for name in wavelets_of(&ctx, &lease.domain).await {
                let epoch = ctx.server.epochs(&name).last().map_or(0, |r| r.epoch) + 1;
                crate::lease::take_over_at(&ctx, &name, epoch).await;
            }
            spread(&ctx, &lease).await;
        }
        // Grants stored by the command line: learn them and tell the
        // peers.
        if now - last_load >= RELOAD_EVERY_MS {
            last_load = now;
            if let Ok(learned) = ctx.trust.load_co_hosts().await
                && learned.iter().any(|grant| Some(grant.person) == own_person)
            {
                let mut keys = ctx
                    .endpoint()
                    .map(|e| e.connected_peers())
                    .unwrap_or_default();
                keys.extend(crate::lease::own_devices(&ctx));
                for grant in &learned {
                    if let Ok(key) = ctx.key_of(&grant.delegate_domain).await {
                        keys.push(key);
                    }
                }
                keys.sort();
                keys.dedup();
                push(&ctx, &keys).await;
            }
        }
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(tick) => {}
            () = ctx.leases.changed.notified() => {}
        }
    }
}

/// How often the co-host task reads the stored grants again.
const RELOAD_EVERY_MS: i64 = 5_000;

/// Checks a grant and stores it (the command line; a running node reads
/// it within a few seconds and tells its peers).
pub async fn store_co_host(
    store: &resonator_server::storage::FederationStore,
    signed: &SignedCoHost,
    now: i64,
) -> Result<VerifiedCoHost, String> {
    let grant = verify_co_host(signed)?;
    store
        .put_co_host(resonator_server::storage::CoHostRecord {
            person: grant.person.as_bytes().to_vec(),
            delegate_domain: grant.delegate_domain.clone(),
            issued_at: grant.issued_at,
            until: grant.until,
            grant: resonator_model::proto::encode(signed).map_err(|e| e.to_string())?,
            learned_at: now,
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(grant)
}
