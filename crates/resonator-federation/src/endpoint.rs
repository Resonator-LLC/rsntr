//! The federation transport: an iroh endpoint speaking ALPN
//! [`ALPN`] (`resonator/fed/1`).
//!
//! Choreography of a connection:
//!
//! 1. The dialer opens the first bidirectional stream and sends a request
//!    carrying its [`Hello`] (protocol version, domains, capabilities,
//!    key rotation statements), then finishes the stream. The acceptor
//!    checks which of the claimed domains the peer's key is bound to (the
//!    [`PeerAuthority`]), and answers with its own `Hello`, or with an
//!    error and closes the connection if none is.
//! 2. Every further request gets a stream of its own: the dialer writes
//!    one request frame and finishes, the acceptor writes one response
//!    frame and finishes. Requests carry a ULID.
//!
//! Requests only flow from dialer to acceptor on a connection; a server
//! that wants to send to a peer dials it (or reuses its cached outbound
//! connection). Outbound connections are cached per peer key.
//!
//! Person mode: a device's Hello carries its device certificate and the
//! `seq` of its person's newest revocation list it knows, with that list.
//! The answer to a Hello carries the revocation lists of the dialer's
//! person newer than the dialer said it knows. A peer whose key gets
//! revoked is cut off: its streams are refused and its connections closed
//! ([`FederationEndpoint::disconnect`]). The `revocations` request
//! (fetch or push a revocation list) is answered here, by the
//! [`PeerAuthority`].

use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use iroh::address_lookup::{DnsAddressLookup, MemoryLookup, PkarrPublisher, PkarrResolver};
use iroh::endpoint::{Connection, RecvStream, SendStream, VarInt, presets};
use iroh::{Endpoint, EndpointAddr, PublicKey, RelayMap, RelayMode, RelayUrl, SecretKey};
use resonator_model::proto::federation_error::{FederationError, federation_error::Code};
use resonator_model::proto::federation_rpc::{
    Chunk, FederationFrame, FederationRequest, FederationResponse, Hello, PROTOCOL_VERSION,
    Revocations, RevocationsResult, SignedDeviceCert, SignedKeyRotation, federation_frame,
    federation_request, federation_response,
};
use resonator_server::config::{FederationConfig, RelaySetting};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::codec::{FrameError, read_frame, write_frame};
use crate::trust::{Authorisation, Trust};

/// Most co-host grants one Hello carries.
const MAX_HELLO_CO_HOSTS: usize = 64;

/// The ALPN of the resonator federation protocol.
pub const ALPN: &[u8] = b"resonator/fed/1";

/// The ALPN of pairing ([`crate::pairing`]): a device without a
/// certificate asks an admin device of a person for one. No Hello; each
/// stream carries one `pair` request and its answer.
pub const PAIR_ALPN: &[u8] = b"resonator/pair/1";

/// Most requests one pairing connection may make.
const MAX_PAIR_REQUESTS: usize = 256;

/// Answers `pair` requests on [`PAIR_ALPN`] connections.
#[async_trait]
pub trait PairService: Send + Sync {
    /// Answers a `pair` request from the device with key `joiner`.
    async fn pair(
        &self,
        joiner: PublicKey,
        request: resonator_model::proto::federation_rpc::Pair,
    ) -> federation_response::Result;

    /// Answers a `pair-code` request (the PAKE of pairing by code) from
    /// the device with key `joiner`.
    async fn pair_code(
        &self,
        joiner: PublicKey,
        request: resonator_model::proto::federation_rpc::PairCode,
    ) -> federation_response::Result;
}

/// Application close code for a normal close.
const CLOSE_OK: u32 = 0;
/// Application close code for a peer that failed its Hello.
const CLOSE_REFUSED: u32 = 1;

/// How the endpoint reaches and is reached by others.
#[derive(Debug, Clone)]
pub struct EndpointOptions {
    pub relay: RelaySetting,
    /// Publish this endpoint's addresses and look up others' by key
    /// through iroh's DNS address lookup. Off: static addresses only (see
    /// [`FederationEndpoint::add_peer_addr`]).
    pub discovery: bool,
    /// UDP addresses to bind; empty for any port on all interfaces.
    pub bind: Vec<SocketAddr>,
    /// How long a request (or a Hello) may take to be answered.
    pub request_timeout: Duration,
    /// Requests served at the same time on one connection.
    pub max_concurrent_requests: usize,
}

impl EndpointOptions {
    pub fn from_config(config: &FederationConfig) -> Result<Self, String> {
        Ok(EndpointOptions {
            relay: config.relay_setting()?,
            discovery: config.discovery_enabled()?,
            bind: config.listen_addrs()?,
            request_timeout: Duration::from_secs(config.request_timeout_secs),
            max_concurrent_requests: 64,
        })
    }

    /// No relay, no discovery, bound to the loopback interface: for tests
    /// and same-host setups, with peers' addresses given explicitly.
    pub fn local() -> Self {
        EndpointOptions {
            relay: RelaySetting::Disabled,
            discovery: false,
            bind: vec![SocketAddr::from(([127, 0, 0, 1], 0))],
            request_timeout: Duration::from_secs(10),
            max_concurrent_requests: 64,
        }
    }
}

/// What this server says about itself in its Hello.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalIdentity {
    /// The wave domains this server hosts.
    pub domains: Vec<String>,
    pub capabilities: Vec<String>,
    /// Rotation statements chaining the server key to earlier keys of its
    /// domains.
    pub rotations: Vec<SignedKeyRotation>,
}

impl LocalIdentity {
    fn hello(&self) -> Hello {
        Hello {
            protocol_version: Some(PROTOCOL_VERSION),
            domain: self.domains.clone(),
            capability: self.capabilities.clone(),
            key_rotation: self.rotations.clone(),
            device_cert: None,
            revocation_seq: None,
            revocations: Vec::new(),
            host_lease: Vec::new(),
            co_host: Vec::new(),
        }
    }
}

/// Decides which domains a peer key may speak for.
///
/// The methods with defaults are person identity's: a plain server
/// knows no certificates or revocations.
#[async_trait]
pub trait PeerAuthority: Send + Sync {
    async fn authorise(
        &self,
        peer: &PublicKey,
        domain: &str,
        rotations: &[SignedKeyRotation],
    ) -> Authorisation;

    /// Takes in what a peer's Hello (or the answer to ours) carries
    /// before its domains are checked: its device certificate and
    /// revocation lists.
    async fn hello_received(&self, _peer: &PublicKey, _hello: &Hello) {}

    /// Completes this node's Hello (to `peer`, answering `their` Hello if
    /// this is an answer): revocation `seq`, revocation lists.
    fn complete_hello(&self, _hello: &mut Hello, _peer: &PublicKey, _their: Option<&Hello>) {}

    /// Whether `peer`'s key has been revoked.
    fn is_revoked(&self, _peer: &PublicKey) -> bool {
        false
    }

    /// Answers a `revocations` request.
    async fn revocations(&self, _peer: &PublicKey, _request: Revocations) -> RevocationsResult {
        RevocationsResult::default()
    }
}

#[async_trait]
impl PeerAuthority for Trust {
    async fn authorise(
        &self,
        peer: &PublicKey,
        domain: &str,
        rotations: &[SignedKeyRotation],
    ) -> Authorisation {
        match self.check(peer, domain, rotations).await {
            Ok(verdict) => verdict,
            Err(error) => {
                tracing::warn!(%domain, %error, "cannot check a federation peer");
                Authorisation::NotBound
            }
        }
    }

    async fn hello_received(&self, peer: &PublicKey, hello: &Hello) {
        if let Some(cert) = &hello.device_cert {
            match self.learn_cert(cert).await {
                Ok(cert) if cert.device != *peer => {
                    tracing::info!(peer = %short(peer), "a peer presented another device's certificate");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::info!(peer = %short(peer), %error, "a peer presented a bad device certificate");
                }
            }
        }
        for list in &hello.revocations {
            if let Err(error) = self.learn_revocations(list).await {
                tracing::info!(peer = %short(peer), %error, "a peer sent a bad revocation list");
            }
        }
        for grant in &hello.co_host {
            if let Err(error) = self.learn_co_host(grant).await {
                tracing::info!(peer = %short(peer), %error, "a peer sent a bad co-host grant");
            }
        }
        for lease in &hello.host_lease {
            if let Err(error) = self.learn_lease(lease).await {
                tracing::info!(peer = %short(peer), %error, "a peer sent a bad host lease");
            }
        }
        // Co-hosts step back once a device of the person is around.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
            });
        for cert in self.persons().certs_of(peer) {
            if !self.persons().is_revoked_by(&cert.person, peer) {
                self.co_hosts().note_seen(&cert.person, now);
                self.leases().note_device_seen(*peer, now);
            }
        }
        self.leases().hello_seen.notify_waiters();
    }

    fn complete_hello(&self, hello: &mut Hello, peer: &PublicKey, their: Option<&Hello>) {
        let persons = self.persons();
        if let Some(own) = persons.own() {
            hello.host_lease = crate::lease::hello_leases(&self.leases());
            hello.revocation_seq = Some(persons.revocation_seq(&own));
            if let Some(list) = persons.revocations(&own) {
                hello.revocations.push(list.signed);
            }
        }
        // Co-host grants, so that every server can check a co-host's
        // epochs and leases.
        hello.co_host = self
            .co_hosts()
            .all()
            .into_iter()
            .take(MAX_HELLO_CO_HOSTS)
            .map(|grant| grant.signed)
            .collect();
        // Lists of the peer's persons newer than the peer said it knows.
        let Some(their) = their else {
            return;
        };
        let known = their.revocation_seq.unwrap_or(0);
        for cert in persons.certs_of(peer) {
            if Some(cert.person) == persons.own() {
                continue;
            }
            if let Some(list) = persons.revocations(&cert.person)
                && list.seq > known
            {
                hello.revocations.push(list.signed);
            }
        }
    }

    fn is_revoked(&self, peer: &PublicKey) -> bool {
        Trust::is_revoked(self, peer)
    }

    async fn revocations(&self, peer: &PublicKey, request: Revocations) -> RevocationsResult {
        if let Some(list) = &request.list {
            match self.learn_revocations(list).await {
                Ok(Some(_)) => {
                    tracing::debug!(peer = %short(peer), "a peer pushed a newer revocation list");
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::info!(peer = %short(peer), %error, "a peer pushed a bad revocation list");
                }
            }
        }
        let person = request
            .person
            .as_deref()
            .and_then(|bytes| <&[u8; 32]>::try_from(bytes).ok())
            .and_then(|bytes| PublicKey::from_bytes(bytes).ok());
        RevocationsResult {
            list: person
                .and_then(|person| self.persons().revocations(&person))
                .map(|list| list.signed),
        }
    }
}

/// A [`PeerAuthority`] that needs no storage: a peer may speak for its own
/// key domain and nothing else. For tests and diagnostics.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyDomainAuthority;

#[async_trait]
impl PeerAuthority for KeyDomainAuthority {
    async fn authorise(
        &self,
        peer: &PublicKey,
        domain: &str,
        _: &[SignedKeyRotation],
    ) -> Authorisation {
        match crate::key::parse_key_domain(domain) {
            Ok(Some(key)) if key == *peer => Authorisation::Authorised,
            _ => Authorisation::NotBound,
        }
    }
}

/// A peer that has completed the Hello.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    /// The key proven by the handshake.
    pub key: PublicKey,
    /// The domains of its Hello that it is authorised for.
    pub domains: Vec<String>,
    /// Its Hello as received.
    pub hello: Hello,
}

impl Peer {
    pub fn speaks_for(&self, domain: &str) -> bool {
        self.domains.iter().any(|d| d == domain)
    }
}

/// Bytes that follow a response on its stream, sent as
/// [`Chunk`](resonator_model::proto::federation_rpc::Chunk) frames.
pub type Body = Pin<Box<dyn AsyncRead + Send>>;

/// Answers requests from peers (every request but the Hello).
#[async_trait]
pub trait RequestHandler: Send + Sync {
    async fn handle(
        &self,
        peer: &Peer,
        request_id: &str,
        op: federation_request::Op,
    ) -> federation_response::Result;

    /// [`RequestHandler::handle`] for requests whose answer may be
    /// followed by bytes (`fetch-attachment`). By default, none are.
    async fn handle_with_body(
        &self,
        peer: &Peer,
        request_id: &str,
        op: federation_request::Op,
    ) -> (federation_response::Result, Option<Body>) {
        (self.handle(peer, request_id, op).await, None)
    }
}

/// Size of the chunks a body is sent in.
const CHUNK_SIZE: usize = 256 * 1024;

/// The bytes following a response, as they arrive.
pub struct BodyStream {
    recv: RecvStream,
    timeout: Duration,
    peer: String,
}

impl BodyStream {
    /// The next piece of the body; `None` once the peer has finished the
    /// stream. Each piece must arrive within the request timeout.
    pub async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, RequestError> {
        let protocol = |message: String| RequestError::Protocol {
            peer: self.peer.clone(),
            message,
        };
        match tokio::time::timeout(self.timeout, read_frame(&mut self.recv)).await {
            Err(_) => Err(RequestError::Timeout {
                peer: self.peer.clone(),
            }),
            Ok(Err(FrameError::Truncated)) => Ok(None),
            Ok(Err(error)) => Err(protocol(error.to_string())),
            Ok(Ok(FederationFrame {
                body: Some(federation_frame::Body::Chunk(chunk)),
            })) => Ok(Some(chunk.data.unwrap_or_default())),
            Ok(Ok(_)) => Err(protocol("expected a chunk".to_owned())),
        }
    }
}

/// A [`RequestHandler`] that echoes and knows no addresses.
#[derive(Debug, Clone, Copy, Default)]
pub struct EchoHandler;

#[async_trait]
impl RequestHandler for EchoHandler {
    async fn handle(
        &self,
        _peer: &Peer,
        _request_id: &str,
        op: federation_request::Op,
    ) -> federation_response::Result {
        default_response(op)
    }
}

/// The answer to requests every handler answers alike: echo, and an error
/// for ops it does not serve.
pub fn default_response(op: federation_request::Op) -> federation_response::Result {
    use resonator_model::proto::federation_rpc::ProbeResult;
    match op {
        federation_request::Op::Echo(echo) => federation_response::Result::Echo(echo),
        federation_request::Op::Probe(_) => federation_response::Result::Probe(ProbeResult {
            exists: Some(false),
        }),
        federation_request::Op::Hello(_) => federation_response::Result::Error(
            FederationError::new(Code::UnexpectedRequest, "Hello only on the first stream"),
        ),
        _ => federation_response::Result::Error(FederationError::new(
            Code::BadRequest,
            "this server does not serve wave operations",
        )),
    }
}

/// Failure to set up the endpoint.
#[derive(Debug, Error)]
pub enum EndpointError {
    #[error("invalid federation endpoint setting: {0}")]
    Config(String),
    #[error("cannot bind the federation endpoint: {0}")]
    Bind(String),
}

/// A failed request.
#[derive(Debug, Error)]
pub enum RequestError {
    #[error("cannot connect to {peer}: {message}")]
    Connect { peer: String, message: String },
    #[error("{peer} refused our Hello: {error}")]
    Refused {
        peer: String,
        error: FederationError,
    },
    #[error("{peer} is not authorised for {domain} ({verdict:?})")]
    Unauthorised {
        peer: String,
        domain: String,
        verdict: Authorisation,
    },
    #[error("{peer} answered with an error: {error}")]
    Remote {
        peer: String,
        error: FederationError,
    },
    #[error("{peer} did not answer in time")]
    Timeout { peer: String },
    #[error("protocol error with {peer}: {message}")]
    Protocol { peer: String, message: String },
    #[error("the federation endpoint is shut down")]
    Closed,
}

impl RequestError {
    /// The error as a [`FederationError`], as the original reports
    /// failures to its callers.
    pub fn to_federation_error(&self) -> FederationError {
        match self {
            RequestError::Refused { error, .. } | RequestError::Remote { error, .. } => {
                error.clone()
            }
            RequestError::Timeout { .. } => {
                FederationError::new(Code::RemoteServerTimeout, self.to_string())
            }
            RequestError::Unauthorised { .. } => {
                FederationError::new(Code::NotAuthorized, self.to_string())
            }
            _ => FederationError::new(Code::UndefinedCondition, self.to_string()),
        }
    }
}

/// A new request id: a ULID.
pub fn new_request_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64);
    let mut random = [0u8; 16];
    // Without randomness the id is still unique enough per millisecond
    // for a retry key; failing here would be worse.
    let _ = getrandom::fill(&mut random);
    ulid::Ulid::from_parts(millis, u128::from_le_bytes(random)).to_string()
}

fn request_frame(id: String, op: federation_request::Op) -> FederationFrame {
    FederationFrame {
        body: Some(federation_frame::Body::Request(FederationRequest {
            id: Some(id),
            op: Some(op),
        })),
    }
}

fn response_frame(id: String, result: federation_response::Result) -> FederationFrame {
    FederationFrame {
        body: Some(federation_frame::Body::Response(FederationResponse {
            id: Some(id),
            result: Some(result),
        })),
    }
}

fn error_result(code: Code, message: impl Into<String>) -> federation_response::Result {
    federation_response::Result::Error(FederationError::new(code, message))
}

/// An outbound connection that has completed the Hello.
#[derive(Clone)]
struct Outbound {
    connection: Connection,
    peer: Peer,
}

type Slot = Arc<tokio::sync::Mutex<Option<Outbound>>>;

struct Inner {
    endpoint: Endpoint,
    options: EndpointOptions,
    identity: LocalIdentity,
    authority: Arc<dyn PeerAuthority>,
    handler: Arc<dyn RequestHandler>,
    static_addrs: MemoryLookup,
    outbound: Mutex<HashMap<PublicKey, Slot>>,
    /// Accepted connections that completed the Hello, by stable id.
    inbound: Mutex<HashMap<usize, (PublicKey, Connection)>>,
    /// This device's certificate (person mode).
    device_cert: Option<SignedDeviceCert>,
    pair: std::sync::OnceLock<Arc<dyn PairService>>,
    cancel: CancellationToken,
    tasks: TaskTracker,
}

impl Inner {
    /// This node's Hello to `peer` (answering `their`).
    fn hello(&self, peer: &PublicKey, their: Option<&Hello>) -> Hello {
        let mut hello = self.identity.hello();
        hello.device_cert = self.device_cert.clone();
        self.authority.complete_hello(&mut hello, peer, their);
        hello
    }
}

/// The federation endpoint; cheap to clone.
#[derive(Clone)]
pub struct FederationEndpoint {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for FederationEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FederationEndpoint")
            .field("id", &self.id().to_z32())
            .finish_non_exhaustive()
    }
}

fn short(key: &PublicKey) -> String {
    key.fmt_short().to_string()
}

impl FederationEndpoint {
    /// Binds the endpoint with `key` as its identity and starts accepting
    /// connections.
    pub async fn bind(
        key: SecretKey,
        options: EndpointOptions,
        identity: LocalIdentity,
        authority: Arc<dyn PeerAuthority>,
        handler: Arc<dyn RequestHandler>,
    ) -> Result<Self, EndpointError> {
        Self::bind_device(key, options, identity, None, authority, handler).await
    }

    /// [`FederationEndpoint::bind`] for a device of a person: its Hello
    /// carries `device_cert`, by which peers check that its key speaks
    /// for the person's domain.
    pub async fn bind_device(
        key: SecretKey,
        options: EndpointOptions,
        identity: LocalIdentity,
        device_cert: Option<SignedDeviceCert>,
        authority: Arc<dyn PeerAuthority>,
        handler: Arc<dyn RequestHandler>,
    ) -> Result<Self, EndpointError> {
        let relay_mode = match &options.relay {
            RelaySetting::Default => RelayMode::Default,
            RelaySetting::Disabled => RelayMode::Disabled,
            RelaySetting::Custom(url) => {
                let url: RelayUrl = url
                    .parse()
                    .map_err(|error| EndpointError::Config(format!("relay {url}: {error}")))?;
                RelayMode::Custom(RelayMap::from(url))
            }
        };
        let static_addrs = MemoryLookup::new();
        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(key)
            .alpns(vec![ALPN.to_vec(), PAIR_ALPN.to_vec()])
            .relay_mode(relay_mode)
            .address_lookup(static_addrs.clone());
        if options.discovery {
            builder = builder
                .address_lookup(PkarrPublisher::n0_dns())
                .address_lookup(PkarrResolver::n0_dns())
                .address_lookup(DnsAddressLookup::n0_dns());
        }
        if !options.bind.is_empty() {
            builder = builder.clear_ip_transports();
            for addr in &options.bind {
                builder = builder
                    .bind_addr(*addr)
                    .map_err(|error| EndpointError::Config(format!("{addr}: {error}")))?;
            }
        }
        let endpoint = builder
            .bind()
            .await
            .map_err(|error| EndpointError::Bind(error.to_string()))?;
        let inner = Arc::new(Inner {
            endpoint,
            options,
            identity,
            authority,
            handler,
            static_addrs,
            outbound: Mutex::new(HashMap::new()),
            inbound: Mutex::new(HashMap::new()),
            device_cert,
            pair: std::sync::OnceLock::new(),
            cancel: CancellationToken::new(),
            tasks: TaskTracker::new(),
        });
        inner.tasks.spawn(accept_loop(Arc::clone(&inner)));
        tracing::info!(
            key = %inner.endpoint.id().to_z32(),
            sockets = ?inner.endpoint.bound_sockets(),
            "federation endpoint listening"
        );
        Ok(FederationEndpoint { inner })
    }

    /// This endpoint's key.
    pub fn id(&self) -> PublicKey {
        self.inner.endpoint.id()
    }

    /// The address to reach this endpoint at: its key, the sockets it is
    /// bound to and whatever else iroh knows (relay, discovered addresses).
    pub fn addr(&self) -> EndpointAddr {
        let mut addr = self.inner.endpoint.addr();
        for socket in self.inner.endpoint.bound_sockets() {
            if !socket.ip().is_unspecified() {
                addr = addr.with_ip_addr(socket);
            }
        }
        addr
    }

    /// Installs what answers pairing requests. Returns false if one was
    /// installed already.
    pub fn set_pair_service(&self, service: Arc<dyn PairService>) -> bool {
        self.inner.pair.set(service).is_ok()
    }

    /// The underlying iroh endpoint.
    pub fn endpoint(&self) -> &Endpoint {
        &self.inner.endpoint
    }

    /// Makes a peer's addresses known, for dialling it by key without
    /// discovery.
    pub fn add_peer_addr(&self, addr: EndpointAddr) {
        self.inner.static_addrs.add_endpoint_info(addr);
    }

    /// Connects to `key` (or reuses the cached connection) and completes
    /// the Hello. With a `domain`, the peer must be authorised for it.
    pub async fn connect(
        &self,
        key: PublicKey,
        domain: Option<&str>,
    ) -> Result<Peer, RequestError> {
        Ok(self.outbound(key, domain).await?.peer)
    }

    /// Sends a request to `key` and waits for the answer. With a `domain`,
    /// the peer must be authorised for it. An error answer is returned as
    /// [`RequestError::Remote`].
    pub async fn request(
        &self,
        key: PublicKey,
        domain: Option<&str>,
        op: federation_request::Op,
    ) -> Result<federation_response::Result, RequestError> {
        self.request_with_id(key, domain, new_request_id(), op)
            .await
    }

    /// [`FederationEndpoint::request`] with the request id given, for a
    /// request retried later (the receiver answers a request id it has
    /// applied before without applying it again).
    pub async fn request_with_id(
        &self,
        key: PublicKey,
        domain: Option<&str>,
        id: String,
        op: federation_request::Op,
    ) -> Result<federation_response::Result, RequestError> {
        self.request_inner(key, domain, id, op)
            .await
            .map(|(result, _)| result)
    }

    /// Sends a request whose answer is followed by bytes
    /// (`fetch-attachment`) and returns the answer and a stream of the
    /// bytes. An error answer is returned as [`RequestError::Remote`].
    pub async fn request_with_body(
        &self,
        key: PublicKey,
        domain: Option<&str>,
        op: federation_request::Op,
    ) -> Result<(federation_response::Result, BodyStream), RequestError> {
        let (result, recv) = self
            .request_inner(key, domain, new_request_id(), op)
            .await?;
        Ok((
            result,
            BodyStream {
                recv,
                timeout: self.inner.options.request_timeout,
                peer: short(&key),
            },
        ))
    }

    async fn request_inner(
        &self,
        key: PublicKey,
        domain: Option<&str>,
        id: String,
        op: federation_request::Op,
    ) -> Result<(federation_response::Result, RecvStream), RequestError> {
        // One retry on a fresh connection if the cached one has died.
        for attempt in 0..2 {
            let outbound = self.outbound(key, domain).await?;
            match self
                .exchange(&outbound.connection, &key, id.clone(), op.clone())
                .await
            {
                Err(ExchangeError::Stream(message)) if attempt == 0 => {
                    tracing::debug!(peer = %short(&key), %message, "redialling federation peer");
                    self.forget(&key, &outbound.connection);
                }
                Err(ExchangeError::Stream(message)) => {
                    return Err(RequestError::Connect {
                        peer: short(&key),
                        message,
                    });
                }
                Err(ExchangeError::Failed(error)) => return Err(error),
                Ok((federation_response::Result::Error(error), _)) => {
                    return Err(RequestError::Remote {
                        peer: short(&key),
                        error,
                    });
                }
                Ok(answer) => return Ok(answer),
            }
        }
        unreachable!("the loop returns on its second attempt")
    }

    /// Stops accepting, lets requests being served finish (for a few
    /// seconds at most), closes every connection and the endpoint.
    pub async fn shutdown(&self) {
        let inner = &self.inner;
        inner.cancel.cancel();
        inner.tasks.close();
        if tokio::time::timeout(Duration::from_secs(5), inner.tasks.wait())
            .await
            .is_err()
        {
            tracing::warn!("federation requests still running at shutdown");
        }
        let slots: Vec<Slot> = lock(&inner.outbound)
            .drain()
            .map(|(_, slot)| slot)
            .collect();
        for slot in slots {
            if let Some(outbound) = slot.lock().await.take() {
                outbound
                    .connection
                    .close(VarInt::from_u32(CLOSE_OK), b"shutdown");
            }
        }
        inner.endpoint.close().await;
        tracing::info!("federation endpoint closed");
    }

    /// Closes every connection with these keys, in either direction (a
    /// revoked device's). Later Hellos decide whether they come back.
    pub async fn disconnect(&self, keys: &[PublicKey]) {
        if keys.is_empty() {
            return;
        }
        let slots: Vec<Slot> = {
            let outbound = lock(&self.inner.outbound);
            keys.iter()
                .filter_map(|key| outbound.get(key).cloned())
                .collect()
        };
        for slot in slots {
            if let Some(outbound) = slot.lock().await.take() {
                tracing::info!(peer = %short(&outbound.peer.key), "closing a connection to a revoked key");
                outbound
                    .connection
                    .close(VarInt::from_u32(CLOSE_REFUSED), b"revoked");
            }
        }
        let inbound: Vec<Connection> = lock(&self.inner.inbound)
            .values()
            .filter(|(key, _)| keys.contains(key))
            .map(|(_, connection)| connection.clone())
            .collect();
        for connection in inbound {
            tracing::info!(peer = %short(&connection.remote_id()), "closing a connection from a revoked key");
            connection.close(VarInt::from_u32(CLOSE_REFUSED), b"revoked");
        }
    }

    /// The keys of the peers connected now, either way.
    pub fn connected_peers(&self) -> Vec<PublicKey> {
        let mut keys: Vec<PublicKey> = lock(&self.inner.inbound)
            .values()
            .map(|(key, _)| *key)
            .collect();
        for key in lock(&self.inner.outbound).keys() {
            if !keys.contains(key) {
                keys.push(*key);
            }
        }
        keys
    }

    fn forget(&self, key: &PublicKey, connection: &Connection) {
        let slot = lock(&self.inner.outbound).get(key).cloned();
        if let Some(slot) = slot
            && let Ok(mut guard) = slot.try_lock()
            && guard
                .as_ref()
                .is_some_and(|o| o.connection.stable_id() == connection.stable_id())
        {
            *guard = None;
        }
    }

    async fn outbound(
        &self,
        key: PublicKey,
        domain: Option<&str>,
    ) -> Result<Outbound, RequestError> {
        let inner = &self.inner;
        if inner.cancel.is_cancelled() {
            return Err(RequestError::Closed);
        }
        if inner.authority.is_revoked(&key) {
            self.disconnect(&[key]).await;
            return Err(RequestError::Unauthorised {
                peer: short(&key),
                domain: domain.unwrap_or_default().to_owned(),
                verdict: Authorisation::Revoked,
            });
        }
        let slot = Arc::clone(lock(&inner.outbound).entry(key).or_default());
        let mut guard = slot.lock().await;
        if guard
            .as_ref()
            .is_some_and(|o| o.connection.close_reason().is_some())
        {
            *guard = None;
        }
        let outbound = match guard.as_mut() {
            Some(outbound) => outbound,
            None => guard.insert(self.dial(key).await?),
        };
        if let Some(domain) = domain
            && !outbound.peer.speaks_for(domain)
        {
            let verdict = inner
                .authority
                .authorise(&key, domain, &outbound.peer.hello.key_rotation)
                .await;
            if !verdict.is_authorised() {
                return Err(RequestError::Unauthorised {
                    peer: short(&key),
                    domain: domain.to_owned(),
                    verdict,
                });
            }
            outbound.peer.domains.push(domain.to_owned());
        }
        Ok(outbound.clone())
    }

    async fn dial(&self, key: PublicKey) -> Result<Outbound, RequestError> {
        let inner = &self.inner;
        let peer = short(&key);
        let connect = inner.endpoint.connect(EndpointAddr::new(key), ALPN);
        let connection = tokio::time::timeout(inner.options.request_timeout, connect)
            .await
            .map_err(|_| RequestError::Timeout { peer: peer.clone() })?
            .map_err(|error| RequestError::Connect {
                peer: peer.clone(),
                message: error.to_string(),
            })?;
        let hello = federation_request::Op::Hello(inner.hello(&key, None));
        let result = match self
            .exchange(&connection, &key, new_request_id(), hello)
            .await
        {
            Ok((result, _)) => result,
            Err(ExchangeError::Stream(message)) => {
                return Err(RequestError::Connect { peer, message });
            }
            Err(ExchangeError::Failed(error)) => return Err(error),
        };
        let hello = match result {
            federation_response::Result::Hello(hello) => hello,
            federation_response::Result::Error(error) => {
                connection.close(VarInt::from_u32(CLOSE_OK), b"refused");
                return Err(RequestError::Refused { peer, error });
            }
            _ => {
                connection.close(VarInt::from_u32(CLOSE_REFUSED), b"bad hello");
                return Err(RequestError::Protocol {
                    peer,
                    message: "answered the Hello with something else".to_owned(),
                });
            }
        };
        if hello.protocol_version != Some(PROTOCOL_VERSION) {
            connection.close(VarInt::from_u32(CLOSE_REFUSED), b"protocol version");
            return Err(RequestError::Protocol {
                peer,
                message: format!("speaks protocol version {:?}", hello.protocol_version),
            });
        }
        inner.authority.hello_received(&key, &hello).await;
        let domains = authorised_domains(inner.authority.as_ref(), &key, &hello).await;
        tracing::debug!(%peer, ?domains, "federation connection established");
        Ok(Outbound {
            connection,
            peer: Peer {
                key,
                domains,
                hello,
            },
        })
    }

    async fn exchange(
        &self,
        connection: &Connection,
        key: &PublicKey,
        id: String,
        op: federation_request::Op,
    ) -> Result<(federation_response::Result, RecvStream), ExchangeError> {
        let peer = short(key);
        let frame = request_frame(id.clone(), op);
        let exchange = async {
            let (mut send, mut recv) = connection
                .open_bi()
                .await
                .map_err(|error| ExchangeError::Stream(error.to_string()))?;
            write_frame(&mut send, &frame)
                .await
                .map_err(|error| match error {
                    FrameError::Io(error) => ExchangeError::Stream(error.to_string()),
                    other => ExchangeError::Failed(RequestError::Protocol {
                        peer: peer.clone(),
                        message: other.to_string(),
                    }),
                })?;
            send.finish()
                .map_err(|error| ExchangeError::Stream(error.to_string()))?;
            let frame = read_frame(&mut recv).await.map_err(|error| {
                ExchangeError::Failed(RequestError::Protocol {
                    peer: peer.clone(),
                    message: error.to_string(),
                })
            })?;
            Ok((frame, recv))
        };
        let (frame, recv) = tokio::time::timeout(self.inner.options.request_timeout, exchange)
            .await
            .map_err(|_| ExchangeError::Failed(RequestError::Timeout { peer: peer.clone() }))??;
        let protocol = |message: &str| {
            ExchangeError::Failed(RequestError::Protocol {
                peer: peer.clone(),
                message: message.to_owned(),
            })
        };
        match frame.body {
            Some(federation_frame::Body::Response(response)) => {
                if response.id.as_deref() != Some(id.as_str()) {
                    return Err(protocol("answered with another request id"));
                }
                let result = response.result.ok_or_else(|| protocol("empty response"))?;
                Ok((result, recv))
            }
            _ => Err(protocol("answered with a request")),
        }
    }
}

enum ExchangeError {
    /// The stream or connection failed; a fresh connection may help.
    Stream(String),
    Failed(RequestError),
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn authorised_domains(
    authority: &dyn PeerAuthority,
    key: &PublicKey,
    hello: &Hello,
) -> Vec<String> {
    let mut domains = Vec::new();
    for domain in &hello.domain {
        let verdict = authority.authorise(key, domain, &hello.key_rotation).await;
        if verdict.is_authorised() {
            if !domains.contains(domain) {
                domains.push(domain.clone());
            }
        } else {
            tracing::info!(peer = %short(key), %domain, ?verdict, "federation peer not authorised for a domain it claims");
        }
    }
    domains
}

async fn accept_loop(inner: Arc<Inner>) {
    loop {
        let incoming = tokio::select! {
            () = inner.cancel.cancelled() => break,
            incoming = inner.endpoint.accept() => incoming,
        };
        let Some(incoming) = incoming else {
            break;
        };
        let connection_inner = Arc::clone(&inner);
        inner.tasks.spawn(async move {
            match incoming.await {
                Ok(connection) => serve_connection(connection_inner, connection).await,
                Err(error) => tracing::debug!(%error, "incoming federation connection failed"),
            }
        });
    }
}

async fn serve_connection(inner: Arc<Inner>, connection: Connection) {
    if connection.alpn() == PAIR_ALPN {
        serve_pairing(&inner, connection).await;
        return;
    }
    let key = connection.remote_id();
    let peer_name = short(&key);
    let peer = match tokio::time::timeout(
        inner.options.request_timeout,
        accept_hello(&inner, &connection, key),
    )
    .await
    {
        Ok(Ok(peer)) => peer,
        Ok(Err(message)) => {
            tracing::info!(peer = %peer_name, %message, "refused a federation connection");
            connection.close(VarInt::from_u32(CLOSE_REFUSED), b"refused");
            return;
        }
        Err(_) => {
            connection.close(VarInt::from_u32(CLOSE_REFUSED), b"no hello");
            return;
        }
    };
    tracing::debug!(peer = %peer_name, domains = ?peer.domains, "federation peer connected");
    let peer = Arc::new(peer);
    let permits = Arc::new(Semaphore::new(inner.options.max_concurrent_requests.max(1)));
    let id = connection.stable_id();
    lock(&inner.inbound).insert(id, (key, connection.clone()));
    loop {
        let streams = tokio::select! {
            () = inner.cancel.cancelled() => break,
            streams = connection.accept_bi() => streams,
        };
        let Ok((send, recv)) = streams else {
            break;
        };
        if inner.authority.is_revoked(&key) {
            tracing::info!(peer = %peer_name, "cutting off a revoked peer");
            connection.close(VarInt::from_u32(CLOSE_REFUSED), b"revoked");
            break;
        }
        let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
            break;
        };
        let inner_for_stream = Arc::clone(&inner);
        let peer = Arc::clone(&peer);
        inner.tasks.spawn(async move {
            serve_stream(&inner_for_stream, &peer, send, recv).await;
            drop(permit);
        });
    }
    lock(&inner.inbound).remove(&id);
}

/// Serves a pairing connection: one `pair` request per stream.
async fn serve_pairing(inner: &Inner, connection: Connection) {
    let joiner = connection.remote_id();
    for _ in 0..MAX_PAIR_REQUESTS {
        let streams = tokio::select! {
            () = inner.cancel.cancelled() => break,
            streams = tokio::time::timeout(Duration::from_secs(120), connection.accept_bi()) => streams,
        };
        let Ok(Ok((mut send, mut recv))) = streams else {
            break;
        };
        let frame = match tokio::time::timeout(inner.options.request_timeout, read_frame(&mut recv))
            .await
        {
            Ok(Ok(frame)) => frame,
            _ => break,
        };
        let Some(federation_frame::Body::Request(FederationRequest { id: Some(id), op })) =
            frame.body
        else {
            break;
        };
        let result = match (op, inner.pair.get()) {
            (Some(federation_request::Op::Pair(request)), Some(service)) => {
                service.pair(joiner, request).await
            }
            (Some(federation_request::Op::PairCode(request)), Some(service)) => {
                service.pair_code(joiner, request).await
            }
            (Some(federation_request::Op::Pair(_) | federation_request::Op::PairCode(_)), None) => {
                error_result(Code::NotAuthorized, "this device does not pair".to_owned())
            }
            _ => error_result(Code::BadRequest, "only pair is spoken here".to_owned()),
        };
        if write_frame(&mut send, &response_frame(id, result))
            .await
            .is_err()
        {
            break;
        }
        let _ = send.finish();
        let _ = tokio::time::timeout(Duration::from_secs(1), send.stopped()).await;
    }
    connection.close(VarInt::from_u32(0), b"done");
}

/// Reads the Hello on the first stream, decides, and answers.
async fn accept_hello(
    inner: &Inner,
    connection: &Connection,
    key: PublicKey,
) -> Result<Peer, String> {
    let (mut send, mut recv) = connection
        .accept_bi()
        .await
        .map_err(|error| error.to_string())?;
    let frame = read_frame(&mut recv)
        .await
        .map_err(|error| error.to_string())?;
    let Some(federation_frame::Body::Request(FederationRequest {
        id: Some(id),
        op: Some(federation_request::Op::Hello(hello)),
    })) = frame.body
    else {
        return Err("first request is not a Hello".to_owned());
    };
    let refuse = async |send: &mut SendStream, code: Code, message: String| {
        let frame = response_frame(id.clone(), error_result(code, message.clone()));
        let _ = write_frame(send, &frame).await;
        let _ = send.finish();
        // Give the answer a moment to leave before the connection closes.
        let _ = tokio::time::timeout(Duration::from_secs(1), send.stopped()).await;
        Err(message)
    };
    if hello.protocol_version != Some(PROTOCOL_VERSION) {
        return refuse(
            &mut send,
            Code::BadRequest,
            format!(
                "protocol version {:?} is not supported (this server speaks {PROTOCOL_VERSION})",
                hello.protocol_version
            ),
        )
        .await;
    }
    inner.authority.hello_received(&key, &hello).await;
    let domains = authorised_domains(inner.authority.as_ref(), &key, &hello).await;
    if domains.is_empty() {
        return refuse(
            &mut send,
            Code::NotAuthorized,
            format!("not authorised for any of {:?}", hello.domain),
        )
        .await;
    }
    let answer = response_frame(
        id,
        federation_response::Result::Hello(inner.hello(&key, Some(&hello))),
    );
    write_frame(&mut send, &answer)
        .await
        .map_err(|error| error.to_string())?;
    send.finish().map_err(|error| error.to_string())?;
    Ok(Peer {
        key,
        domains,
        hello,
    })
}

/// Serves one request stream.
async fn serve_stream(inner: &Inner, peer: &Peer, mut send: SendStream, mut recv: RecvStream) {
    let frame =
        match tokio::time::timeout(inner.options.request_timeout, read_frame(&mut recv)).await {
            Ok(Ok(frame)) => frame,
            Ok(Err(error)) => {
                tracing::debug!(peer = %short(&peer.key), %error, "unreadable federation request");
                let frame = response_frame(
                    String::new(),
                    error_result(Code::BadRequest, error.to_string()),
                );
                let _ = write_frame(&mut send, &frame).await;
                let _ = send.finish();
                return;
            }
            Err(_) => return,
        };
    let Some(federation_frame::Body::Request(request)) = frame.body else {
        let frame = response_frame(
            String::new(),
            error_result(Code::BadRequest, "expected a request"),
        );
        let _ = write_frame(&mut send, &frame).await;
        let _ = send.finish();
        return;
    };
    let id = request.id.unwrap_or_default();
    let result = match request.op {
        Some(federation_request::Op::Hello(_)) => (
            error_result(Code::UnexpectedRequest, "Hello only on the first stream"),
            None,
        ),
        Some(federation_request::Op::Revocations(request)) => (
            federation_response::Result::Revocations(
                inner.authority.revocations(&peer.key, request).await,
            ),
            None,
        ),
        Some(op) => inner.handler.handle_with_body(peer, &id, op).await,
        None => (error_result(Code::BadRequest, "no operation"), None),
    };
    let (result, body) = match result {
        (result @ federation_response::Result::Error(_), _) => (result, None),
        answer => answer,
    };
    let frame = response_frame(id, result);
    let frame = match crate::codec::encode_frame(&frame) {
        Ok(_) => frame,
        Err(error) => {
            tracing::warn!(%error, "federation response cannot be sent");
            response_frame(
                frame_id(&frame),
                error_result(Code::InternalServerError, error.to_string()),
            )
        }
    };
    let sent_ok = matches!(
        &frame.body,
        Some(federation_frame::Body::Response(FederationResponse {
            result: Some(result),
            ..
        })) if !matches!(result, federation_response::Result::Error(_))
    );
    if let Err(error) = write_frame(&mut send, &frame).await {
        tracing::debug!(peer = %short(&peer.key), %error, "cannot answer a federation request");
        return;
    }
    if let (Some(mut body), true) = (body, sent_ok) {
        let mut buffer = vec![0u8; CHUNK_SIZE];
        loop {
            let read = match body.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) => {
                    // The receiver sees a short body and rejects it.
                    tracing::warn!(%error, "cannot read a federation response body");
                    let _ = send.reset(0u32.into());
                    return;
                }
            };
            let frame = FederationFrame {
                body: Some(federation_frame::Body::Chunk(Chunk {
                    data: Some(buffer[..read].to_vec()),
                })),
            };
            if let Err(error) = write_frame(&mut send, &frame).await {
                tracing::debug!(peer = %short(&peer.key), %error, "cannot send a response body");
                return;
            }
        }
    }
    let _ = send.finish();
}

fn frame_id(frame: &FederationFrame) -> String {
    match &frame.body {
        Some(federation_frame::Body::Response(response)) => response.id.clone().unwrap_or_default(),
        Some(federation_frame::Body::Request(request)) => request.id.clone().unwrap_or_default(),
        Some(federation_frame::Body::Chunk(_)) | None => String::new(),
    }
}
