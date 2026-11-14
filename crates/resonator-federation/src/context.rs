//! What the wave operations share: the wave server, the federation tables,
//! the trust rules and the endpoint, plus conversions between wire and
//! model types.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::PublicKey;
use resonator_model::id::{IdUriCodec, WaveletName};
use resonator_model::proto::federation::{
    ProtocolHashedVersion, ProtocolSignedDelta, ProtocolSignerInfo,
};
use resonator_model::proto::federation_error::{FederationError, federation_error::Code};
use resonator_model::proto::federation_rpc::{federation_request, federation_response};
use resonator_model::proto::{decode, deserialize_hashed_version, serialize_hashed_version};
use resonator_model::version::HashedVersion;
use resonator_server::clock::Clock;
use resonator_server::storage::FederationStore;
use resonator_server::waveserver::WaveServer;
use tokio::sync::Notify;

use crate::endpoint::{FederationEndpoint, RequestError};
use crate::signing::{Signer, parse_signer_info};
use crate::trust::Trust;

/// Timing and size limits of the wave operations. The defaults suit
/// production; tests shorten the backoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tuning {
    /// The first retry of an undelivered notification waits this long;
    /// each further retry twice as long as the one before.
    pub retry_min: Duration,
    /// The longest wait between retries.
    pub retry_max: Duration,
    /// Notifications not delivered for this long are given up; the remote
    /// server catches up through `history` once it hears of the wavelet
    /// again.
    pub outbox_expiry: Duration,
    /// Bytes of applied deltas in one `update` or `history` answer, well
    /// under the 4 MiB frame limit.
    pub page_bytes: usize,
    /// Deltas in one `update` or `history` answer.
    pub page_deltas: usize,
    /// Out-of-order deltas a replica holds while it fills a gap.
    pub max_queued_deltas: usize,
    /// How long `update` waits for the host's own commit before sending
    /// `commit`.
    pub commit_wait: Duration,
    /// Person mode: how often stored revocation lists are read again (the
    /// `person revoke` command writes them while the server runs).
    pub revocation_poll: Duration,
    /// Person mode: how often an anchor device holding the host lease
    /// renews it; a lease lasts three intervals.
    pub lease_renew_anchor: Duration,
    /// Person mode: how often another device holding the host lease
    /// renews it.
    pub lease_renew: Duration,
    /// Person mode: how often a device compares what it has with the
    /// other devices of its person (`sync-summary`) and sends pending
    /// submissions again.
    pub sync_interval: Duration,
    /// Push (`[push] relay` set): a sleeping device is woken at most once
    /// in this long (longer while wakes go unanswered, up to an hour).
    pub wake_interval: Duration,
    /// Push: how soon an undelivered notification to a device just woken
    /// is retried (instead of the backoff), within `wake_window`.
    pub wake_retry: Duration,
    /// Push: how long after a wake its device is expected up.
    pub wake_window: Duration,
    /// Push: how long a lease handover waits for a woken device.
    pub wake_wait: Duration,
    /// Push: how long an attachment fetch waits for a woken device.
    pub wake_fetch: Duration,
    /// Push: how often the push task looks for new hosts to tell how
    /// this device is woken (they are told again every six hours).
    pub push_advertise: Duration,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            retry_min: Duration::from_millis(500),
            retry_max: Duration::from_secs(60),
            outbox_expiry: Duration::from_secs(7 * 24 * 60 * 60),
            page_bytes: 3 * 1024 * 1024,
            page_deltas: 1000,
            max_queued_deltas: 1000,
            commit_wait: Duration::from_secs(10),
            revocation_poll: Duration::from_secs(10),
            lease_renew_anchor: Duration::from_secs(600),
            lease_renew: Duration::from_secs(120),
            sync_interval: Duration::from_secs(60),
            wake_interval: Duration::from_secs(120),
            wake_retry: Duration::from_secs(2),
            wake_window: Duration::from_secs(30),
            wake_wait: Duration::from_secs(20),
            wake_fetch: Duration::from_secs(8),
            push_advertise: Duration::from_secs(60),
        }
    }
}

impl Tuning {
    /// The wait before retry number `attempts + 1`.
    pub fn backoff(&self, attempts: u32) -> Duration {
        let factor = 1u32.checked_shl(attempts.min(31)).unwrap_or(u32::MAX);
        self.retry_min
            .checked_mul(factor)
            .unwrap_or(self.retry_max)
            .min(self.retry_max)
    }
}

/// A replica's deltas that arrived ahead of the history it has, by the
/// version they apply at.
#[derive(Default)]
pub(crate) struct ReplicaQueue {
    pub queued: std::collections::BTreeMap<u64, resonator_server::storage::AppliedDelta>,
}

pub(crate) struct Context {
    pub server: WaveServer,
    pub domain: String,
    pub store: FederationStore,
    pub trust: Trust,
    pub clock: Arc<dyn Clock>,
    pub tuning: Tuning,
    /// This server's signer info, posted to hosts before submitting.
    pub signer_info: ProtocolSignerInfo,
    endpoint: Mutex<Option<FederationEndpoint>>,
    replicas: Mutex<HashMap<WaveletName, Arc<tokio::sync::Mutex<ReplicaQueue>>>>,
    /// Peer keys our signer info was posted to since start.
    pub posted: Mutex<HashSet<PublicKey>>,
    /// Wakes the outbox worker.
    pub outbox_wake: Notify,
    /// Holds the outbox worker (for tests).
    pub outbox_paused: AtomicBool,
    /// The server key, which signs epoch statements.
    pub secret: iroh::SecretKey,
    /// Who may hold the host role of which wavelet.
    pub epoch_authority: Arc<dyn crate::epoch::EpochAuthority>,
    /// Wakes the orphan worker (after a rewind).
    pub orphans_wake: Notify,
    /// Requests being applied, by request id: a copy of one waits for the
    /// first and gets its answer (with the domain it came from).
    in_flight: Mutex<HashMap<String, Claim>>,
    /// Person mode: this node is a device of the person of `domain`.
    pub person_mode: bool,
    /// The keys of the configured static peers.
    pub static_peers: Mutex<Vec<PublicKey>>,
    /// The revocation `seq` spread to peers, by person.
    pub revocations_spread: Mutex<HashMap<[u8; 32], u64>>,
    /// Person mode: the host leases known (shared with the trust rules).
    pub leases: Arc<crate::lease::Leases>,
    /// Wakes the drain of pending submissions (a peer or the holder
    /// changed).
    pub pending_wake: Notify,
    /// Push (`[push] relay` set): waking sleeping devices.
    pub push: Option<Arc<crate::push::Push>>,
    /// Person mode: attachments copied ahead between the person's
    /// devices (`replicate_own_attachments`); unset otherwise and after
    /// shutdown.
    copies: Mutex<Option<Arc<crate::copies::Copies>>>,
    /// Names and avatars, when there is an HTTP front end.
    pub profiles: std::sync::OnceLock<resonator_server::profile::Profiles>,
}

/// The answer to a request being applied once, with the domain it came
/// from.
pub(crate) type Claim = Arc<tokio::sync::OnceCell<(String, federation_response::Result)>>;

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Context {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        server: WaveServer,
        domain: String,
        trust: Trust,
        clock: Arc<dyn Clock>,
        tuning: Tuning,
        signer_info: ProtocolSignerInfo,
        secret: iroh::SecretKey,
        epoch_authority: Arc<dyn crate::epoch::EpochAuthority>,
    ) -> Self {
        Context {
            store: server.database().federation_store(),
            server,
            domain,
            trust: trust.clone(),
            clock,
            tuning,
            signer_info,
            endpoint: Mutex::new(None),
            replicas: Mutex::new(HashMap::new()),
            posted: Mutex::new(HashSet::new()),
            outbox_wake: Notify::new(),
            outbox_paused: AtomicBool::new(false),
            secret,
            epoch_authority,
            orphans_wake: Notify::new(),
            in_flight: Mutex::new(HashMap::new()),
            person_mode: false,
            static_peers: Mutex::new(Vec::new()),
            revocations_spread: Mutex::new(HashMap::new()),
            leases: trust.leases(),
            pending_wake: Notify::new(),
            push: None,
            copies: Mutex::new(None),
            profiles: std::sync::OnceLock::new(),
        }
    }

    /// Person mode: the copying of attachments between devices, if on.
    pub fn copies(&self) -> Option<Arc<crate::copies::Copies>> {
        lock(&self.copies).clone()
    }

    pub fn set_copies(&self, copies: Option<Arc<crate::copies::Copies>>) {
        *lock(&self.copies) = copies;
    }

    /// Turns push on.
    pub fn with_push(mut self, push: Option<Arc<crate::push::Push>>) -> Self {
        self.push = push;
        self
    }

    /// Whether `key` is another device of this node's person (certified
    /// by it and not revoked).
    pub fn is_own_device(&self, key: &PublicKey) -> bool {
        if !self.person_mode || *key == self.own_key() {
            return false;
        }
        let persons = self.trust.persons();
        persons.own().is_some_and(|person| {
            persons.cert(key, &person).is_some() && !persons.is_revoked_by(&person, key)
        })
    }

    /// Records the keys of the configured static peers.
    pub fn set_static_peers(&self, peers: Vec<PublicKey>) {
        *lock(&self.static_peers) = peers;
    }

    /// Marks this node as a device of the person of its domain.
    pub fn with_person_mode(mut self, person_mode: bool) -> Self {
        self.person_mode = person_mode;
        self
    }

    /// This node's key.
    pub fn own_key(&self) -> PublicKey {
        self.secret.public()
    }

    /// The domain hosting a wavelet and the key to dial for it: the
    /// device of this node's person that holds it
    /// ([`WaveletHost::Delegated`](resonator_server::waveserver::WaveletHost)),
    /// the certified device holding its newest host epoch, or whatever
    /// key the host domain resolves to.
    pub async fn host_key(&self, name: &WaveletName) -> Result<(String, PublicKey), RequestError> {
        let host = self.server.host_domain(name);
        let key = |bytes: &[u8]| {
            <&[u8; 32]>::try_from(bytes)
                .ok()
                .and_then(|bytes| PublicKey::from_bytes(bytes).ok())
        };
        if let Some(holder) = self.server.host_key(name).as_deref().and_then(key) {
            return Ok((host, holder));
        }
        if let Some(epoch) = self.server.epochs(name).last()
            && let Some(holder) = key(&epoch.holder)
            && holder != self.own_key()
            && self.trust.persons().has_unrevoked_cert(&holder)
        {
            return Ok((host, holder));
        }
        let key = self.key_of(&host).await?;
        Ok((host, key))
    }

    /// Sends a request to the host of a wavelet ([`Context::host_key`]),
    /// which must be authorised for the host domain.
    pub async fn request_host(
        &self,
        name: &WaveletName,
        id: Option<String>,
        op: federation_request::Op,
    ) -> Result<federation_response::Result, RequestError> {
        let (host, key) = self.host_key(name).await?;
        let endpoint = self.endpoint()?;
        match id {
            Some(id) => endpoint.request_with_id(key, Some(&host), id, op).await,
            None => endpoint.request(key, Some(&host), op).await,
        }
    }

    /// The outbox targets that stand for `domain`: each known device of a
    /// person domain (`<domain>/<device key>`), else the domain itself.
    /// This node's own domain is a target only in person mode, for the
    /// other devices of its person.
    pub fn targets(&self, domain: &str) -> Vec<String> {
        let own = self.own_key();
        let mut devices: Vec<String> = self
            .trust
            .devices_of(domain)
            .into_iter()
            .filter(|device| *device != own)
            .map(|device| format!("{domain}/{}", device.to_z32()))
            .collect();
        // Devices its cached device directory lists that are not known
        // here yet: the domain's own row is handed to them, greeted first
        // (`crate::directory::hand_to_devices`).
        if domain != self.domain && !self.trust.listed_devices(domain).is_empty() {
            devices.push(domain.to_owned());
        }
        if domain == self.domain {
            return if self.person_mode {
                devices
            } else {
                Vec::new()
            };
        }
        if devices.is_empty() {
            vec![domain.to_owned()]
        } else {
            devices
        }
    }

    /// Sends a request to an outbox target ([`Context::targets`]).
    pub async fn request_target(
        &self,
        target: &str,
        id: Option<String>,
        op: federation_request::Op,
    ) -> Result<federation_response::Result, RequestError> {
        let (domain, device) = parse_target(target);
        let Some(device) = device else {
            return self.request(domain, id, op).await;
        };
        let key = crate::key::parse_key_domain(device)
            .ok()
            .flatten()
            .ok_or_else(|| RequestError::Connect {
                peer: target.to_owned(),
                message: "not a device key".to_owned(),
            })?;
        let endpoint = self.endpoint()?;
        match id {
            Some(id) => endpoint.request_with_id(key, Some(domain), id, op).await,
            None => endpoint.request(key, Some(domain), op).await,
        }
    }

    /// Records the device certificate a signer info carries (person
    /// mode), if it certifies the signer's key.
    pub async fn learn_signer_cert(&self, info: &ProtocolSignerInfo, signer: &Signer) {
        let Some(cert) = &info.device_cert else {
            return;
        };
        match crate::person::verify_cert(cert) {
            Ok(verified) if verified.device == signer.key => {
                if let Err(error) = self.trust.learn_cert(cert).await {
                    tracing::warn!(%error, "cannot record a device certificate");
                }
            }
            Ok(_) => tracing::info!("a signer info carries another key's certificate"),
            Err(error) => tracing::info!(%error, "a signer info carries a bad certificate"),
        }
    }

    /// Claims a request id: the first caller applies the request, and
    /// callers with the same id until it is released wait for its answer.
    pub fn claim(&self, request_id: &str) -> Claim {
        Arc::clone(
            lock(&self.in_flight)
                .entry(request_id.to_owned())
                .or_default(),
        )
    }

    /// Releases a claim once its answer is durable: later copies of the
    /// request find it recorded.
    pub fn release(&self, request_id: &str, claim: &Claim) {
        let mut in_flight = lock(&self.in_flight);
        if in_flight
            .get(request_id)
            .is_some_and(|current| Arc::ptr_eq(current, claim) && current.initialized())
        {
            in_flight.remove(request_id);
        }
    }

    /// Sets the endpoint once it is bound, or forgets it at shutdown (the
    /// endpoint's handler holds this context).
    pub fn set_endpoint(&self, endpoint: Option<FederationEndpoint>) {
        *lock(&self.endpoint) = endpoint;
    }

    pub fn endpoint(&self) -> Result<FederationEndpoint, RequestError> {
        lock(&self.endpoint).clone().ok_or(RequestError::Closed)
    }

    pub fn now(&self) -> i64 {
        self.clock.now_millis()
    }

    /// The lock serialising the updates of one replica.
    pub fn replica(&self, name: &WaveletName) -> Arc<tokio::sync::Mutex<ReplicaQueue>> {
        let mut replicas = lock(&self.replicas);
        // Forget idle replicas now and then; their queues are empty.
        if replicas.len() > 1024 {
            replicas.retain(|_, queue| Arc::strong_count(queue) > 1);
        }
        Arc::clone(replicas.entry(name.clone()).or_default())
    }

    /// The key to dial for `domain`.
    pub async fn key_of(&self, domain: &str) -> Result<PublicKey, RequestError> {
        match self.trust.resolve(domain).await {
            Ok(Some(key)) => Ok(key),
            Ok(None) => Err(RequestError::Connect {
                peer: domain.to_owned(),
                message: "no server key is known for the domain".to_owned(),
            }),
            Err(error) => Err(RequestError::Connect {
                peer: domain.to_owned(),
                message: error.to_string(),
            }),
        }
    }

    /// Sends a request to the server of `domain`, which must be
    /// authorised for it.
    pub async fn request(
        &self,
        domain: &str,
        id: Option<String>,
        op: federation_request::Op,
    ) -> Result<federation_response::Result, RequestError> {
        let key = self.key_of(domain).await?;
        let endpoint = self.endpoint()?;
        match id {
            Some(id) => endpoint.request_with_id(key, Some(domain), id, op).await,
            None => endpoint.request(key, Some(domain), op).await,
        }
    }

    /// The signer recorded under `signer_id`.
    pub async fn stored_signer(&self, signer_id: &[u8]) -> Option<Signer> {
        let record = self.store.signer_info(signer_id).await.ok()??;
        let info: ProtocolSignerInfo = decode(&record.info).ok()?;
        let (id, signer) = parse_signer_info(&info).ok()?;
        (id == signer_id).then_some(signer)
    }

    /// The known signers of a signed delta's signatures, by signer id.
    pub async fn known_signers(&self, signed: &ProtocolSignedDelta) -> HashMap<Vec<u8>, Signer> {
        let mut signers = HashMap::new();
        for signature in &signed.signature {
            let Some(id) = signature.signer_id.as_deref() else {
                continue;
            };
            if signers.contains_key(id) {
                continue;
            }
            if let Some(signer) = self.stored_signer(id).await {
                signers.insert(id.to_vec(), signer);
            }
        }
        signers
    }
}

/// The domain of an outbox target, and the device key (z-base-32) if
/// the target is one device of a person domain.
pub(crate) fn parse_target(target: &str) -> (&str, Option<&str>) {
    match target.split_once('/') {
        Some((domain, device)) => (domain, Some(device)),
        None => (target, None),
    }
}

/// A [`FederationError`] result.
pub(crate) fn error(code: Code, message: impl Into<String>) -> federation_response::Result {
    federation_response::Result::Error(FederationError::new(code, message))
}

pub(crate) fn wavelet_uri(name: &WaveletName) -> String {
    IdUriCodec::wavelet_name_to_uri(name)
}

pub(crate) fn parse_wavelet_name(uri: Option<&str>) -> Result<WaveletName, FederationError> {
    IdUriCodec::uri_to_wavelet_name(uri.unwrap_or_default())
        .map_err(|e| FederationError::new(Code::BadRequest, format!("bad wavelet name: {e}")))
}

pub(crate) fn to_proto_version(
    version: &HashedVersion,
) -> Result<ProtocolHashedVersion, FederationError> {
    serialize_hashed_version(version)
        .map_err(|e| FederationError::new(Code::InternalServerError, e.to_string()))
}

pub(crate) fn from_proto_version(
    version: Option<&ProtocolHashedVersion>,
) -> Result<HashedVersion, FederationError> {
    let version =
        version.ok_or_else(|| FederationError::new(Code::BadRequest, "missing version"))?;
    deserialize_hashed_version(version)
        .map_err(|e| FederationError::new(Code::BadRequest, format!("bad version: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_maximum() {
        let tuning = Tuning::default();
        assert_eq!(tuning.backoff(0), Duration::from_millis(500));
        assert_eq!(tuning.backoff(1), Duration::from_secs(1));
        assert_eq!(tuning.backoff(3), Duration::from_secs(4));
        assert_eq!(tuning.backoff(7), Duration::from_secs(60));
        assert_eq!(tuning.backoff(u32::MAX), Duration::from_secs(60));
    }
}
