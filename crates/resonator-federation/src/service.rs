//! Federation as part of a running server: loading the server key before
//! the wave server starts (so that its deltas are signed), then starting
//! the endpoint with the trust rules and the request handler once it runs,
//! together with the fan-out to remote servers ([`crate::outbox`]) and the
//! forwarding of local users' submissions to remote hosts
//! ([`crate::remote`]).
//!
//! [`ServerHandler`] answers the wave operations: `submit`, `history`,
//! `signer-info` and `post-signer-info` as a host ([`crate::host`]),
//! `update` and `commit` as a replica ([`crate::replica`]). Requests that
//! change something are idempotent by their ULID (`federation_applied`).
//! With an attachment store ([`FederationSetup::serve_attachments`]), it
//! also answers `fetch-attachment` as the origin of attachments, and the
//! store fetches other servers' attachments through it
//! ([`crate::attachments`]).
//!
//! Person mode (`[person] enabled`, see [`crate::person`]): the node is
//! one device of a person. Its wave domain is the person domain
//! ([`FederationSetup::person_domain`]), its endpoint key is its device
//! key, certified by the person key, and exactly one device of the person
//! hosts the person's wavelets (`person.holder`); the others are replicas
//! that forward their users' submissions to it
//! ([`WaveletHost::Delegated`](resonator_server::waveserver::WaveletHost)).

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use iroh::{EndpointAddr, PublicKey};
use resonator_model::id::ParticipantId;
use resonator_model::proto::federation_error::federation_error::Code;
use resonator_model::proto::federation_rpc::{
    FederationResponse, ProbeResult, federation_request, federation_response,
};
use resonator_model::proto::{decode, encode};
use resonator_server::attachment::AttachmentService;
use resonator_server::clock::{Clock, SystemClock};
use resonator_server::config::{Config, FederationPeer};
use resonator_server::http::AppState;
use resonator_server::storage::{AccountStore, OutboxEntry, StorageError};
use resonator_server::waveserver::{
    PersonHosting, SharedDeltaSigner, SubmitReceipt, WaveBusEvent, WaveServer,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::attachments::{self, RemoteOrigin};
use crate::context::{Context, Tuning, error};
use crate::dns::{HickoryTxtResolver, TxtResolver};
use crate::endpoint::{
    Body, EndpointError, EndpointOptions, FederationEndpoint, LocalIdentity, Peer, RequestError,
    RequestHandler, default_response,
};
use crate::epoch::{EpochAuthority, WaveletDomainOnly};
use crate::key::{KeyError, ServerKey, key_domain, parse_key_domain};
use crate::lease::{LeaseError, LeaseState, VerifiedLease};
use crate::person::{
    FileKeyStore, KeyStore, PersonBook, PersonDevices, PersonError, PersonKey, VerifiedCert,
    cert_from_hex,
};
use crate::remote::{Forwarder, Prober};
use crate::signing::{
    Ed25519DeltaSigner, VerifyError, device_signer_info, signer_info, signer_info_record,
};
use crate::trust::{Policy, Trust};
use crate::{host, outbox, replica};
use resonator_model::id::WaveletName;
use resonator_model::proto::federation_error::FederationError;
use resonator_model::proto::federation_rpc::SignedEpochStart;
use resonator_model::version::HashedVersion;

/// Capabilities announced in the Hello.
pub const CAPABILITIES: &[&str] = &[
    "probe",
    "echo",
    "submit",
    "history",
    "signer-info",
    "post-signer-info",
    "update",
    "commit",
    "fetch-attachment",
    "resubmit",
];

/// How long applied request ids are remembered.
const APPLIED_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// How often old applied request ids are pruned.
const APPLIED_PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Failure to set up federation.
#[derive(Debug, Error)]
pub enum SetupError {
    #[error(transparent)]
    Key(#[from] KeyError),
    #[error("invalid federation configuration: {0}")]
    Config(String),
    #[error(transparent)]
    Endpoint(#[from] EndpointError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Signer(#[from] VerifyError),
    #[error("{0}")]
    Dns(String),
    #[error(transparent)]
    Person(#[from] PersonError),
}

/// Federation set up but not yet running: the server key is loaded (and
/// created on first start).
pub struct FederationSetup {
    config: Config,
    key: ServerKey,
    tuning: Tuning,
    resolver: Option<Arc<dyn TxtResolver>>,
    /// The wave bus updates since [`FederationSetup::subscribe`].
    events: Option<tokio::sync::mpsc::UnboundedReceiver<WaveBusEvent>>,
    /// The HTTP side; see [`FederationSetup::serve_app`].
    app: Option<AppState>,
    /// Who may host which wavelet in later epochs.
    epoch_authority: Arc<dyn EpochAuthority>,
    /// Person mode: who this device is.
    person: Option<PersonSetup>,
    /// What this node knows of persons (shared with the trust rules).
    persons: Arc<PersonBook>,
    /// Pairing by code: where codes are published and looked up (from
    /// `federation.pairing_rendezvous` unless replaced).
    rendezvous: Option<Arc<dyn crate::pair_code::Rendezvous>>,
}

/// This node as a device of a person.
#[derive(Debug, Clone)]
struct PersonSetup {
    /// The person domain.
    domain: String,
    /// This device's certificate.
    cert: VerifiedCert,
    /// Deprecated `person.holder`: the device to submit to before any
    /// host lease is known; `None` for none (or this one).
    holder: Option<PublicKey>,
    /// Whether this device is an anchor (preferred lease holder).
    anchor: bool,
}

impl PersonSetup {
    /// Who this device is, from the `[person]` settings: its certificate
    /// (self-issued on an admin device, which holds the person key),
    /// the person domain and the holder of the person's wavelets.
    fn load(config: &Config, device: &ServerKey) -> Result<Option<Self>, SetupError> {
        let settings = &config.person;
        if !settings.enabled {
            return Ok(None);
        }
        let store = FileKeyStore::new(config.person_key_path());
        // An admin device paired as a plain device (no person key, a
        // certificate) is one: the certificate decides.
        let paired_plain = settings.admin
            && settings.cert.is_empty()
            && config.device_cert_path().exists()
            && store.load()?.is_none();
        let cert = if settings.admin && !paired_plain {
            let phrase = store.load()?.ok_or_else(|| {
                SetupError::Config(format!(
                    "person.admin is set, but {} holds no person key (run `resonator person \
                     init` or `resonator person recover`)",
                    store.path().display()
                ))
            })?;
            let key = PersonKey::from_phrase(&phrase);
            // The admin certificate pairing gave, if any (it keeps the
            // name given there); else one issued now.
            let paired = std::fs::read_to_string(config.device_cert_path())
                .ok()
                .and_then(|text| cert_from_hex(&text).ok())
                .filter(|cert| {
                    cert.admin && cert.person == key.public() && cert.device == device.public()
                });
            match paired {
                Some(cert) => cert,
                None => crate::person::verify_cert(&key.issue_cert(
                    &device.public(),
                    &settings.device_name,
                    true,
                    SystemClock.now_millis(),
                ))?,
            }
        } else {
            let text = if settings.cert.is_empty() {
                let path = config.device_cert_path();
                std::fs::read_to_string(&path).map_err(|error| {
                    SetupError::Config(format!(
                        "person mode needs this device's certificate in person.cert or {} \
                         (from `resonator person cert {}` on an admin device): {error}",
                        path.display(),
                        device.key_domain()
                    ))
                })?
            } else {
                settings.cert.clone()
            };
            cert_from_hex(&text)?
        };
        if cert.device != device.public() {
            return Err(SetupError::Config(format!(
                "the device certificate is for {}, not this device's key {}",
                cert.device.to_z32(),
                device.key_domain()
            )));
        }
        let domain = if settings.domain.is_empty() {
            key_domain(&cert.person)
        } else {
            settings.domain.clone()
        };
        match parse_key_domain(&domain) {
            Ok(Some(key)) if key != cert.person => {
                return Err(SetupError::Config(format!(
                    "person.domain {domain} is another person's key than the certificate's {}",
                    cert.person.to_z32()
                )));
            }
            Ok(_) => {}
            Err(error) => return Err(SetupError::Config(error.to_string())),
        }
        let holder = if settings.holder.is_empty() {
            None
        } else {
            let holder = parse_key_domain(&settings.holder)
                .ok()
                .flatten()
                .ok_or_else(|| {
                    SetupError::Config(format!(
                        "person.holder '{}' is not a device key",
                        settings.holder
                    ))
                })?;
            (holder != device.public()).then_some(holder)
        };
        Ok(Some(PersonSetup {
            domain,
            cert,
            holder,
            anchor: settings.anchor,
        }))
    }
}

impl std::fmt::Debug for FederationSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FederationSetup")
            .field("key", &self.key)
            .field("tuning", &self.tuning)
            .finish_non_exhaustive()
    }
}

/// A static peer of the configuration, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticPeer {
    pub domain: String,
    pub key: PublicKey,
    pub addr: std::net::SocketAddr,
}

impl StaticPeer {
    pub fn from_config(peer: &FederationPeer) -> Result<Self, String> {
        let invalid = |what: &str| format!("federation.peers: {what}");
        let from_key = if peer.key.is_empty() {
            None
        } else {
            Some(
                parse_key_domain(&peer.key)
                    .ok()
                    .flatten()
                    .ok_or_else(|| invalid(&format!("'{}' is not a server key", peer.key)))?,
            )
        };
        let from_domain = parse_key_domain(&peer.domain).ok().flatten();
        // A key domain with another key: a device of the person whose
        // key the domain is (checked by its certificate when it connects).
        let key = match (from_key, from_domain) {
            (Some(key), _) | (None, Some(key)) => key,
            (None, None) => {
                return Err(invalid(&format!(
                    "{} is not a key domain, so it needs a key",
                    peer.domain
                )));
            }
        };
        let domain = if peer.domain.is_empty() {
            key_domain(&key)
        } else {
            peer.domain.clone()
        };
        let addr = peer
            .addr
            .parse()
            .map_err(|_| invalid(&format!("addr '{}' is not a host:port", peer.addr)))?;
        Ok(StaticPeer { domain, key, addr })
    }
}

impl FederationSetup {
    /// Loads the server key if federation is enabled; `None` if it is not,
    /// in which case nothing is created or changed.
    pub fn load(config: &Config) -> Result<Option<Self>, SetupError> {
        if !config.federation.enabled {
            return Ok(None);
        }
        EndpointOptions::from_config(&config.federation).map_err(SetupError::Config)?;
        let key = ServerKey::load_or_create(&config.federation_key_path())?;
        let person = PersonSetup::load(config, &key)?;
        if let Some(person) = &person
            && person.domain != config.domain
        {
            return Err(SetupError::Config(format!(
                "in person mode the domain is the person domain {}, not {} (see \
                 FederationSetup::person_domain)",
                person.domain, config.domain
            )));
        }
        match parse_key_domain(&config.domain) {
            Ok(Some(_)) if person.is_some() => {}
            Ok(Some(domain_key)) if domain_key != key.public() => {
                return Err(SetupError::Config(format!(
                    "domain {} is a key domain, but not that of this server's key {}",
                    config.domain,
                    key.key_domain()
                )));
            }
            Ok(_) => {}
            Err(error) => return Err(SetupError::Config(error.to_string())),
        }
        for peer in &config.federation.peers {
            StaticPeer::from_config(peer).map_err(SetupError::Config)?;
        }
        let persons = PersonBook::new();
        let epoch_authority: Arc<dyn EpochAuthority> = if person.is_some() {
            Arc::new(PersonDevices::new(Arc::clone(&persons)))
        } else {
            Arc::new(WaveletDomainOnly)
        };
        let mut tuning = Tuning::default();
        let secs = |secs: u64, default: Duration| {
            if secs == 0 {
                default
            } else {
                Duration::from_secs(secs)
            }
        };
        tuning.lease_renew_anchor = secs(
            config.person.lease_renew_anchor_secs,
            tuning.lease_renew_anchor,
        );
        tuning.lease_renew = secs(config.person.lease_renew_secs, tuning.lease_renew);
        Ok(Some(FederationSetup {
            config: config.clone(),
            key,
            tuning,
            resolver: None,
            events: None,
            app: None,
            epoch_authority,
            person,
            persons,
            rendezvous: crate::control::config_rendezvous(config),
        }))
    }

    /// Person mode: the person domain, which must be the server's
    /// `domain` (set it before the wave server is created). `None` if
    /// person mode is off. Loads (or creates) the device key, and reads
    /// the person key or the device certificate.
    pub fn person_domain(config: &Config) -> Result<Option<String>, SetupError> {
        if !config.federation.enabled || !config.person.enabled {
            return Ok(None);
        }
        let key = ServerKey::load_or_create(&config.federation_key_path())?;
        Ok(PersonSetup::load(config, &key)?.map(|person| person.domain))
    }

    /// Person mode: sets `config.domain` to the person domain (see
    /// [`FederationSetup::person_domain`]).
    pub fn configure_person(config: &mut Config) -> Result<(), SetupError> {
        if let Some(domain) = Self::person_domain(config)? {
            config.domain = domain;
        }
        Ok(())
    }

    /// What this node knows of persons, shared with the trust rules (for
    /// a [`PersonDevices`] epoch authority).
    pub fn persons(&self) -> &Arc<PersonBook> {
        &self.persons
    }

    /// Person mode: this device's certificate.
    pub fn device_cert(&self) -> Option<&VerifiedCert> {
        self.person.as_ref().map(|person| &person.cert)
    }

    /// Person mode: where the person's wavelets are hosted, for
    /// [`WaveServer::set_person_hosting`].
    pub fn person_hosting(&self) -> Option<PersonHosting> {
        // Until the host lease is known: the configured holder, if any;
        // else nobody, and submissions wait.
        self.person.as_ref().map(|person| match person.holder {
            Some(holder) => PersonHosting {
                device: self.key.public().as_bytes().to_vec(),
                holder: Some(holder.as_bytes().to_vec()),
            },
            None => PersonHosting::unknown(self.key.public().as_bytes().to_vec()),
        })
    }

    /// Replaces the timing and size limits (tests shorten the backoff).
    pub fn with_tuning(mut self, tuning: Tuning) -> Self {
        self.tuning = tuning;
        self
    }

    /// Replaces the system DNS resolver for `_resonator` TXT records.
    pub fn with_resolver(mut self, resolver: Arc<dyn TxtResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// Replaces where pairing codes are published and looked up (tests
    /// share a [`crate::pair_code::MemoryRendezvous`]).
    pub fn with_rendezvous(mut self, rendezvous: Arc<dyn crate::pair_code::Rendezvous>) -> Self {
        self.rendezvous = Some(rendezvous);
        self
    }

    /// Decides which servers may host which wavelets in later epochs
    /// (default: [`WaveletDomainOnly`]).
    pub fn with_epoch_authority(mut self, authority: Arc<dyn EpochAuthority>) -> Self {
        self.epoch_authority = authority;
        self
    }

    pub fn key(&self) -> &ServerKey {
        &self.key
    }

    /// Starts listening to the wave server's updates now, before anything
    /// can write to it (call it right after the wave server is created and
    /// before the server serves): [`FederationSetup::start`] then fans out
    /// every delta applied in between. Without it, `start` subscribes
    /// itself, and deltas applied before are only caught up by the
    /// start-up check of the outbox.
    pub fn subscribe(&mut self, server: &WaveServer) {
        if self.person.is_some() {
            server.set_person_hosting(self.person_hosting());
        }
        if self.events.is_none() {
            let (_, events) = server.bus().subscribe_channel();
            self.events = Some(events);
        }
    }

    /// Joins federation to the HTTP side of the server: serves the
    /// attachments of its store to peers whose users take part in their
    /// wavelets, lets the store fetch attachments of other servers for
    /// local users (`fetch-attachment`), and answers `/federation/probe`
    /// (`probe`). Without this, none of these happen.
    pub fn serve_app(&mut self, state: &AppState) {
        // Device mode: `/session` names this device by its key.
        state.set_device_key(self.key.key_domain());
        self.app = Some(state.clone());
    }

    /// The signer to put into the wave server's options.
    pub fn delta_signer(&self) -> SharedDeltaSigner {
        SharedDeltaSigner::new(Ed25519DeltaSigner::new(self.key.secret().clone()))
    }

    /// Records this server's signer info, starts the endpoint, the
    /// fan-out and its outbox worker, and makes the wave server forward
    /// submissions to remote wavelets.
    pub async fn start(self, server: &WaveServer) -> Result<Federation, SetupError> {
        let config = &self.config;
        let db = server.database();
        let store = db.federation_store();
        server.load_epochs().await?;
        let info = match &self.person {
            Some(person) => device_signer_info(
                &self.key.public(),
                &config.domain,
                person.cert.signed.clone(),
            ),
            None => signer_info(&self.key.public(), &config.domain, Vec::new()),
        };
        store.put_signer_info(signer_info_record(&info)?).await?;
        let person_hosting = self.person_hosting();
        let resolver: Arc<dyn TxtResolver> = match self.resolver {
            Some(resolver) => resolver,
            None => Arc::new(
                HickoryTxtResolver::from_system().map_err(|e| SetupError::Dns(e.to_string()))?,
            ),
        };
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let anchor = self
            .person
            .as_ref()
            .is_some_and(|person| crate::control::saved_anchor(config).unwrap_or(person.anchor));
        let leases = crate::lease::Leases::new(anchor);
        let trust = Trust::new(
            store,
            resolver,
            Policy::from_config(&config.federation),
            [config.domain.clone()],
            Arc::clone(&clock),
        )
        .with_persons(Arc::clone(&self.persons))
        .with_leases(Arc::clone(&leases))
        .with_directory(
            self.rendezvous
                .clone()
                .filter(|_| config.federation.device_directory)
                .map(crate::directory::Directory::new),
        );
        let config_error = |e: crate::trust::TrustError| SetupError::Config(e.to_string());
        trust.load_co_hosts().await.map_err(config_error)?;
        if let Some(person) = &self.person {
            self.persons.set_own(person.cert.person);
            trust.load_persons().await.map_err(config_error)?;
            trust
                .learn_cert(&person.cert.signed)
                .await
                .map_err(config_error)?;
            if trust.is_revoked(&self.key.public()) {
                tracing::warn!("this device has been revoked by its person");
            }
            leases.set_own(person.cert.person, config.domain.clone());
            trust.load_leases().await.map_err(config_error)?;
            server.set_person_hosting(person_hosting.clone());
            if let Some(holder) = person.holder {
                self.persons.set_route(&config.domain, holder);
            }
        }
        let peers = config
            .federation
            .peers
            .iter()
            .map(StaticPeer::from_config)
            .collect::<Result<Vec<_>, _>>()
            .map_err(SetupError::Config)?;
        for peer in &peers {
            // Devices of a person: a key domain listed with another key, a
            // domain listed with several keys, or this node's own domain.
            let device = parse_key_domain(&peer.domain)
                .ok()
                .flatten()
                .is_some_and(|key| key != peer.key)
                || peers
                    .iter()
                    .any(|other| other.domain == peer.domain && other.key != peer.key)
                || peer.domain == config.domain;
            if device {
                self.persons.add_static_device(&peer.domain, peer.key);
                if peer.domain != config.domain {
                    self.persons.route_if_none(&peer.domain, peer.key);
                }
                continue;
            }
            trust
                .pin_configured(&peer.domain, &peer.key)
                .await
                .map_err(config_error)?;
        }
        let push = if config.push.enabled() {
            let push = crate::push::Push::new(
                config.push.clone(),
                self.key.secret().clone(),
                db.federation_store(),
                Arc::clone(&clock),
                self.tuning.clone(),
            );
            push.load().await?;
            Some(push)
        } else {
            None
        };
        let ctx = Arc::new(
            Context::new(
                server.clone(),
                config.domain.clone(),
                trust.clone(),
                clock,
                self.tuning.clone(),
                info,
                self.key.secret().clone(),
                // Co-hosts of a person may host its wavelets too.
                Arc::new(crate::cohost::CoHostAware {
                    inner: Arc::clone(&self.epoch_authority),
                    grants: Arc::clone(trust.co_hosts()),
                    persons: Arc::clone(&self.persons),
                }),
            )
            .with_person_mode(self.person.is_some())
            .with_push(push.clone()),
        );
        let mut capabilities: Vec<String> = CAPABILITIES.iter().map(|c| (*c).to_owned()).collect();
        if self.person.is_some() {
            capabilities.push("revocations".to_owned());
            capabilities.push("lease".to_owned());
            capabilities.push("sync-summary".to_owned());
        }
        let identity = LocalIdentity {
            domains: vec![config.domain.clone()],
            capabilities,
            rotations: Vec::new(),
        };
        let handler = ServerHandler {
            accounts: db.account_store(),
            domain: config.domain.clone(),
            ctx: Some(Arc::clone(&ctx)),
            attachments: self.app.as_ref().map(|app| app.attachments.clone()),
        };
        let options =
            EndpointOptions::from_config(&config.federation).map_err(SetupError::Config)?;
        let endpoint = FederationEndpoint::bind_device(
            self.key.secret().clone(),
            options,
            identity,
            self.person
                .as_ref()
                .map(|person| person.cert.signed.clone()),
            Arc::new(trust.clone()),
            Arc::new(handler),
        )
        .await?;
        for peer in &peers {
            endpoint.add_peer_addr(EndpointAddr::new(peer.key).with_ip_addr(peer.addr));
        }
        // Devices met by pairing: their addresses, and in person mode the
        // person's devices among them.
        let own_person = self.person.as_ref().map(|person| person.cert.person);
        for (device, addr, _) in ctx.store.device_addrs().await? {
            let Some(key) = <[u8; 32]>::try_from(device.as_slice())
                .ok()
                .and_then(|bytes| PublicKey::from_bytes(&bytes).ok())
            else {
                continue;
            };
            if let Ok(addr) = addr.parse::<std::net::SocketAddr>() {
                endpoint.add_peer_addr(EndpointAddr::new(key).with_ip_addr(addr));
            }
            if let Some(person) = own_person
                && key != self.key.public()
                && self.persons.cert(&key, &person).is_some()
                && !self.persons.is_revoked_by(&person, &key)
            {
                self.persons.add_static_device(&config.domain, key);
            }
        }
        // Where this device is, for invitations made on the command line.
        if self.person.is_some() {
            let now = ctx.now();
            for socket in endpoint.addr().ip_addrs() {
                let _ = ctx
                    .store
                    .put_device_addr(
                        self.key.public().as_bytes().to_vec(),
                        socket.to_string(),
                        now,
                    )
                    .await;
            }
        }
        ctx.set_static_peers(peers.iter().map(|peer| peer.key).collect());
        ctx.set_endpoint(Some(endpoint.clone()));
        let inviter = self.person.as_ref().map(|_| {
            let account = if config.is_device() {
                config.device.account.clone()
            } else {
                String::new()
            };
            Arc::new(
                crate::pairing::Inviter::new(
                    Arc::new(crate::control::NodeInviterHost {
                        ctx: Arc::clone(&ctx),
                    }),
                    config.person_key_path(),
                    account,
                )
                .with_rendezvous(self.rendezvous.clone()),
            )
        });
        if let Some(inviter) = &inviter {
            endpoint.set_pair_service(Arc::clone(inviter) as Arc<dyn crate::endpoint::PairService>);
        }
        let ops = Arc::new(crate::control::PersonOps {
            ctx: Arc::clone(&ctx),
            config: config.clone(),
            inviter,
            rendezvous: self.rendezvous.clone(),
            join_stage: std::sync::Mutex::new(None),
        });
        if let Some(app) = &self.app {
            if !app
                .attachments
                .set_origin(Arc::new(RemoteOrigin::new(Arc::clone(&ctx))))
            {
                tracing::warn!("the attachment store already fetches from other servers");
            }
            // Person mode: attachments uploaded here are copied to the
            // person's other devices, and theirs here.
            if self.person.is_some() && config.federation.replicate_own_attachments {
                crate::copies::Copies::install(
                    &ctx,
                    Arc::new(crate::copies::Copies::new(
                        app.attachments.clone(),
                        config.federation.replicate_max_bytes(),
                    )),
                );
            }
            if !app.set_remote_probe(Arc::new(Prober {
                ctx: Arc::clone(&ctx),
            })) {
                tracing::warn!("remote addresses are already probed");
            }
            if !app.set_person_control(Arc::new(crate::control::HttpControl {
                ops: Arc::clone(&ops),
                app: app.clone(),
            })) {
                tracing::warn!("the person settings are already answered");
            }
        }
        if !server.set_remote_host(Arc::new(Forwarder {
            ctx: Arc::clone(&ctx),
        })) {
            tracing::warn!("the wave server already forwards to remote hosts");
        }
        let cancel = CancellationToken::new();
        let tasks = TaskTracker::new();
        let events = match self.events {
            Some(events) => events,
            None => server.bus().subscribe_channel().1,
        };
        tasks.spawn(outbox::fan_out(Arc::clone(&ctx), events, cancel.clone()));
        tasks.spawn(outbox::reconcile(Arc::clone(&ctx)));
        tasks.spawn(outbox::worker(Arc::clone(&ctx), cancel.clone()));
        tasks.spawn(prune_applied(Arc::clone(&ctx), cancel.clone()));
        tasks.spawn(crate::epoch::orphan_worker(
            Arc::clone(&ctx),
            cancel.clone(),
        ));
        // These wait on peers mid-step: dropped at once at shutdown.
        let until_cancelled = |cancel: CancellationToken, task| async move {
            tokio::select! {
                () = cancel.cancelled() => {}
                () = task => {}
            }
        };
        tasks.spawn(until_cancelled(
            cancel.clone(),
            Box::pin(crate::sync::drain_pending(Arc::clone(&ctx), cancel.clone()))
                as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
        ));
        // Co-hosting for persons that granted it to this domain.
        tasks.spawn(until_cancelled(
            cancel.clone(),
            Box::pin(crate::cohost::run(Arc::clone(&ctx), cancel.clone())),
        ));
        if let Some(push) = &push {
            tasks.spawn(until_cancelled(
                cancel.clone(),
                Box::pin(crate::push::run(
                    Arc::clone(&ctx),
                    Arc::clone(push),
                    cancel.clone(),
                )),
            ));
        }
        if self.person.is_some() {
            if crate::control::recovery_marker_path(config).exists() {
                ctx.leases.hold_back(true);
                tasks.spawn(until_cancelled(
                    cancel.clone(),
                    Box::pin(Arc::clone(&ops).finish_recovery(cancel.clone())),
                ));
            }
            tasks.spawn(revocation_watcher(Arc::clone(&ctx), cancel.clone()));
            if let Some(directory) = trust.directory() {
                tasks.spawn(until_cancelled(
                    cancel.clone(),
                    Box::pin(crate::directory::publish_task(
                        Arc::clone(&ctx),
                        Arc::clone(directory),
                        config.person_key_path(),
                        cancel.clone(),
                    )),
                ));
            }
            tasks.spawn(until_cancelled(
                cancel.clone(),
                Box::pin(crate::lease::run(
                    Arc::clone(&ctx),
                    person_hosting.clone(),
                    cancel.clone(),
                )),
            ));
            tasks.spawn(until_cancelled(
                cancel.clone(),
                Box::pin(crate::sync::run(Arc::clone(&ctx), cancel.clone())),
            ));
        }
        tasks.close();
        tracing::info!(
            key = %self.key.key_domain(),
            domain = %config.domain,
            mode = ?config.federation.mode,
            peers = peers.len(),
            push = push.is_some(),
            "federation enabled"
        );
        Ok(Federation {
            endpoint,
            trust,
            ctx,
            cancel,
            tasks,
            ops,
        })
    }
}

async fn prune_applied(ctx: Arc<Context>, cancel: CancellationToken) {
    loop {
        let retention = i64::try_from(APPLIED_RETENTION.as_millis()).unwrap_or(i64::MAX);
        match ctx
            .store
            .prune_applied(ctx.now().saturating_sub(retention))
            .await
        {
            Ok(0) => {}
            Ok(pruned) => tracing::debug!(pruned, "forgot old federation request ids"),
            Err(error) => tracing::warn!(%error, "cannot prune federation request ids"),
        }
        tokio::select! {
            () = cancel.cancelled() => break,
            () = tokio::time::sleep(APPLIED_PRUNE_INTERVAL) => {}
        }
    }
}

/// Person mode: cuts off devices as their persons revoke them and passes
/// new revocation lists on, whether they came from a peer, from
/// [`Federation::publish_revocations`] or from storage (the
/// `person revoke` command writes there while the server runs).
async fn revocation_watcher(ctx: Arc<Context>, cancel: CancellationToken) {
    let persons = Arc::clone(ctx.trust.persons());
    // What is known at start has been spread before.
    for list in persons.all_revocations() {
        lock_pushed(&ctx).insert(*list.person.as_bytes(), list.seq);
    }
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            () = persons.revocations_changed() => {}
            () = tokio::time::sleep(ctx.tuning.revocation_poll) => {
                if let Err(error) = ctx.trust.load_persons().await {
                    tracing::warn!(%error, "cannot read revocation lists");
                }
            }
        }
        spread_revocations(&ctx).await;
    }
}

fn lock_pushed(
    ctx: &Context,
) -> std::sync::MutexGuard<'_, std::collections::HashMap<[u8; 32], u64>> {
    crate::context::lock(&ctx.revocations_spread)
}

/// Closes the connections of devices revoked by lists not spread yet, and
/// tells every peer this node knows of these lists.
pub(crate) async fn spread_revocations(ctx: &Context) {
    let Ok(endpoint) = ctx.endpoint() else {
        return;
    };
    let persons = ctx.trust.persons();
    let fresh: Vec<_> = {
        let mut pushed = lock_pushed(ctx);
        persons
            .all_revocations()
            .into_iter()
            .filter(|list| {
                let seen = pushed.entry(*list.person.as_bytes()).or_insert(0);
                if list.seq > *seen {
                    *seen = list.seq;
                    true
                } else {
                    false
                }
            })
            .collect()
    };
    if fresh.is_empty() {
        return;
    }
    let own = ctx.own_key();
    let mut keys = endpoint.connected_peers();
    for key in persons
        .static_devices()
        .into_iter()
        .chain(persons.certified_devices())
        .chain(crate::context::lock(&ctx.static_peers).clone())
    {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys.retain(|key| *key != own && !persons.is_revoked(key));
    for list in fresh {
        let revoked = list.devices();
        endpoint.disconnect(&revoked).await;
        tracing::info!(
            person = %list.person.to_z32(),
            seq = list.seq,
            peers = keys.len(),
            "spreading a revocation list"
        );
        let mut pushes = tokio::task::JoinSet::new();
        for key in keys.iter().copied() {
            let endpoint = endpoint.clone();
            let op = federation_request::Op::Revocations(
                resonator_model::proto::federation_rpc::Revocations {
                    person: Some(list.person.as_bytes().to_vec()),
                    list: Some(list.signed.clone()),
                },
            );
            pushes.spawn(async move {
                if let Err(error) = endpoint.request(key, None, op).await {
                    tracing::debug!(peer = %key.fmt_short(), %error, "revocation list not delivered");
                }
            });
        }
        while pushes.join_next().await.is_some() {}
    }
}

/// Another device of this device's person, as [`Federation::own_devices`]
/// lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnDevice {
    pub key: PublicKey,
    /// It said it is an anchor (an always-on device preferred as holder).
    pub anchor: bool,
    /// It answered the lease task at its last look.
    pub reachable: bool,
}

/// Federation running alongside the wave server.
pub struct Federation {
    endpoint: FederationEndpoint,
    trust: Trust,
    ctx: Arc<Context>,
    cancel: CancellationToken,
    tasks: TaskTracker,
    ops: Arc<crate::control::PersonOps>,
}

impl std::fmt::Debug for Federation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Federation")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl Federation {
    pub fn endpoint(&self) -> &FederationEndpoint {
        &self.endpoint
    }

    pub fn trust(&self) -> &Trust {
        &self.trust
    }

    /// Person mode's operations for people: pairing, the device list,
    /// revoking, co-hosts, the anchor setting.
    pub fn person(&self) -> &crate::control::PersonOps {
        &self.ops
    }

    /// Person mode: records a new revocation list of a person (signed by
    /// the person key, e.g. [`PersonKey::revoke`]), cuts off the devices
    /// it revokes and tells every peer this node knows. Returns whether
    /// the list was newer than the one known.
    pub async fn publish_revocations(
        &self,
        list: &resonator_model::proto::federation_rpc::SignedRevocationList,
    ) -> Result<bool, crate::trust::TrustError> {
        let learned = self.trust.learn_revocations(list).await?.is_some();
        spread_revocations(&self.ctx).await;
        Ok(learned)
    }

    /// Asks the server or device at `key` for its newest revocation list
    /// of `person`, and records it if it is newer. Returns whether it was.
    pub async fn fetch_revocations(
        &self,
        key: PublicKey,
        person: PublicKey,
    ) -> Result<bool, RequestError> {
        let op = federation_request::Op::Revocations(
            resonator_model::proto::federation_rpc::Revocations {
                person: Some(person.as_bytes().to_vec()),
                list: None,
            },
        );
        let federation_response::Result::Revocations(result) =
            self.endpoint.request(key, None, op).await?
        else {
            return Ok(false);
        };
        let Some(list) = result.list else {
            return Ok(false);
        };
        let learned = self
            .trust
            .learn_revocations(&list)
            .await
            .map_err(|error| RequestError::Protocol {
                peer: key.fmt_short().to_string(),
                message: error.to_string(),
            })?
            .is_some();
        spread_revocations(&self.ctx).await;
        Ok(learned)
    }

    /// Sends a request to the server of `domain`, which must be authorised
    /// for it. For diagnostics and tests.
    pub async fn request(
        &self,
        domain: &str,
        op: federation_request::Op,
    ) -> Result<federation_response::Result, RequestError> {
        self.ctx.request(domain, None, op).await
    }

    /// [`Federation::request`] with a given request id.
    pub async fn request_with_id(
        &self,
        domain: &str,
        id: String,
        op: federation_request::Op,
    ) -> Result<federation_response::Result, RequestError> {
        self.ctx.request(domain, Some(id), op).await
    }

    /// The notifications still to deliver or delivered, by domain and
    /// wavelet.
    pub async fn outbox(&self) -> Result<Vec<OutboxEntry>, StorageError> {
        self.ctx.store.outbox_entries().await
    }

    /// Holds or releases the outbox worker; held, notifications queue up
    /// but are not sent. For tests and maintenance.
    /// Makes this server the host of a wavelet from its current version
    /// on, in `epoch` (higher than any known), and tells the other
    /// servers; returns the epoch's start. The epoch authority of every
    /// server involved must let this server host the wavelet.
    pub async fn take_over(
        &self,
        wavelet_name: &WaveletName,
        epoch: u64,
    ) -> Result<HashedVersion, FederationError> {
        crate::epoch::take_over(&self.ctx, wavelet_name, epoch).await
    }

    /// Person mode: the host lease of this device's person as this device
    /// sees it.
    pub fn lease_state(&self) -> LeaseState {
        self.ctx.leases.state(&self.ctx.own_key(), self.ctx.now())
    }

    /// Person mode: the newest host lease known of this device's person.
    pub fn lease(&self) -> Option<VerifiedLease> {
        self.ctx.leases.own_current()
    }

    /// The newest host lease known of `person` (any person: a co-host
    /// holds others').
    pub fn lease_of(&self, person: &PublicKey) -> Option<VerifiedLease> {
        self.ctx.leases.current(person)
    }

    /// Person mode: hands the host lease this device holds over to
    /// another device of the person, `to`. Returns once `to` has the
    /// signed handover; it then catches up with this device, claims the
    /// next term and takes the person's wavelets over (this device steps
    /// down when it learns of the new lease).
    pub async fn handover(&self, to: PublicKey) -> Result<(), LeaseError> {
        crate::lease::hand_over(&self.ctx, to).await
    }

    /// Person mode: compares and syncs with another device of the person
    /// now; returns how many wavelets moved on here.
    pub async fn sync_with(&self, device: PublicKey) -> Result<usize, RequestError> {
        crate::sync::exchange(&self.ctx, device).await
    }

    /// Sends the pending submissions now; returns how many were answered.
    pub async fn drain_pending(&self) -> usize {
        self.ctx.server.drain_pending().await
    }

    /// Person mode: the other devices of this device's person, with what
    /// the lease task last learned of them; anchors first, then devices
    /// that answered at the last look.
    pub fn own_devices(&self) -> Vec<OwnDevice> {
        let anchors = self.ctx.leases.known_anchors();
        let reachable = self.ctx.leases.reachable();
        let mut devices: Vec<OwnDevice> = crate::lease::own_devices(&self.ctx)
            .into_iter()
            .map(|key| OwnDevice {
                anchor: anchors.contains(&key),
                reachable: reachable.contains(&key),
                key,
            })
            .collect();
        devices.sort_by_key(|device| (!device.anchor, !device.reachable));
        devices
    }

    /// Wakes the lease task to look at the lease again now (after the app
    /// was suspended, for instance).
    pub fn recheck_lease(&self) {
        self.ctx.leases.changed.notify_one();
    }

    /// Tells the endpoint that the network may have changed (the app comes
    /// back from the background, the device moved to another network), so
    /// that it looks for its addresses and paths again.
    pub async fn network_change(&self) {
        self.endpoint.endpoint().network_change().await;
    }

    /// Adopts epoch statements about a wavelet (as if a peer had sent
    /// them); a server that hosted an earlier epoch steps down.
    pub async fn adopt_epochs(
        &self,
        wavelet_name: &WaveletName,
        statements: &[SignedEpochStart],
    ) -> Result<(), FederationError> {
        crate::epoch::adopt(&self.ctx, wavelet_name, statements)
            .await
            .map(|_| ())
    }

    /// Resubmits pending orphaned deltas now.
    pub fn resubmit_orphans(&self) {
        self.ctx.orphans_wake.notify_one();
    }

    /// Push (`[push] relay` set): this device's push token as the app
    /// got it (`None`: it has none). It is sealed by the relay and the
    /// sealed token advertised in the background; without push, nothing
    /// happens.
    pub fn set_push_token(&self, token: Option<crate::push::RawPushToken>) {
        if let Some(push) = &self.ctx.push {
            push.set_token(token);
        }
    }

    /// Push, if `[push] relay` is set.
    pub fn push(&self) -> Option<&Arc<crate::push::Push>> {
        self.ctx.push.as_ref()
    }

    pub fn hold_outbox(&self, hold: bool) {
        self.ctx.outbox_paused.store(hold, Ordering::SeqCst);
        self.ctx.outbox_wake.notify_one();
    }

    /// Stops the fan-out and the endpoint; call before the wave server
    /// shuts down. Undelivered notifications stay in the outbox.
    pub async fn shutdown(self) {
        self.cancel.cancel();
        self.ctx.outbox_wake.notify_one();
        if tokio::time::timeout(Duration::from_secs(5), self.tasks.wait())
            .await
            .is_err()
        {
            tracing::warn!("federation tasks still running at shutdown");
        }
        crate::copies::Copies::uninstall(&self.ctx);
        self.endpoint.shutdown().await;
        self.ctx.set_endpoint(None);
    }
}

/// Answers federation requests for a wave server.
pub struct ServerHandler {
    accounts: AccountStore,
    domain: String,
    ctx: Option<Arc<Context>>,
    attachments: Option<AttachmentService>,
}

impl ServerHandler {
    /// A handler that answers `probe` and `echo` only.
    pub fn new(accounts: AccountStore, domain: impl Into<String>) -> Self {
        ServerHandler {
            accounts,
            domain: domain.into(),
            ctx: None,
            attachments: None,
        }
    }

    /// Answers a request that changes something once per request id: a
    /// retry gets the first answer again.
    ///
    /// Exactly once: the request id is claimed before anything is applied,
    /// so a copy arriving while the first is being applied waits for it and
    /// gets its answer. A delta submitted is recorded as applied in the
    /// same transaction as the delta, and the claim is released only once
    /// that is durable, so neither a concurrent copy nor a retry after a
    /// crash applies it again. `update`, `commit` and `post-signer-info`
    /// are recorded after they are applied; applying one of them twice
    /// changes nothing.
    async fn once(
        &self,
        ctx: &Context,
        peer: &Peer,
        request_id: &str,
        op: federation_request::Op,
    ) -> federation_response::Result {
        if request_id.is_empty() {
            return self.apply(ctx, peer, request_id, op).await.0;
        }
        let claim = ctx.claim(request_id);
        let (domain, result) = claim
            .get_or_init(|| async move {
                let first_domain = peer.domains.first().cloned().unwrap_or_default();
                match ctx.store.applied_request(request_id).await {
                    Ok(Some((domain, response))) => {
                        let result = response
                            .and_then(|bytes| decode::<FederationResponse>(&bytes).ok())
                            .and_then(|response| response.result);
                        if let Some(result) = result {
                            tracing::debug!(%request_id, "answered a repeated federation request");
                            return (domain, result);
                        }
                        // Recorded without an answer: applied, but what
                        // it answered is lost; applying again is wrong.
                        return (
                            domain,
                            error(Code::UnexpectedRequest, "request already applied"),
                        );
                    }
                    Ok(None) => {}
                    Err(e) => {
                        return (
                            first_domain,
                            error(Code::InternalServerError, e.to_string()),
                        );
                    }
                }
                let (result, recorded) = self.apply(ctx, peer, request_id, op).await;
                if !recorded && !matches!(result, federation_response::Result::Error(_)) {
                    let response = FederationResponse {
                        id: Some(request_id.to_owned()),
                        result: Some(result.clone()),
                    };
                    if let Ok(bytes) = encode(&response)
                        && let Err(error) = ctx
                            .store
                            .record_applied(request_id, &first_domain, ctx.now(), Some(bytes))
                            .await
                    {
                        tracing::warn!(%error, "cannot record an applied federation request");
                    }
                }
                (first_domain, result)
            })
            .await
            .clone();
        ctx.release(request_id, &claim);
        if !peer.speaks_for(&domain) {
            return error(Code::UnexpectedRequest, "request id already used");
        }
        result
    }

    /// Applies a request that changes something; returns the answer and
    /// whether it was recorded as applied already (with the delta).
    async fn apply(
        &self,
        ctx: &Context,
        peer: &Peer,
        request_id: &str,
        op: federation_request::Op,
    ) -> (federation_response::Result, bool) {
        match op {
            federation_request::Op::Submit(request) => {
                let receipt = (!request_id.is_empty()).then(|| {
                    let id = request_id.to_owned();
                    SubmitReceipt {
                        request_id: id.clone(),
                        domain: peer.domains.first().cloned().unwrap_or_default(),
                        applied_at: ctx.now(),
                        response: Box::new(move |result| {
                            let response = FederationResponse {
                                id: Some(id),
                                result: Some(host::submit_result(result)),
                            };
                            encode(&response).ok()
                        }),
                    }
                });
                host::submit(ctx, peer, request, receipt).await
            }
            federation_request::Op::PostSignerInfo(request) => {
                (host::post_signer_info(ctx, peer, request).await, false)
            }
            federation_request::Op::Update(request) => {
                (replica::update(ctx, peer, request).await, false)
            }
            federation_request::Op::Commit(request) => {
                (replica::commit(ctx, peer, request).await, false)
            }
            federation_request::Op::Resubmit(request) => {
                (crate::epoch::resubmit(ctx, peer, request).await, false)
            }
            other => (default_response(other), false),
        }
    }
}

#[async_trait]
impl RequestHandler for ServerHandler {
    async fn handle_with_body(
        &self,
        peer: &Peer,
        request_id: &str,
        op: federation_request::Op,
    ) -> (federation_response::Result, Option<Body>) {
        match (op, &self.ctx, &self.attachments) {
            (federation_request::Op::FetchAttachment(request), Some(ctx), Some(attachments)) => {
                attachments::serve(ctx, attachments, peer, request).await
            }
            (op, _, _) => (self.handle(peer, request_id, op).await, None),
        }
    }

    async fn handle(
        &self,
        peer: &Peer,
        request_id: &str,
        op: federation_request::Op,
    ) -> federation_response::Result {
        if let Some(ctx) = &self.ctx {
            match op {
                federation_request::Op::History(request) => {
                    return host::history(ctx, peer, request).await;
                }
                federation_request::Op::GetSignerInfo(request) => {
                    return host::signer_info(ctx, peer, request).await;
                }
                federation_request::Op::Lease(request) => {
                    return crate::lease::serve(ctx, peer, request).await;
                }
                federation_request::Op::CoHosts(request) => {
                    crate::cohost::serve(ctx, request).await;
                    return federation_response::Result::Ack(Default::default());
                }
                federation_request::Op::PushTargets(request) if ctx.push.is_some() => {
                    let push = ctx.push.as_ref().expect("checked");
                    return crate::push::serve(ctx, push, peer, request).await;
                }
                federation_request::Op::PersonInfo(request) => {
                    return crate::recovery::person_info(ctx, request);
                }
                federation_request::Op::Holdings(_) => {
                    return crate::recovery::holdings(ctx, peer).await;
                }
                federation_request::Op::SyncSummary(request) if ctx.person_mode => {
                    return crate::sync::serve(ctx, peer, request).await;
                }
                federation_request::Op::AttachmentCopies(request) if ctx.person_mode => {
                    return crate::copies::serve_copies(ctx, peer, request);
                }
                federation_request::Op::OwnAttachments(request)
                    if ctx.person_mode && self.attachments.is_some() =>
                {
                    let attachments = self.attachments.as_ref().expect("checked");
                    return crate::copies::serve_own(ctx, attachments, peer, request).await;
                }
                op @ (federation_request::Op::Submit(_)
                | federation_request::Op::PostSignerInfo(_)
                | federation_request::Op::Update(_)
                | federation_request::Op::Commit(_)
                | federation_request::Op::Resubmit(_)) => {
                    return self.once(ctx, peer, request_id, op).await;
                }
                _ => {}
            }
        }
        match op {
            federation_request::Op::Probe(probe) => {
                let exists = match ParticipantId::of(probe.address.as_deref().unwrap_or_default()) {
                    Ok(id) if id.domain() == self.domain => {
                        matches!(self.accounts.get_account(&id).await, Ok(Some(_)))
                    }
                    _ => false,
                };
                federation_response::Result::Probe(ProbeResult {
                    exists: Some(exists),
                })
            }
            other => default_response(other),
        }
    }
}
