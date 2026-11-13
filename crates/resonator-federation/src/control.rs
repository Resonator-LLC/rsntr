//! Person mode's operations for people: pairing, the device list,
//! revoking, co-hosts, recovery and whether a wave's host is frozen.
//! [`PersonOps`] does them for the [`crate::Federation`] methods, the
//! CLI and the FFI; [`HttpControl`] answers the settings UI
//! (`/person/...`, [`resonator_server::http::PersonControl`]).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use iroh::{EndpointAddr, PublicKey};
use resonator_model::id::WaveletName;
use resonator_server::config::Config;
use resonator_server::http::{AppState, ControlError, HostStatus, PersonControl};
use resonator_server::storage::FederationStore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cohost::persons_of;
use crate::context::Context;
use crate::endpoint::FederationEndpoint;
use crate::key::{key_domain, parse_key_domain};
use crate::pair_code::{PairCode, Rendezvous};
use crate::pairing::{
    Inviter, InviterHost, JoinStage, Joined, NewInvitation, PairError, PendingPairing,
    PersonBundle, Progress,
};
use crate::person::{FileKeyStore, KeyStore, PersonKey, RecoveryPhrase, VerifiedCert};

/// The file that asks the next start to finish a recovery (see
/// [`prepare_recovery`]).
pub const RECOVERY_MARKER: &str = "recovery.json";
/// The file that keeps the anchor setting made in the settings.
/// How long a recovery that no peer answers keeps the lease unclaimed.
const RECOVERY_HOLD: Duration = Duration::from_secs(120);

pub const ANCHOR_FILE: &str = "person-anchor";
/// How long a joining device waits for an admin pairing to be
/// confirmed on the inviting device.
pub const CONFIRM_WAIT: Duration = Duration::from_secs(180);

/// What [`prepare_recovery`] leaves for the next start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryMarker {
    /// Revoke every other device of the person.
    #[serde(default)]
    pub revoke_old: bool,
    /// Peers to ask besides the configured ones: `<z32 key>` or
    /// `<z32 key>@<host:port>`.
    #[serde(default)]
    pub peers: Vec<String>,
}

/// Makes the node of `config` the person of `phrase` at its next start:
/// stores the person key (an earlier key and device certificate are
/// moved aside), and leaves a marker that has the next start issue this
/// device a certificate, publish a revocation list, pull the person's
/// waves from the peers and take the lease ([`crate::recovery`]).
pub fn prepare_recovery(
    config: &Config,
    phrase: &RecoveryPhrase,
    revoke_old: bool,
    peers: Vec<String>,
) -> Result<PersonKey, String> {
    for peer in &peers {
        parse_peer(peer).ok_or_else(|| format!("'{peer}' is not a peer (<key>[@host:port])"))?;
    }
    let now = now_millis();
    let key_path = config.person_key_path();
    let store = FileKeyStore::new(&key_path);
    let same = store
        .load()
        .ok()
        .flatten()
        .is_some_and(|current| current.words() == phrase.words());
    if !same {
        set_aside(&key_path, now)?;
        store.store(phrase).map_err(|e| e.to_string())?;
    }
    set_aside(&config.device_cert_path(), now)?;
    allow_rebind(config).map_err(|e| e.to_string())?;
    let marker = RecoveryMarker { revoke_old, peers };
    let path = config.data_directory().join(RECOVERY_MARKER);
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&marker).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(PersonKey::from_phrase(phrase))
}

/// Lets the next start move this device's database (if it holds no
/// waves) to its new person's domain.
fn allow_rebind(config: &Config) -> Result<(), PairError> {
    std::fs::write(
        config
            .data_directory()
            .join(resonator_server::app::REBIND_MARKER),
        b"",
    )
    .map_err(|e| PairError::Other(e.to_string()))
}

fn set_aside(path: &Path, now: i64) -> Result<(), String> {
    if path.exists() {
        let mut aside = path.as_os_str().to_owned();
        aside.push(format!(".old-{now}"));
        std::fs::rename(path, &aside)
            .map_err(|e| format!("cannot move {} aside: {e}", path.display()))?;
    }
    Ok(())
}

/// `<z32 key>` or `<z32 key>@<host:port>`.
pub fn parse_peer(text: &str) -> Option<(PublicKey, Option<SocketAddr>)> {
    let (key, addr) = match text.trim().split_once('@') {
        Some((key, addr)) => (key, Some(addr.parse().ok()?)),
        None => (text.trim(), None),
    };
    let key = parse_key_domain(key).ok().flatten()?;
    Some((key, addr))
}

/// The anchor setting saved in the settings, if any.
pub fn saved_anchor(config: &Config) -> Option<bool> {
    let text = std::fs::read_to_string(config.data_directory().join(ANCHOR_FILE)).ok()?;
    match text.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// A device of the person as the settings list it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceEntry {
    /// `<z32 key>`.
    pub key: String,
    pub name: String,
    pub admin: bool,
    pub revoked: bool,
    /// This device.
    pub this: bool,
    /// Connected now.
    pub online: bool,
    /// When it last said Hello here (milliseconds), if since the start.
    pub last_seen: Option<i64>,
    /// It holds the host lease.
    pub holder: bool,
    pub anchor: bool,
}

/// What the settings show of the person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonOverview {
    /// The person domain (`<z32 person key>`).
    pub person: String,
    pub domain: String,
    /// This device: `<z32 key>`, its name, and whether it holds the
    /// person key.
    pub device: String,
    pub device_name: String,
    pub admin: bool,
    pub anchor: bool,
    pub devices: Vec<DeviceEntry>,
    pub lease: Option<LeaseEntry>,
    pub co_hosts: Vec<CoHostEntry>,
    pub revocation_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeaseEntry {
    pub holder: String,
    pub term: u64,
    pub expires_at: i64,
    pub held_here: bool,
    pub expired: bool,
    pub online: bool,
    /// Held by a co-host of this domain.
    pub co_host: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoHostEntry {
    pub domain: String,
    pub until: i64,
    pub active: bool,
}

/// What pairing by link or code did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JoinSummary {
    /// The person domain this device belongs to now.
    pub person: String,
    pub admin: bool,
    /// The inviting device.
    pub inviter: String,
}

/// The operations, shared by [`crate::Federation`] and [`HttpControl`].
pub struct PersonOps {
    pub(crate) ctx: Arc<Context>,
    pub(crate) config: Config,
    /// Person mode: answers pairing (on every device; only one holding
    /// the person key can invite).
    pub(crate) inviter: Option<Arc<Inviter>>,
    /// Pairing by code: where codes are looked up (none: links only).
    pub(crate) rendezvous: Option<Arc<dyn Rendezvous>>,
    /// How far the join under way has come.
    pub(crate) join_stage: std::sync::Mutex<Option<JoinStage>>,
}

impl std::fmt::Debug for PersonOps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersonOps")
            .field("domain", &self.ctx.domain)
            .finish_non_exhaustive()
    }
}

fn refused(message: impl Into<String>) -> PairError {
    PairError::Refused(message.into())
}

impl PersonOps {
    /// The endpoint, while federation runs (none after its shutdown, so
    /// that nothing keeps it).
    fn endpoint(&self) -> Result<FederationEndpoint, PairError> {
        self.ctx
            .endpoint()
            .map_err(|_| PairError::Other("federation is stopped".to_owned()))
    }

    fn person_mode(&self) -> Result<(), PairError> {
        if self.ctx.person_mode {
            Ok(())
        } else {
            Err(refused("person mode is not enabled"))
        }
    }

    /// The person key, on an admin device.
    fn person_key(&self) -> Result<PersonKey, PairError> {
        self.person_mode()?;
        let phrase = FileKeyStore::new(self.config.person_key_path())
            .load()
            .map_err(|e| PairError::Other(e.to_string()))?
            .ok_or(PairError::NotAdmin)?;
        let key = PersonKey::from_phrase(&phrase);
        if self.ctx.trust.persons().own() != Some(key.public()) {
            return Err(PairError::NotAdmin);
        }
        Ok(key)
    }

    /// The recovery phrase (admin devices). Never log it.
    pub fn phrase(&self) -> Result<RecoveryPhrase, PairError> {
        self.person_key()?;
        FileKeyStore::new(self.config.person_key_path())
            .load()
            .map_err(|e| PairError::Other(e.to_string()))?
            .ok_or(PairError::NotAdmin)
    }

    fn inviter(&self) -> Result<&Arc<Inviter>, PairError> {
        self.person_mode()?;
        self.inviter.as_ref().ok_or(PairError::NotAdmin)
    }

    pub async fn invite(&self, admin: bool) -> Result<NewInvitation, PairError> {
        self.inviter()?.invite(admin).await
    }

    pub async fn invitations(&self) -> Result<Vec<PendingPairing>, PairError> {
        Ok(self.inviter()?.invitations().await)
    }

    pub async fn confirm(&self, id: &str) -> Result<(), PairError> {
        self.inviter()?.confirm(id).await
    }

    pub async fn cancel(&self, id: &str) -> Result<bool, PairError> {
        self.inviter()?.cancel(id).await
    }

    /// Pairs this device by `text`, a pairing link or code (see
    /// [`crate::pairing::join`] and [`crate::pairing::join_code`]), and
    /// writes what makes it a device of that person; it is one after the
    /// next start.
    pub async fn join(
        &self,
        text: &str,
        name: &str,
        wait: Duration,
    ) -> Result<JoinSummary, PairError> {
        self.person_mode()?;
        let name = if name.is_empty() {
            self.config.person.device_name.clone()
        } else {
            name.to_owned()
        };
        let endpoint = self.endpoint()?;
        let own_addrs = own_addrs(&endpoint);
        let progress = |stage: JoinStage| {
            if let Ok(mut current) = self.join_stage.lock() {
                *current = Some(stage);
            }
        };
        let joined = join_any(
            &self.config,
            endpoint.endpoint(),
            self.rendezvous.as_deref(),
            text,
            &name,
            own_addrs,
            wait,
            &progress,
        )
        .await;
        drop(endpoint);
        if let Ok(mut current) = self.join_stage.lock() {
            *current = None;
        }
        let joined = joined?;
        self.install(&joined).await?;
        Ok(summary(&joined))
    }

    /// How far the join under way has come (none if none runs).
    pub fn join_stage(&self) -> Option<JoinStage> {
        self.join_stage.lock().ok().and_then(|stage| *stage)
    }

    async fn install(&self, joined: &Joined) -> Result<(), PairError> {
        crate::pairing::install(
            joined,
            &self.config.person_key_path(),
            &self.config.device_cert_path(),
        )?;
        crate::pairing::remember(joined, &self.ctx.store).await?;
        allow_rebind(&self.config)?;
        // A recovery not finished yet is moot now.
        let _ = std::fs::remove_file(self.config.data_directory().join(RECOVERY_MARKER));
        tracing::info!(
            person = %joined.cert.person.fmt_short(),
            admin = joined.cert.admin,
            "paired this device; it becomes a device of that person at its next start"
        );
        Ok(())
    }

    /// The person, its devices, lease and co-hosts.
    pub fn overview(&self) -> Result<PersonOverview, PairError> {
        self.person_mode()?;
        let persons = self.ctx.trust.persons();
        let person = persons.own().ok_or_else(|| refused("no person"))?;
        let me = self.ctx.own_key();
        let now = self.ctx.now();
        let lease = self.ctx.leases.current(&person);
        let connected = self
            .endpoint()
            .map(|endpoint| endpoint.connected_peers())
            .unwrap_or_default();
        let anchors = self.ctx.leases.known_anchors();
        let admin = self.person_key().is_ok();
        let mut devices: Vec<DeviceEntry> = persons
            .certified_devices()
            .into_iter()
            .filter_map(|device| persons.cert(&device, &person))
            .map(|cert| DeviceEntry {
                key: cert.device.to_z32(),
                name: cert.name.clone(),
                admin: cert.admin,
                revoked: persons.is_revoked_by(&person, &cert.device),
                this: cert.device == me,
                online: cert.device == me || connected.contains(&cert.device),
                last_seen: if cert.device == me {
                    Some(now)
                } else {
                    self.ctx.leases.device_last_seen(&cert.device)
                },
                holder: lease.as_ref().is_some_and(|lease| {
                    lease.device == cert.device && lease.co_host_domain.is_none()
                }),
                anchor: if cert.device == me {
                    self.ctx.leases.anchor()
                } else {
                    anchors.contains(&cert.device)
                },
            })
            .collect();
        devices.sort_by(|a, b| (!a.this, a.revoked, &a.name).cmp(&(!b.this, b.revoked, &b.name)));
        let own_cert = persons.cert(&me, &person);
        Ok(PersonOverview {
            person: key_domain(&person),
            domain: self.ctx.domain.clone(),
            device: me.to_z32(),
            device_name: own_cert.as_ref().map_or_else(
                || self.config.person.device_name.clone(),
                |cert| cert.name.clone(),
            ),
            admin,
            anchor: self.ctx.leases.anchor(),
            devices,
            lease: lease.map(|lease| LeaseEntry {
                holder: lease.device.to_z32(),
                term: lease.term,
                expires_at: lease.expires_at,
                held_here: lease.device == me && !lease.expired(now),
                expired: lease.expired(now),
                online: self.ctx.leases.is_online(),
                co_host: lease.co_host_domain.clone(),
            }),
            co_hosts: self
                .ctx
                .trust
                .co_hosts()
                .of_person(&person)
                .into_iter()
                .map(|grant| CoHostEntry {
                    active: grant.active(now),
                    domain: grant.delegate_domain,
                    until: grant.until,
                })
                .collect(),
            revocation_seq: persons.revocation_seq(&person),
        })
    }

    /// Revokes `device` (admin), and tells every peer.
    pub async fn revoke(&self, device: PublicKey) -> Result<u64, PairError> {
        let key = self.person_key()?;
        if device == self.ctx.own_key() {
            return Err(refused("this device cannot revoke itself"));
        }
        let persons = self.ctx.trust.persons();
        let previous = persons.revocations(&key.public());
        let signed = key.revoke(previous.as_ref(), vec![device], self.ctx.now());
        let learned = self
            .ctx
            .trust
            .learn_revocations(&signed)
            .await
            .map_err(|e| PairError::Other(e.to_string()))?;
        crate::service::spread_revocations(&self.ctx).await;
        Ok(learned.map_or(0, |list| list.seq))
    }

    /// Makes this device an anchor or not (kept across starts).
    pub fn set_anchor(&self, anchor: bool) -> Result<(), PairError> {
        self.person_mode()?;
        let path = self.config.data_directory().join(ANCHOR_FILE);
        std::fs::write(&path, if anchor { "true\n" } else { "false\n" })
            .map_err(|e| PairError::Other(e.to_string()))?;
        self.ctx.leases.set_anchor(anchor);
        Ok(())
    }

    /// Grants co-hosting to `domain` until `until` (milliseconds; 0 or a
    /// past time withdraws it), and tells the peers and the delegate.
    pub async fn co_host(&self, domain: &str, until: i64) -> Result<(), PairError> {
        let key = self.person_key()?;
        if domain.is_empty() || domain == self.ctx.domain {
            return Err(refused("name another server's domain"));
        }
        let now = self.ctx.now();
        let until = if until <= now { now } else { until };
        let signed = key.grant_co_host(domain, until, now);
        self.ctx
            .trust
            .learn_co_host(&signed)
            .await
            .map_err(|e| PairError::Other(e.to_string()))?;
        let mut keys = self.endpoint()?.connected_peers();
        keys.extend(crate::lease::own_devices(&self.ctx));
        if let Ok(delegate) = self.ctx.key_of(domain).await {
            keys.push(delegate);
        }
        keys.sort();
        keys.dedup();
        crate::cohost::push(&self.ctx, &keys).await;
        Ok(())
    }

    /// Whether the host of `name` is frozen: a person whose lease ran
    /// out with none of its devices reachable (nor a co-host holding
    /// it), or whose holding device was revoked.
    pub async fn host_status(&self, name: &WaveletName) -> HostStatus {
        let domain = self.ctx.server.epochs(name).pop().map_or_else(
            || name.wavelet_id.domain().to_owned(),
            |epoch| epoch.holder_domain,
        );
        let mut status = HostStatus {
            host: domain.clone(),
            ..HostStatus::default()
        };
        if self.ctx.server.is_local_wavelet(name) {
            status.local = true;
            return status;
        }
        let persons = self.ctx.trust.persons();
        let candidates = persons_of(persons, name.wavelet_id.domain());
        let Some(person) = candidates.first().copied() else {
            return status;
        };
        let now = self.ctx.now();
        let Some(lease) = self.ctx.leases.current(&person) else {
            return status;
        };
        if persons.is_revoked_by(&person, &lease.device) && lease.co_host_domain.is_none() {
            status.frozen = true;
            status.reason = "the device hosting it was revoked".to_owned();
            return status;
        }
        if !lease.expired(now) {
            return status;
        }
        // Is any device of the person (or a co-host) reachable?
        let wavelet_domain = name.wavelet_id.domain().to_owned();
        let mut devices = self.ctx.trust.devices_of(&wavelet_domain);
        devices.retain(|device| !persons.is_revoked_by(&person, device));
        let Ok(endpoint) = self.endpoint() else {
            return status;
        };
        for device in devices {
            let reached = tokio::time::timeout(
                Duration::from_secs(3),
                endpoint.connect(device, Some(&wavelet_domain)),
            )
            .await;
            if matches!(reached, Ok(Ok(_))) {
                return status;
            }
        }
        status.frozen = true;
        status.reason = "its host's lease ran out and none of its devices answers".to_owned();
        status
    }

    /// Finishes a recovery left by [`prepare_recovery`]: asks peers until
    /// one answers, then removes the marker.
    pub(crate) async fn finish_recovery(
        self: Arc<Self>,
        cancel: tokio_util::sync::CancellationToken,
    ) {
        /// Lets the lease task claim again however this ends.
        struct Release<'a>(&'a Context);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                self.0.leases.hold_back(false);
            }
        }
        let mut release = Some(Release(&self.ctx));
        let path = self.config.data_directory().join(RECOVERY_MARKER);
        let Ok(text) = std::fs::read(&path) else {
            return;
        };
        let marker: RecoveryMarker = match serde_json::from_slice(&text) {
            Ok(marker) => marker,
            Err(error) => {
                tracing::warn!(%error, "ignoring an unreadable recovery marker");
                let _ = std::fs::remove_file(&path);
                return;
            }
        };
        let Ok(key) = self.person_key() else {
            tracing::warn!("a recovery is pending, but this device holds no person key");
            return;
        };
        let mut peers = Vec::new();
        for (peer, addr) in marker.peers.iter().filter_map(|peer| parse_peer(peer)) {
            if let Some(addr) = addr {
                let Ok(endpoint) = self.endpoint() else {
                    return;
                };
                endpoint.add_peer_addr(EndpointAddr::new(peer).with_ip_addr(addr));
            }
            peers.push(peer);
        }
        let mut wait = Duration::from_millis(200);
        let mut waited = Duration::ZERO;
        loop {
            let report = crate::recovery::recover(&self.ctx, &key, &peers, marker.revoke_old).await;
            if report.peers > 0 {
                let _ = std::fs::remove_file(&path);
                return;
            }
            tracing::info!("no peer answered the recovery yet; trying again");
            // Nobody to pull from for a while: host what is here.
            if waited >= RECOVERY_HOLD {
                release.take();
            }
            waited += wait;
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(wait) => {}
            }
            wait = (wait * 2).min(Duration::from_secs(60));
        }
    }
}

fn summary(joined: &Joined) -> JoinSummary {
    JoinSummary {
        person: key_domain(&joined.cert.person),
        admin: joined.cert.admin,
        inviter: joined.inviter.to_z32(),
    }
}

/// The rendezvous of pairing by code `config` names
/// (`federation.pairing_rendezvous`), if any.
pub fn config_rendezvous(config: &Config) -> Option<Arc<dyn Rendezvous>> {
    crate::pair_code::rendezvous_from_url(config.federation.pairing_rendezvous_url().ok().flatten())
}

/// Joins by `text`, a link or a code, refusing an invitation for another
/// account than a device's own.
#[allow(clippy::too_many_arguments)]
async fn join_any(
    config: &Config,
    endpoint: &iroh::Endpoint,
    rendezvous: Option<&dyn Rendezvous>,
    text: &str,
    name: &str,
    own_addrs: Vec<String>,
    wait: Duration,
    progress: Progress<'_>,
) -> Result<Joined, PairError> {
    let own_account = config.is_device().then_some(config.device.account.as_str());
    if crate::pair_code::is_code(text) {
        let code = PairCode::parse(text).map_err(|error| PairError::BadCode(error.to_string()))?;
        let rendezvous = rendezvous.ok_or_else(|| {
            refused("pairing by code is off on this device (federation.pairing_rendezvous); use the link")
        })?;
        return crate::pairing::join_code(
            endpoint,
            rendezvous,
            &code,
            name,
            own_addrs,
            wait,
            own_account,
            progress,
        )
        .await;
    }
    let invitation = crate::pairing::decode_link(text)?;
    let account = invitation.account.clone().unwrap_or_default();
    if let Some(own) = own_account
        && !account.is_empty()
        && account != own
    {
        return Err(refused(format!(
            "the invitation is for the account '{account}', this device's is '{own}'"
        )));
    }
    crate::pairing::join_with_progress(endpoint, text, name, own_addrs, wait, progress).await
}

/// This device's dialable socket addresses, as `<host:port>`.
fn own_addrs(endpoint: &FederationEndpoint) -> Vec<String> {
    endpoint
        .addr()
        .ip_addrs()
        .map(ToString::to_string)
        .collect()
}

/// [`InviterHost`] over the running node.
pub(crate) struct NodeInviterHost {
    pub(crate) ctx: Arc<Context>,
}

#[async_trait]
impl InviterHost for NodeInviterHost {
    fn store(&self) -> FederationStore {
        self.ctx.store.clone()
    }

    fn device(&self) -> PublicKey {
        self.ctx.own_key()
    }

    fn addr(&self) -> EndpointAddr {
        match self.ctx.endpoint() {
            Ok(endpoint) => endpoint.addr(),
            Err(_) => EndpointAddr::new(self.ctx.own_key()),
        }
    }

    async fn learn_device(&self, cert: &VerifiedCert, addrs: &[SocketAddr]) {
        if let Err(error) = self.ctx.trust.learn_cert(&cert.signed).await {
            tracing::warn!(%error, "cannot record a paired device's certificate");
        }
        let now = self.ctx.now();
        let mut addr = EndpointAddr::new(cert.device);
        for socket in addrs {
            addr = addr.with_ip_addr(*socket);
            let _ = self
                .ctx
                .store
                .put_device_addr(cert.device.as_bytes().to_vec(), socket.to_string(), now)
                .await;
        }
        if let Ok(endpoint) = self.ctx.endpoint() {
            endpoint.add_peer_addr(addr);
        }
        self.ctx
            .trust
            .persons()
            .add_static_device(&self.ctx.domain, cert.device);
        self.ctx.leases.changed.notify_one();
    }

    async fn bundle(&self) -> PersonBundle {
        let persons = self.ctx.trust.persons();
        let Some(person) = persons.own() else {
            return PersonBundle::default();
        };
        let mut device_addrs = Vec::new();
        if let Ok(stored) = self.ctx.store.device_addrs().await {
            for (device, addr, _) in stored {
                if let Ok(bytes) = <[u8; 32]>::try_from(device.as_slice())
                    && let Ok(key) = PublicKey::from_bytes(&bytes)
                    && persons.cert(&key, &person).is_some()
                    && !persons.is_revoked_by(&person, &key)
                {
                    device_addrs.push(format!("{} {addr}", key.to_z32()));
                }
            }
        }
        PersonBundle {
            certs: persons
                .certified_devices()
                .into_iter()
                .filter_map(|device| persons.cert(&device, &person))
                .map(|cert| cert.signed)
                .collect(),
            revocations: persons.revocations(&person).map(|list| list.signed),
            leases: crate::lease::hello_leases(&self.ctx.leases),
            co_hosts: self
                .ctx
                .trust
                .co_hosts()
                .of_person(&person)
                .into_iter()
                .map(|grant| grant.signed)
                .collect(),
            device_addrs,
        }
    }
}

/// The settings UI's side: [`PersonControl`] over [`PersonOps`]. Pairing
/// by link and recovery make the node restart
/// ([`AppState::request_restart`]).
pub(crate) struct HttpControl {
    pub(crate) ops: Arc<PersonOps>,
    pub(crate) app: AppState,
}

fn control_error(error: PairError) -> ControlError {
    let status = match &error {
        PairError::BadLink | PairError::BadCode(_) => 400,
        PairError::CodeNotFound => 404,
        PairError::Expired
        | PairError::Refused(_)
        | PairError::NotConfirmed
        | PairError::WrongCode { .. } => 409,
        PairError::NotAdmin => 403,
        PairError::Connect(_) | PairError::BadAnswer(_) => 502,
        PairError::Other(_) => 500,
    };
    ControlError::new(status, error.to_string())
}

fn to_value<T: Serialize>(value: &T) -> Result<Value, ControlError> {
    serde_json::to_value(value).map_err(|e| ControlError::new(500, e.to_string()))
}

fn pending_json(pending: &PendingPairing) -> Value {
    json!({
        "id": pending.id,
        "admin": pending.admin,
        "expires_at": pending.expires_at,
        "state": pending.state,
        "joiner": pending.joiner,
        "joiner_name": pending.joiner_name,
    })
}

impl PersonControl for HttpControl {
    fn overview(&self) -> BoxFuture<'_, Result<Value, ControlError>> {
        async move {
            let overview = self.ops.overview().map_err(control_error)?;
            to_value(&overview)
        }
        .boxed()
    }

    fn revoke<'a>(&'a self, device: &'a str) -> BoxFuture<'a, Result<Value, ControlError>> {
        async move {
            let key = parse_key_domain(device)
                .ok()
                .flatten()
                .ok_or_else(|| ControlError::new(400, "Not a device key."))?;
            let seq = self.ops.revoke(key).await.map_err(control_error)?;
            Ok(json!({ "revocation_seq": seq }))
        }
        .boxed()
    }

    fn set_anchor(&self, anchor: bool) -> BoxFuture<'_, Result<Value, ControlError>> {
        async move {
            self.ops.set_anchor(anchor).map_err(control_error)?;
            Ok(json!({ "anchor": anchor }))
        }
        .boxed()
    }

    fn phrase(&self) -> Result<String, ControlError> {
        self.ops
            .phrase()
            .map(|phrase| phrase.words().to_owned())
            .map_err(control_error)
    }

    fn recover<'a>(
        &'a self,
        phrase: &'a str,
        revoke_old: bool,
        peers: Vec<String>,
    ) -> BoxFuture<'a, Result<Value, ControlError>> {
        async move {
            let phrase = RecoveryPhrase::parse(phrase)
                .map_err(|_| ControlError::new(400, "That is not a valid recovery phrase."))?;
            let key = prepare_recovery(&self.ops.config, &phrase, revoke_old, peers)
                .map_err(|e| ControlError::new(400, e))?;
            self.app.request_restart();
            Ok(json!({ "person": key.domain(), "restart": true }))
        }
        .boxed()
    }

    fn co_host<'a>(
        &'a self,
        domain: &'a str,
        until: i64,
    ) -> BoxFuture<'a, Result<Value, ControlError>> {
        async move {
            self.ops
                .co_host(domain, until)
                .await
                .map_err(control_error)?;
            Ok(json!({ "domain": domain, "until": until }))
        }
        .boxed()
    }

    fn invite(&self, admin: bool) -> BoxFuture<'_, Result<Value, ControlError>> {
        async move {
            let invitation = self.ops.invite(admin).await.map_err(control_error)?;
            let mut value = pending_json(&invitation.pending);
            value["qr"] = json!(crate::pairing::qr_svg(&invitation.link));
            value["link"] = json!(invitation.link);
            value["code"] = json!(invitation.code);
            Ok(value)
        }
        .boxed()
    }

    fn invitations(&self) -> BoxFuture<'_, Result<Value, ControlError>> {
        async move {
            let list = self.ops.invitations().await.map_err(control_error)?;
            Ok(json!({ "invitations": list.iter().map(pending_json).collect::<Vec<_>>() }))
        }
        .boxed()
    }

    fn confirm<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Value, ControlError>> {
        async move {
            self.ops.confirm(id).await.map_err(control_error)?;
            Ok(json!({ "confirmed": id }))
        }
        .boxed()
    }

    fn cancel<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Value, ControlError>> {
        async move {
            let removed = self.ops.cancel(id).await.map_err(control_error)?;
            Ok(json!({ "removed": removed }))
        }
        .boxed()
    }

    fn join<'a>(
        &'a self,
        text: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Value, ControlError>> {
        async move {
            let summary = self
                .ops
                .join(text, name, CONFIRM_WAIT)
                .await
                .map_err(control_error)?;
            self.app.request_restart();
            let mut value = to_value(&summary)?;
            value["restart"] = json!(true);
            Ok(value)
        }
        .boxed()
    }

    fn join_progress(&self) -> Value {
        json!({ "stage": self.ops.join_stage().map(JoinStage::as_str) })
    }

    fn host_status<'a>(&'a self, wavelet: &'a WaveletName) -> BoxFuture<'a, HostStatus> {
        self.ops.host_status(wavelet).boxed()
    }
}

/// The path of the recovery marker of `config`.
pub fn recovery_marker_path(config: &Config) -> PathBuf {
    config.data_directory().join(RECOVERY_MARKER)
}

/// [`InviterHost`] for the command line, on the database a running node
/// shares: makes invitations that the node answers.
struct OfflineHost {
    store: FederationStore,
    device: PublicKey,
    addr: EndpointAddr,
}

#[async_trait]
impl InviterHost for OfflineHost {
    fn store(&self) -> FederationStore {
        self.store.clone()
    }

    fn device(&self) -> PublicKey {
        self.device
    }

    fn addr(&self) -> EndpointAddr {
        self.addr.clone()
    }

    async fn learn_device(&self, _: &VerifiedCert, _: &[SocketAddr]) {}

    async fn bundle(&self) -> PersonBundle {
        PersonBundle::default()
    }
}

/// `person pair invite` on the command line: an invitation in the
/// database of the device of `config` (an admin device), which its
/// running node answers. The link names the addresses the node recorded
/// at its last start and the configured federation listen addresses; so
/// does the rendezvous record of its code (published on `rendezvous`).
pub async fn offline_invite(
    config: &Config,
    store: FederationStore,
    device: PublicKey,
    admin: bool,
    rendezvous: Option<Arc<dyn Rendezvous>>,
) -> Result<NewInvitation, PairError> {
    offline_inviter(config, store, device, rendezvous)
        .await
        .invite(admin)
        .await
}

/// `person pair cancel` on the command line: withdraws an invitation and
/// its code's rendezvous record.
pub async fn offline_cancel(
    config: &Config,
    store: FederationStore,
    device: PublicKey,
    id: &str,
    rendezvous: Option<Arc<dyn Rendezvous>>,
) -> Result<bool, PairError> {
    offline_inviter(config, store, device, rendezvous)
        .await
        .cancel(id)
        .await
}

async fn offline_inviter(
    config: &Config,
    store: FederationStore,
    device: PublicKey,
    rendezvous: Option<Arc<dyn Rendezvous>>,
) -> Inviter {
    let mut addr = EndpointAddr::new(device);
    if let Ok(stored) = store.device_addrs().await {
        for (key, socket, _) in stored {
            if key.as_slice() == device.as_bytes()
                && let Ok(socket) = socket.parse::<SocketAddr>()
            {
                addr = addr.with_ip_addr(socket);
            }
        }
    }
    for socket in config.federation.listen_addrs().unwrap_or_default() {
        if !socket.ip().is_unspecified() {
            addr = addr.with_ip_addr(socket);
        }
    }
    if let Ok(resonator_server::config::RelaySetting::Custom(url)) =
        config.federation.relay_setting()
        && let Ok(url) = url.parse::<iroh::RelayUrl>()
    {
        addr = addr.with_relay_url(url);
    }
    let account = if config.is_device() {
        config.device.account.clone()
    } else {
        String::new()
    };
    Inviter::new(
        Arc::new(OfflineHost {
            store,
            device,
            addr,
        }),
        config.person_key_path(),
        account,
    )
    .with_rendezvous(rendezvous)
}

/// `person pair join` on the command line: pairs the device of `config`
/// (its key `secret`) by `text`, a link or a code (looked up on
/// `rendezvous`), from an endpoint of its own, and writes
/// the certificate, the person key (admin pairing) and what the inviting
/// device sent into `store`. The node is that person's device at its next
/// start.
#[allow(clippy::too_many_arguments)]
pub async fn offline_join(
    config: &Config,
    secret: iroh::SecretKey,
    store: &FederationStore,
    text: &str,
    name: &str,
    wait: Duration,
    rendezvous: Option<Arc<dyn Rendezvous>>,
    progress: Progress<'_>,
) -> Result<JoinSummary, PairError> {
    // Refuse what is neither before binding an endpoint.
    if crate::pair_code::is_code(text) {
        PairCode::parse(text).map_err(|error| PairError::BadCode(error.to_string()))?;
    } else {
        crate::pairing::decode_link(text)?;
    }
    let relay_mode = match config.federation.relay_setting() {
        Ok(resonator_server::config::RelaySetting::Disabled) => iroh::RelayMode::Disabled,
        Ok(resonator_server::config::RelaySetting::Custom(url)) => {
            match url.parse::<iroh::RelayUrl>() {
                Ok(url) => iroh::RelayMode::Custom(iroh::RelayMap::from(url)),
                Err(_) => iroh::RelayMode::Disabled,
            }
        }
        _ => iroh::RelayMode::Default,
    };
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(secret)
        .relay_mode(relay_mode)
        .bind()
        .await
        .map_err(|e| PairError::Connect(e.to_string()))?;
    let name = if name.is_empty() {
        config.person.device_name.clone()
    } else {
        name.to_owned()
    };
    let joined = join_any(
        config,
        &endpoint,
        rendezvous.as_deref(),
        text,
        &name,
        Vec::new(),
        wait,
        progress,
    )
    .await;
    endpoint.close().await;
    let joined = joined?;
    crate::pairing::install(
        &joined,
        &config.person_key_path(),
        &config.device_cert_path(),
    )?;
    crate::pairing::remember(&joined, store).await?;
    allow_rebind(config)?;
    let _ = std::fs::remove_file(config.data_directory().join(RECOVERY_MARKER));
    Ok(summary(&joined))
}
