//! [`Transport`] over iroh.
//!
//! Serving: an iroh `Router` accepts ALPN `resonator/rdf/0` connections.
//! The handler runs the hello exchange on the connection's first bi
//! stream (answering `rsntr:Refused` when versions or encodings cannot
//! meet), then keeps accepting request streams. A request in a
//! modulation this node does not serve is fast-failed with
//! `mod-unsupported` at this layer; everything else flows to the node
//! through the channel [`IrohTransport::bind`] hands out.
//!
//! Dialing: connections are opened lazily and cached per peer
//! `EndpointId`. A cached connection is reused until it closes; every
//! request gets its own bi stream on it.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::BytesMut;
use iroh::endpoint::{Connection, QuicTransportConfig, RecvStream, SendStream, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey, TransportAddr};
use iroh_tickets::endpoint::EndpointTicket;
use oxrdf::{Literal, NamedNode, Term};
use resonator_protocol::vocab::prop;
use resonator_protocol::{
    ALPN, EnvelopeObject, ErrorCode, ErrorEnvelope, Generic, Hello, MAX_FRAME_LEN, decode_envelope,
    encode_envelope,
};
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, warn};

use crate::{IncomingRequest, PeerId, RequestStream, Transport, TransportError};

/// Backpressure bound of the incoming-request channel from `bind`.
const INCOMING_QUEUE: usize = 32;

/// Connection-level QUIC idle timeout. A peer that restarts leaves the
/// old connection looking alive (`close_reason()` stays `None`,
/// `open_bi` succeeds locally); with iroh's 5s keep-alives this bound
/// turns such a zombie into a closed connection within ~10s instead of
/// hanging requests indefinitely. Healthy-but-quiet connections
/// (entrainments, parked requests) are unaffected: the keep-alives keep
/// them under the bound.
const CONN_IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Upper bound on one dial (connect + hello exchange). A dial against
/// stale addresses and a not-yet-republished discovery record otherwise
/// waits on QUIC handshake timeouts far longer than any caller wants.
const DIAL_TIMEOUT: Duration = Duration::from_secs(15);

/// How often a live connection's paths are re-sampled for the
/// [`AddrSink`].
const PATH_SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

/// Size of one read into the frame reassembly buffer.
const READ_CHUNK: usize = 16 * 1024;

/// The newline-terminated plain-text banner answered, unframed, to a
/// caller whose opening bytes are not a length-prefixed frame (a bare
/// `help`, `HELP`, or `?`, say) - connection-protocol.md section 4bis.
/// A courtesy outside the framed protocol: the node explains itself and
/// leaves the connection open for a real hello.
pub const PLAINTEXT_BANNER: &str = "\
resonator node (rsntr, envelope 0.1). I speak RDF objects over QUIC.
Hand-driven? Ask me for help in one line:
  [] a rsntr:Query ; rsntr:mod \"help\" .
and I will reply with usage. Not admitted? Knock:
  [] a rsntr:Knock ; rsntr:message \"who you are and what you want\" .
";

/// The [`PeerId`] an ed25519 secret key would serve under, computed
/// without binding an endpoint or doing any I/O. Answers "what is my
/// id?" for tooling that only holds the `rsntr.key` bytes.
pub fn endpoint_id_from_secret(secret: [u8; 32]) -> PeerId {
    PeerId(*SecretKey::from_bytes(&secret).public().as_bytes())
}

/// Binds an iroh endpoint for `secret_key`: localhost-only with relays
/// and lookup disabled when `offline`, the n0 production preset
/// otherwise.
async fn bind_endpoint(secret_key: SecretKey, offline: bool) -> Result<Endpoint, TransportError> {
    let transport_config = QuicTransportConfig::builder()
        .max_idle_timeout(Some(CONN_IDLE_TIMEOUT.try_into().expect("10s fits an IdleTimeout")))
        .build();
    let bound = if offline {
        Endpoint::builder(presets::Minimal)
            .secret_key(secret_key)
            .relay_mode(RelayMode::Disabled)
            .transport_config(transport_config)
            .bind_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .map_err(|e| TransportError::Bind(e.to_string()))?
            .bind()
            .await
    } else {
        Endpoint::builder(presets::N0)
            .secret_key(secret_key)
            .transport_config(transport_config)
            .bind()
            .await
    };
    bound.map_err(|e| TransportError::Bind(e.to_string()))
}

/// Stands up a short-lived endpoint for `secret`, waits up to `wait` for
/// it to learn an address (its home relay under n0, or a direct socket
/// when `offline`), and returns the dialing ticket string. The endpoint
/// is dropped before returning and no router ever runs; this backs
/// `rsntr ticket`, and its output pastes into any iroh app (dumbpipe
/// included). Offline tickets carry direct addresses only, so they dial
/// on the local network; the default carries a relay for NAT traversal.
pub async fn mint_ticket(
    secret: [u8; 32],
    offline: bool,
    wait: std::time::Duration,
) -> Result<String, TransportError> {
    let endpoint = bind_endpoint(SecretKey::from_bytes(&secret), offline).await?;
    let deadline = tokio::time::Instant::now() + wait;
    while endpoint.addr().addrs.is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let ticket = EndpointTicket::from(endpoint.addr()).to_string();
    drop(endpoint);
    Ok(ticket)
}

/// Splits a dialing ticket (from [`mint_ticket`] or
/// [`IrohTransport::ticket`]) into the peer's [`PeerId`] and its direct
/// socket addresses. Any relay in the ticket is dropped; the returned
/// addresses are LAN/direct dial hints fit for
/// [`IrohTransport::add_peer_addrs`] or persisting. An empty list is not
/// an error: dialing by id alone still works via relays and lookup when
/// online.
pub fn parse_ticket(s: &str) -> Result<(PeerId, Vec<SocketAddr>), TransportError> {
    let ticket: EndpointTicket = s
        .trim()
        .parse()
        .map_err(|e| TransportError::InvalidTicket(format!("{e}")))?;
    let addr: EndpointAddr = ticket.into();
    Ok((
        PeerId(*addr.id.as_bytes()),
        addr.ip_addrs().copied().collect(),
    ))
}

/// Checks a peer's hello against what this node can serve (connection
/// protocol sec 5); a mismatch yields the refusal code and reason.
fn hello_incompatibility(hello: &Hello) -> Option<(&'static str, String)> {
    // The major version lives in the ALPN (pinned 0); rsntr:ver is the
    // minor, and any 0.x minor is fine within major 0.
    if hello.envelope_version != "0" && !hello.envelope_version.starts_with("0.") {
        return Some((
            "envelope-version",
            format!(
                "this node speaks envelope 0.x only, got {:?}",
                hello.envelope_version
            ),
        ));
    }
    if !hello.encodings.iter().any(|e| e == "turtle") {
        return Some((
            "encoding",
            "no common frame encoding: turtle is mandatory".to_string(),
        ));
    }
    None
}

/// The `rsntr:Refused` handshake answer. Refused is additive vocabulary,
/// so it travels as a Generic frame (the codec passes unknown rsntr
/// classes through).
fn refused_envelope(code: &str, reason: &str) -> EnvelopeObject {
    let lit = |s: &str| Term::Literal(Literal::new_simple_literal(s));
    EnvelopeObject::Generic(Generic {
        class: "Refused".to_string(),
        props: vec![
            (NamedNode::from(prop::CODE), lit(code)),
            (NamedNode::from(prop::REASON), lit(reason)),
        ],
    })
}

/// Pulls (code, reason) out of a received `rsntr:Refused` Generic frame.
fn parse_refused(generic: &Generic) -> (String, String) {
    let mut code = "unspecified".to_string();
    let mut reason = String::new();
    for (predicate, object) in &generic.props {
        let Term::Literal(lit) = object else { continue };
        if predicate.as_str() == prop::CODE.as_str() {
            code = lit.value().to_string();
        } else if predicate.as_str() == prop::REASON.as_str() {
            reason = lit.value().to_string();
        }
    }
    (code, reason)
}

/// Async accept gate for the blobs provider: given the dialer's proven
/// [`PeerId`], decide whether it may fetch at all. Typically backed by
/// the node's `_peers` table (chat protocol sec 6.2); beyond the gate
/// the 256-bit hash is the capability.
pub type BlobGate = Arc<
    dyn Fn(PeerId) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        + Send
        + Sync,
>;

/// Serve iroh-blobs on the shared endpoint and router.
#[derive(Clone)]
pub struct BlobsConfig {
    /// Filesystem blob store root (lives beside `rsntr.db` in a node
    /// dir).
    pub store_dir: std::path::PathBuf,
    /// Accept gate; `None` serves every dialer (tests only).
    pub gate: Option<BlobGate>,
}

impl std::fmt::Debug for BlobsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobsConfig")
            .field("store_dir", &self.store_dir)
            .field("gate", &self.gate.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

/// What [`IrohTransport::bind`] needs to know.
#[derive(Debug, Clone)]
pub struct IrohConfig {
    /// The hello sent on every new connection. Keeping it in agreement
    /// with the node's `_rsntr` table is the caller's job.
    pub hello: Hello,
    /// ed25519 secret key bytes; absent means generate a fresh key.
    pub secret_key: Option<[u8; 32]>,
    /// Offline preset: localhost bind, no relays, no lookup services.
    /// Dialing then needs manual addresses via
    /// [`IrohTransport::add_peer_addrs`]. Meant for tests and LAN use.
    pub offline: bool,
    /// Serve the iroh-gossip ALPN on the same endpoint and router, so
    /// one identity covers both the rdf protocol and presence. The
    /// spawned handle comes back through [`IrohTransport::gossip`] for
    /// the presence surface.
    pub gossip: bool,
    /// Serve the iroh-blobs ALPN on the same endpoint and router (file
    /// transfer, chat protocol sec 6). The store comes back through
    /// [`IrohTransport::blobs`].
    pub blobs: Option<BlobsConfig>,
}

impl IrohConfig {
    /// Production preset: n0 relays and address lookup.
    pub fn n0(hello: Hello) -> Self {
        Self {
            hello,
            secret_key: None,
            offline: false,
            gossip: false,
            blobs: None,
        }
    }

    /// Offline test preset: manual localhost addresses only.
    pub fn offline(hello: Hello) -> Self {
        Self {
            hello,
            secret_key: None,
            offline: true,
            gossip: false,
            blobs: None,
        }
    }
}

/// A dialed connection kept for reuse, with the hello its peer sent.
#[derive(Debug, Clone)]
struct LiveConn {
    conn: Connection,
    hello: Hello,
}

/// Async per-dial address lookup: given the peer id, the current dial
/// addresses. Installed via [`IrohTransport::set_addr_source`] and
/// consulted on every dial (rdf and sibling ALPNs alike), so address
/// changes made while the transport runs - a `rsntr peer add` against
/// the serving node's `_peers`, say - take effect without a restart,
/// unlike the static [`IrohTransport::add_peer_addrs`] book.
pub type AddrSource = Arc<
    dyn Fn(PeerId) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<SocketAddr>> + Send>>
        + Send
        + Sync,
>;

/// Interior slot for the optional [`AddrSource`] (manual Debug: the
/// closure has none).
#[derive(Default)]
struct AddrSourceSlot(std::sync::Mutex<Option<AddrSource>>);

impl AddrSourceSlot {
    fn get(&self) -> Option<AddrSource> {
        self.0.lock().expect("addr source lock poisoned").clone()
    }
}

impl std::fmt::Debug for AddrSourceSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AddrSourceSlot")
            .field(&self.get().map(|_| "<fn>"))
            .finish()
    }
}

/// The mirror of [`AddrSource`]: called with a peer's currently
/// observed direct addresses whenever a live connection (dialed or
/// accepted) knows them. Installed via
/// [`IrohTransport::set_addr_sink`]; the owner typically merges the
/// addresses into `_peers` so future dials stay fresh across the
/// peer's restarts (ports change every serve run).
pub type AddrSink = Arc<dyn Fn(PeerId, Vec<SocketAddr>) + Send + Sync>;

/// Interior slot for the optional [`AddrSink`], shared with the
/// acceptor and every connection's path watcher.
#[derive(Default)]
struct AddrSinkSlot(std::sync::Mutex<Option<AddrSink>>);

impl AddrSinkSlot {
    fn get(&self) -> Option<AddrSink> {
        self.0.lock().expect("addr sink lock poisoned").clone()
    }
}

impl std::fmt::Debug for AddrSinkSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AddrSinkSlot")
            .field(&self.get().map(|_| "<fn>"))
            .finish()
    }
}

/// Watches a live connection's network paths and reports the peer's
/// direct (IP) remote addresses to the sink whenever the set changes.
/// One lightweight task per connection; it ends when the connection
/// closes. Relay paths are never reported: only addresses a future
/// dial can use directly.
fn watch_paths(conn: Connection, peer: PeerId, slot: Arc<AddrSinkSlot>) {
    tokio::spawn(async move {
        let mut last: Vec<SocketAddr> = Vec::new();
        loop {
            let mut ips: Vec<SocketAddr> = conn
                .paths()
                .iter()
                .filter(|p| p.is_ip())
                .filter_map(|p| match p.remote_addr() {
                    TransportAddr::Ip(sa) => Some(*sa),
                    _ => None,
                })
                .collect();
            ips.sort();
            ips.dedup();
            if !ips.is_empty() && ips != last {
                last = ips.clone();
                if let Some(sink) = slot.get() {
                    sink(peer, ips);
                }
            }
            // Re-sample while the connection lives; stop when it ends.
            if tokio::time::timeout(PATH_SAMPLE_INTERVAL, conn.closed())
                .await
                .is_ok()
            {
                break;
            }
        }
    });
}

/// The iroh transport: accept router on one side, connection cache on
/// the other.
#[derive(Debug)]
pub struct IrohTransport {
    endpoint: Endpoint,
    router: Router,
    gossip: Option<iroh_gossip::Gossip>,
    blobs: Option<iroh_blobs::store::fs::FsStore>,
    local_hello: Hello,
    /// Cache of dialed connections by peer. A tokio Mutex on purpose: it
    /// is held across the dial await, which also serializes concurrent
    /// dials to the same peer.
    conns: Mutex<HashMap<PeerId, LiveConn>>,
    /// Manually registered dial addresses, for peers reachable without
    /// any lookup service.
    addr_book: std::sync::Mutex<HashMap<PeerId, Vec<SocketAddr>>>,
    /// Live per-dial address lookup, layered over the static book.
    addr_source: AddrSourceSlot,
    /// Observed-address reporting for live connections, shared with the
    /// acceptor and every path watcher.
    addr_sink: Arc<AddrSinkSlot>,
    /// Outbound connections actually dialed (reuse tests read this).
    dials: AtomicU64,
    /// Inbound connections accepted (reuse tests read this).
    accepted: Arc<AtomicU64>,
}

impl IrohTransport {
    /// Binds the endpoint and spawns the accept router; returns the
    /// transport and the channel incoming requests arrive on.
    pub async fn bind(
        config: IrohConfig,
    ) -> Result<
        (
            Arc<Self>,
            mpsc::Receiver<IncomingRequest<IrohRequestStream>>,
        ),
        TransportError,
    > {
        let secret_key = config
            .secret_key
            .map(|bytes| SecretKey::from_bytes(&bytes))
            .unwrap_or_else(SecretKey::generate);
        let endpoint = bind_endpoint(secret_key, config.offline).await?;

        let (tx, rx) = mpsc::channel(INCOMING_QUEUE);
        let accepted = Arc::new(AtomicU64::new(0));
        let addr_sink = Arc::new(AddrSinkSlot::default());
        let mut builder = Router::builder(endpoint.clone()).accept(
            ALPN,
            Acceptor {
                local_hello: config.hello.clone(),
                incoming: tx,
                accepted: accepted.clone(),
                addr_sink: addr_sink.clone(),
            },
        );
        let gossip = if config.gossip {
            let gossip = iroh_gossip::Gossip::builder().spawn(endpoint.clone());
            builder = builder.accept(iroh_gossip::ALPN, gossip.clone());
            Some(gossip)
        } else {
            None
        };
        let blobs = match config.blobs {
            Some(bc) => {
                let store = iroh_blobs::store::fs::FsStore::load(&bc.store_dir)
                    .await
                    .map_err(|e| TransportError::Blobs(format!("loading the blob store: {e}")))?;
                builder = builder.accept(
                    iroh_blobs::ALPN,
                    GatedBlobs {
                        inner: iroh_blobs::BlobsProtocol::new(&store, None),
                        gate: bc.gate,
                    },
                );
                Some(store)
            }
            None => None,
        };
        let router = builder.spawn();

        let transport = Arc::new(Self {
            endpoint,
            router,
            gossip,
            blobs,
            local_hello: config.hello,
            conns: Mutex::new(HashMap::new()),
            addr_book: std::sync::Mutex::new(HashMap::new()),
            addr_source: AddrSourceSlot::default(),
            addr_sink,
            dials: AtomicU64::new(0),
            accepted,
        });
        Ok((transport, rx))
    }

    /// This node's identity as a [`PeerId`].
    pub fn peer_id(&self) -> PeerId {
        PeerId(*self.endpoint.id().as_bytes())
    }

    /// The underlying iroh endpoint, for surfaces that share this
    /// identity (presence attaches its gossip loops here).
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The gossip handle, when [`IrohConfig::gossip`] asked for one.
    pub fn gossip(&self) -> Option<&iroh_gossip::Gossip> {
        self.gossip.as_ref()
    }

    /// The local blob store, when [`IrohConfig::blobs`] asked for one.
    pub fn blobs(&self) -> Option<&iroh_blobs::store::fs::FsStore> {
        self.blobs.as_ref()
    }

    /// Imports a file into this node's blob store (BLAKE3 hashed, pinned
    /// under a tag) and returns `(64-hex hash, exact byte size)`.
    pub async fn blob_add_path(
        &self,
        path: &std::path::Path,
    ) -> Result<(String, u64), TransportError> {
        let store = self
            .blobs
            .as_ref()
            .ok_or_else(|| TransportError::Blobs("no blob store on this endpoint".into()))?;
        blob_add_to_store(store, path).await
    }

    /// Fetches `hash` (64-hex, `blake3:` prefix accepted) from `peer`
    /// over the iroh-blobs ALPN with verified (bao) streaming, into this
    /// node's blob store when it has one, else an in-memory store, and
    /// writes the verified bytes to `target`. Returns the byte size.
    pub async fn blob_fetch_to_path(
        &self,
        peer: PeerId,
        hash: &str,
        target: &std::path::Path,
    ) -> Result<u64, TransportError> {
        let hash = parse_blob_hash(hash)?;
        let conn = self.connect_alpn(peer, iroh_blobs::ALPN).await?;
        let target = std::path::absolute(target)
            .map_err(|e| TransportError::Blobs(format!("resolving {}: {e}", target.display())))?;
        match &self.blobs {
            Some(store) => fetch_and_export(store, conn, hash, &target).await,
            None => {
                let store = iroh_blobs::store::mem::MemStore::new();
                fetch_and_export(&store, conn, hash, &target).await
            }
        }
    }

    /// [`blob_fetch_to_path`](Self::blob_fetch_to_path), but returns the
    /// verified bytes in memory (the `rsntr fetch` stdout path).
    pub async fn blob_fetch_bytes(
        &self,
        peer: PeerId,
        hash: &str,
    ) -> Result<Vec<u8>, TransportError> {
        let hash = parse_blob_hash(hash)?;
        let conn = self.connect_alpn(peer, iroh_blobs::ALPN).await?;
        match &self.blobs {
            Some(store) => fetch_and_read(store, conn, hash).await,
            None => {
                let store = iroh_blobs::store::mem::MemStore::new();
                fetch_and_read(&store, conn, hash).await
            }
        }
    }

    /// Opens a raw connection to `peer` on `alpn`, using the same manual
    /// address book as the rdf dialer. Sibling protocols (iroh-blobs)
    /// dial through this so one identity covers everything.
    pub async fn connect_alpn(
        &self,
        peer: PeerId,
        alpn: &[u8],
    ) -> Result<Connection, TransportError> {
        let endpoint_id = EndpointId::from_bytes(peer.as_bytes())
            .map_err(|e| TransportError::InvalidPeerId(e.to_string()))?;
        let manual = self.dial_addrs(peer).await;
        let addr =
            EndpointAddr::new(endpoint_id).with_addrs(manual.into_iter().map(TransportAddr::Ip));
        self.endpoint
            .connect(addr, alpn)
            .await
            .map_err(|e| TransportError::Dial(e.to_string()))
    }

    /// The endpoint's current dialing ticket (id plus known addresses
    /// and relay), pasteable into another iroh app such as dumbpipe. For
    /// the no-serving-node variant see [`mint_ticket`].
    pub fn ticket(&self) -> String {
        EndpointTicket::from(self.endpoint.addr()).to_string()
    }

    /// The socket addresses this endpoint is bound to, for the other
    /// side of an offline test or for persisting as dial hints.
    pub fn direct_addrs(&self) -> Vec<SocketAddr> {
        self.endpoint.bound_sockets()
    }

    /// Registers manual dial addresses for a peer (offline / LAN use).
    pub fn add_peer_addrs(&self, peer: PeerId, addrs: Vec<SocketAddr>) {
        self.addr_book
            .lock()
            .expect("addr book lock poisoned")
            .entry(peer)
            .or_default()
            .extend(addrs);
    }

    /// Installs the live address lookup consulted on every dial, layered
    /// over the static book. A serving node backs this with its `_peers`
    /// table so `rsntr peer add` from another process is dialable
    /// without a restart.
    pub fn set_addr_source(&self, source: AddrSource) {
        *self
            .addr_source
            .0
            .lock()
            .expect("addr source lock poisoned") = Some(source);
    }

    /// Installs the observed-address sink consulted by every live
    /// connection's path watcher (see [`AddrSink`]).
    pub fn set_addr_sink(&self, sink: AddrSink) {
        *self
            .addr_sink
            .0
            .lock()
            .expect("addr sink lock poisoned") = Some(sink);
    }

    /// Drops the cached connection to `peer`, closing it. The next
    /// [`Transport::open`] dials fresh. Callers use this after a
    /// request died on a pooled connection (a restarted peer's zombie)
    /// to retry on a new dial instead of waiting out the idle timeout
    /// again.
    pub async fn evict(&self, peer: PeerId) {
        if let Some(live) = self.conns.lock().await.remove(&peer) {
            live.conn.close(0u32.into(), b"evicted");
        }
    }

    /// The current dial addresses for `peer`: the static book plus the
    /// live source, deduplicated.
    async fn dial_addrs(&self, peer: PeerId) -> Vec<SocketAddr> {
        let mut addrs: Vec<SocketAddr> = self
            .addr_book
            .lock()
            .expect("addr book lock poisoned")
            .get(&peer)
            .cloned()
            .unwrap_or_default();
        if let Some(source) = self.addr_source.get() {
            addrs.extend(source(peer).await);
        }
        addrs.sort();
        addrs.dedup();
        addrs
    }

    /// How many outbound connections were dialed; reusing a cached
    /// connection does not count.
    pub fn dial_count(&self) -> u64 {
        self.dials.load(Ordering::Relaxed)
    }

    /// How many inbound connections were accepted.
    pub fn connections_accepted(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }

    /// Stops the router (and gossip, when served) and closes the
    /// endpoint.
    pub async fn shutdown(&self) {
        if let Some(gossip) = &self.gossip
            && let Err(e) = gossip.shutdown().await
        {
            debug!(error = %e, "gossip shutdown error");
        }
        if let Err(e) = self.router.shutdown().await {
            warn!(error = %e, "router shutdown join error");
        }
        if let Some(store) = &self.blobs
            && let Err(e) = store.shutdown().await
        {
            debug!(error = %e, "blob store shutdown error");
        }
        self.endpoint.close().await;
    }

    /// Dials `peer` and exchanges hellos on the first bi stream,
    /// bounded by [`DIAL_TIMEOUT`]: stale addresses plus a stale
    /// discovery record must fail fast, not hang the caller.
    async fn dial(&self, peer: PeerId) -> Result<LiveConn, TransportError> {
        match tokio::time::timeout(DIAL_TIMEOUT, self.dial_inner(peer)).await {
            Ok(res) => res,
            Err(_) => Err(TransportError::Dial(format!(
                "dialing {peer} timed out after {}s",
                DIAL_TIMEOUT.as_secs()
            ))),
        }
    }

    async fn dial_inner(&self, peer: PeerId) -> Result<LiveConn, TransportError> {
        let endpoint_id = EndpointId::from_bytes(peer.as_bytes())
            .map_err(|e| TransportError::InvalidPeerId(e.to_string()))?;
        let manual = self.dial_addrs(peer).await;
        let addr =
            EndpointAddr::new(endpoint_id).with_addrs(manual.into_iter().map(TransportAddr::Ip));

        self.dials.fetch_add(1, Ordering::Relaxed);
        let conn = self
            .endpoint
            .connect(addr, ALPN)
            .await
            .map_err(|e| TransportError::Dial(e.to_string()))?;

        // One hello each way on the first bi stream; the dialer speaks
        // first.
        let (send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| TransportError::Handshake(format!("open hello stream: {e}")))?;
        let mut hello_stream = IrohRequestStream::new(send, recv);
        hello_stream
            .send(&EnvelopeObject::Hello(self.local_hello.clone()))
            .await?;
        hello_stream.finish().await?;
        let hello = match hello_stream.recv().await {
            Ok(Some(EnvelopeObject::Hello(h))) => h,
            Ok(Some(EnvelopeObject::Generic(g))) if g.class == "Refused" => {
                let (code, reason) = parse_refused(&g);
                return Err(TransportError::Refused { code, reason });
            }
            Ok(Some(other)) => {
                return Err(TransportError::Handshake(format!(
                    "expected hello, got {other:?}"
                )));
            }
            Ok(None) => {
                return Err(TransportError::Handshake(
                    "stream closed before the peer's hello".into(),
                ));
            }
            Err(e) => return Err(TransportError::Handshake(e.to_string())),
        };
        if let Some((code, reason)) = hello_incompatibility(&hello) {
            conn.close(2u32.into(), code.as_bytes());
            return Err(TransportError::Refused {
                code: code.to_string(),
                reason,
            });
        }
        debug!(peer = %peer, mods = ?hello.mods, "dialed and exchanged hellos");
        Ok(LiveConn { conn, hello })
    }
}

impl Transport for IrohTransport {
    type Stream = IrohRequestStream;

    fn local_id(&self) -> PeerId {
        self.peer_id()
    }

    async fn open(&self, peer: PeerId) -> Result<(IrohRequestStream, Hello), TransportError> {
        let mut conns = self.conns.lock().await;
        if let Some(live) = conns.get(&peer) {
            if live.conn.close_reason().is_none() {
                match live.conn.open_bi().await {
                    Ok((send, recv)) => {
                        return Ok((IrohRequestStream::new(send, recv), live.hello.clone()));
                    }
                    Err(e) => {
                        debug!(peer = %peer, error = %e, "cached connection unusable, redialing");
                    }
                }
            }
            conns.remove(&peer);
        }
        let live = self.dial(peer).await?;
        let (send, recv) = live
            .conn
            .open_bi()
            .await
            .map_err(|e| TransportError::Stream(format!("open request stream: {e}")))?;
        let hello = live.hello.clone();
        watch_paths(live.conn.clone(), peer, self.addr_sink.clone());
        conns.insert(peer, live);
        Ok((IrohRequestStream::new(send, recv), hello))
    }
}

// ---------------------------------------------------------------------------
// Blobs (file transfer, chat protocol sec 6)
// ---------------------------------------------------------------------------

/// The iroh-blobs `ProtocolHandler` wrapped in the peer gate: a dialer
/// the gate refuses is closed before the provider sees a request.
struct GatedBlobs {
    inner: iroh_blobs::BlobsProtocol,
    gate: Option<BlobGate>,
}

impl std::fmt::Debug for GatedBlobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatedBlobs")
            .field("gate", &self.gate.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl ProtocolHandler for GatedBlobs {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if let Some(gate) = &self.gate {
            let peer = PeerId(*connection.remote_id().as_bytes());
            if !gate(peer).await {
                debug!(peer = %peer, "blob connection refused by the peer gate");
                connection.close(2u32.into(), b"not admitted");
                return Ok(());
            }
        }
        self.inner.accept(connection).await
    }

    async fn shutdown(&self) {
        // The store belongs to the transport, which flushes it in its
        // own shutdown; nothing to do per handler.
    }
}

/// Parses a chat-protocol blob hash: 64-hex, with or without the
/// `blake3:` prefix. Validated here because `Hash::from_str` asserts on
/// unexpected lengths instead of erroring.
pub fn parse_blob_hash(s: &str) -> Result<iroh_blobs::Hash, TransportError> {
    let hex = s.strip_prefix("blake3:").unwrap_or(s).trim();
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(TransportError::Blobs(format!(
            "invalid blob hash {s:?}: expected 64 hex chars (blake3: prefix optional)"
        )));
    }
    hex.to_ascii_lowercase()
        .parse::<iroh_blobs::Hash>()
        .map_err(|e| TransportError::Blobs(format!("invalid blob hash {s:?}: {e}")))
}

/// Imports one file into `store`, pinning it under a tag; returns
/// `(64-hex hash, exact byte size)`.
async fn blob_add_to_store(
    store: &iroh_blobs::api::Store,
    path: &std::path::Path,
) -> Result<(String, u64), TransportError> {
    let size = std::fs::metadata(path)
        .map_err(|e| TransportError::Blobs(format!("reading {}: {e}", path.display())))?
        .len();
    let abs = std::path::absolute(path)
        .map_err(|e| TransportError::Blobs(format!("resolving {}: {e}", path.display())))?;
    let tag = store
        .blobs()
        .add_path(&abs)
        .await
        .map_err(|e| TransportError::Blobs(format!("importing {}: {e}", path.display())))?;
    Ok((tag.hash.to_hex(), size))
}

/// Fetches `hash` over `conn` into `store` (verified streaming; already
/// local ranges are skipped) and exports the bytes to `target`.
async fn fetch_and_export(
    store: &iroh_blobs::api::Store,
    conn: Connection,
    hash: iroh_blobs::Hash,
    target: &std::path::Path,
) -> Result<u64, TransportError> {
    store
        .remote()
        .fetch(conn, hash)
        .complete()
        .await
        .map_err(|e| TransportError::Blobs(format!("fetch failed: {e}")))?;
    store
        .blobs()
        .export(hash, target)
        .finish()
        .await
        .map_err(|e| TransportError::Blobs(format!("exporting to {}: {e}", target.display())))
}

/// Fetches `hash` over `conn` into `store` and reads the verified bytes.
async fn fetch_and_read(
    store: &iroh_blobs::api::Store,
    conn: Connection,
    hash: iroh_blobs::Hash,
) -> Result<Vec<u8>, TransportError> {
    store
        .remote()
        .fetch(conn, hash)
        .complete()
        .await
        .map_err(|e| TransportError::Blobs(format!("fetch failed: {e}")))?;
    store
        .blobs()
        .get_bytes(hash)
        .await
        .map(|b| b.to_vec())
        .map_err(|e| TransportError::Blobs(format!("reading fetched blob: {e}")))
}

/// How long [`blob_import`] waits for the exclusive store lock before
/// concluding another process (a serving node) holds it.
const BLOB_IMPORT_OPEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Imports a file into the blob store at `store_dir` without binding any
/// endpoint (the `rsntr chat send --file` path). Returns `(64-hex hash,
/// exact byte size)`. The store is exclusive: while another process (a
/// serving node) holds it, the open would block on the store lock
/// forever, so it runs on its own task under a bounded wait and reports
/// the lock as an error instead of hanging.
pub async fn blob_import(
    store_dir: &std::path::Path,
    file: &std::path::Path,
) -> Result<(String, u64), TransportError> {
    let dir = store_dir.to_path_buf();
    let load = tokio::spawn(async move { iroh_blobs::store::fs::FsStore::load(&dir).await });
    let store = match tokio::time::timeout(BLOB_IMPORT_OPEN_TIMEOUT, load).await {
        Err(_) => {
            return Err(TransportError::Blobs(format!(
                "the blob store at {} is locked, most likely by a node serving this \
                 directory; stop `rsntr serve` here before importing attachments",
                store_dir.display()
            )));
        }
        Ok(joined) => joined
            .map_err(|e| TransportError::Blobs(format!("blob store open task: {e}")))?
            .map_err(|e| TransportError::Blobs(format!("opening the blob store: {e}")))?,
    };
    let result = blob_add_to_store(&store, file).await;
    let _ = store.shutdown().await;
    result
}

// ---------------------------------------------------------------------------
// Accepting
// ---------------------------------------------------------------------------

/// The `ProtocolHandler` behind `resonator/rdf/0`.
#[derive(Debug, Clone)]
struct Acceptor {
    local_hello: Hello,
    incoming: mpsc::Sender<IncomingRequest<IrohRequestStream>>,
    accepted: Arc<AtomicU64>,
    addr_sink: Arc<AddrSinkSlot>,
}

impl ProtocolHandler for Acceptor {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let peer = PeerId(*connection.remote_id().as_bytes());
        self.accepted.fetch_add(1, Ordering::Relaxed);

        // QUIC hands us streams in creation order and the dialer opens
        // its hello stream before any request stream, so the first
        // accepted bi stream is the hello stream.
        let (send, recv) = match connection.accept_bi().await {
            Ok(pair) => pair,
            Err(e) => {
                debug!(peer = %peer, error = %e, "connection closed before hello");
                return Ok(());
            }
        };
        let mut hello_stream = IrohRequestStream::new(send, recv);

        // Section 4bis: a caller that does not speak frames (a human, or
        // an AI handed a raw socket) opens with bare text such as
        // "HELP\n". Peek the raw bytes; on a probe, answer the unframed
        // banner and keep waiting for a real hello frame.
        loop {
            match hello_stream.plaintext_probe().await {
                Ok(true) => {
                    debug!(peer = %peer, "answered plain-text probe with banner");
                }
                Ok(false) => break,
                Err(e) => {
                    debug!(peer = %peer, error = %e, "probe read failed before hello");
                    return Ok(());
                }
            }
        }

        let peer_hello = match hello_stream.recv().await {
            Ok(Some(EnvelopeObject::Hello(h))) => h,
            other => {
                warn!(peer = %peer, got = ?other, "connection did not start with a hello");
                connection.close(1u32.into(), b"expected hello");
                return Ok(());
            }
        };

        // Handshake refusal (connection protocol sec 5): rsntr:Refused
        // in place of a hello, then close.
        if let Some((code, reason)) = hello_incompatibility(&peer_hello) {
            debug!(peer = %peer, code, "refusing hello");
            if let Err(e) = hello_stream.send(&refused_envelope(code, &reason)).await {
                debug!(peer = %peer, error = %e, "failed to send refusal");
            }
            let _ = hello_stream.finish().await;
            // No immediate close(): that can throw away the refusal
            // frame before it is delivered. The refused dialer drops the
            // connection after reading it; bound the wait regardless.
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(10), connection.closed()).await;
            return Ok(());
        }

        if let Err(e) = hello_stream
            .send(&EnvelopeObject::Hello(self.local_hello.clone()))
            .await
        {
            debug!(peer = %peer, error = %e, "failed to answer hello");
            return Ok(());
        }
        let _ = hello_stream.finish().await;
        debug!(peer = %peer, mods = ?peer_hello.mods, "accepted connection, hellos exchanged");
        // An inbound connection is the freshest address information
        // there is: report the peer's live direct addresses so the
        // owner can keep its dial hints current across peer restarts.
        watch_paths(connection.clone(), peer, self.addr_sink.clone());

        // From here on: one bi stream per request, served concurrently.
        loop {
            let (send, recv) = match connection.accept_bi().await {
                Ok(pair) => pair,
                Err(e) => {
                    debug!(peer = %peer, reason = %e, "connection done");
                    break;
                }
            };
            tokio::spawn(route_request(
                IrohRequestStream::new(send, recv),
                peer,
                peer_hello.clone(),
                self.local_hello.mods.clone(),
                self.incoming.clone(),
            ));
        }
        Ok(())
    }
}

/// Reads a request stream's first frame, applies the transport-level
/// modulation gate, and hands everything else to the node.
async fn route_request(
    mut stream: IrohRequestStream,
    peer: PeerId,
    peer_hello: Hello,
    mods: Vec<String>,
    incoming: mpsc::Sender<IncomingRequest<IrohRequestStream>>,
) {
    let first = match stream.recv().await {
        Ok(Some(obj)) => obj,
        Ok(None) => return,
        Err(e) => {
            debug!(peer = %peer, error = %e, "request stream failed before first frame");
            return;
        }
    };

    // Fail fast before the authenticator ever runs: the envelope doc's
    // mod gate, plus the rule that hello never appears on later streams.
    let fast_fail = match &first {
        EnvelopeObject::Query(s) | EnvelopeObject::Execute(s) => {
            let served = mods
                .iter()
                .any(|t| resonator_protocol::mod_matches(&s.modulation, t));
            (!served).then(|| ErrorEnvelope {
                id: Some(s.id.clone()),
                code: ErrorCode::ModUnsupported.as_str().to_string(),
                reason: Some(format!(
                    "modulation {:?} not served; this node demodulates [{}]",
                    s.modulation,
                    mods.join(", ")
                )),
            })
        }
        EnvelopeObject::Hello(_) => Some(ErrorEnvelope {
            id: None,
            code: ErrorCode::ProtocolError.as_str().to_string(),
            reason: Some("hello is only valid on the first stream of a connection".into()),
        }),
        _ => None,
    };
    if let Some(err) = fast_fail {
        debug!(peer = %peer, code = %err.code, "fast-failing request at the transport");
        if let Err(e) = stream.send(&EnvelopeObject::Error(err)).await {
            debug!(peer = %peer, error = %e, "failed to send fast-fail error");
        }
        let _ = stream.finish().await;
        return;
    }

    let request = IncomingRequest {
        peer,
        peer_hello,
        first,
        stream,
    };
    if incoming.send(request).await.is_err() {
        debug!(peer = %peer, "incoming channel closed, dropping request");
    }
}

// ---------------------------------------------------------------------------
// The stream type
// ---------------------------------------------------------------------------

/// Frames in both directions over one iroh (QUIC) bi stream.
#[derive(Debug)]
pub struct IrohRequestStream {
    send: SendStream,
    recv: RecvStream,
    buf: BytesMut,
    eof: bool,
}

impl IrohRequestStream {
    fn new(send: SendStream, recv: RecvStream) -> Self {
        Self {
            send,
            recv,
            buf: BytesMut::new(),
            eof: false,
        }
    }

    /// Reads the first raw chunk and decides: real frame, or plain-text
    /// probe (connection-protocol.md section 4bis)?
    ///
    /// Probe: the buffered bytes cannot be a length-prefixed frame but
    /// are printable UTF-8. The unframed banner goes back, the probe
    /// bytes are dropped, and `Ok(true)` says "await the next chunk".
    /// Frame: the bytes stay buffered for [`recv`](RequestStream::recv)
    /// and `Ok(false)` is returned; clean EOF is also `Ok(false)`.
    async fn plaintext_probe(&mut self) -> Result<bool, TransportError> {
        let mut chunk = [0u8; READ_CHUNK];
        match self.recv.read(&mut chunk).await {
            Ok(Some(n)) => self.buf.extend_from_slice(&chunk[..n]),
            Ok(None) => {
                self.eof = true;
                return Ok(false);
            }
            Err(e) => return Err(TransportError::Stream(e.to_string())),
        }
        if !looks_like_probe(&self.buf) {
            return Ok(false);
        }
        self.send
            .write_all(PLAINTEXT_BANNER.as_bytes())
            .await
            .map_err(|e| TransportError::Stream(e.to_string()))?;
        // Probe bytes live outside the framed protocol; discard them and
        // let the caller keep the stream open for a real hello frame.
        self.buf.clear();
        Ok(true)
    }
}

/// Do these opening bytes look like a plain-text probe instead of a
/// length-prefixed frame? A frame starts with a u32-LE length at most
/// [`MAX_FRAME_LEN`]; printable ASCII like "HELP\n" reads as an absurd
/// (~1.3 GB) length, and a lone "?" is shorter than any prefix. Bytes
/// that cannot be a frame prefix and are printable UTF-8 count as a
/// probe.
fn looks_like_probe(buf: &[u8]) -> bool {
    if buf.len() >= 4 {
        let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if len <= MAX_FRAME_LEN {
            // A believable frame prefix: not a probe.
            return false;
        }
    }
    // Either too short for a prefix or the prefix is impossible. Only
    // printable UTF-8 text qualifies as a probe.
    match std::str::from_utf8(buf) {
        Ok(s) => !s.is_empty() && s.chars().all(|c| !c.is_control() || c.is_whitespace()),
        Err(_) => false,
    }
}

impl RequestStream for IrohRequestStream {
    async fn send(&mut self, obj: &EnvelopeObject) -> Result<(), TransportError> {
        let mut out = BytesMut::new();
        encode_envelope(obj, &mut out)?;
        self.send
            .write_all(&out)
            .await
            .map_err(|e| TransportError::Stream(e.to_string()))
    }

    async fn recv(&mut self) -> Result<Option<EnvelopeObject>, TransportError> {
        loop {
            if let Some(obj) = decode_envelope(&mut self.buf)? {
                return Ok(Some(obj));
            }
            if self.eof {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                return Err(TransportError::Stream(format!(
                    "stream ended mid-frame with {} trailing bytes",
                    self.buf.len()
                )));
            }
            let mut chunk = [0u8; READ_CHUNK];
            match self.recv.read(&mut chunk).await {
                Ok(Some(n)) => self.buf.extend_from_slice(&chunk[..n]),
                Ok(None) => self.eof = true,
                Err(e) => return Err(TransportError::Stream(e.to_string())),
            }
        }
    }

    async fn send_raw(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.send
            .write_all(bytes)
            .await
            .map_err(|e| TransportError::Stream(e.to_string()))
    }

    async fn closed(&mut self) {
        // Resolves on STOP_SENDING, stream reset, or the connection dying;
        // any of those means the peer is done with this feed.
        let _ = self.send.stopped().await;
    }

    /// Anything already buffered (bytes read past the framed part)
    /// first, then raw reads off the wire. The media modulation's client
    /// side calls this after the `rsntr:Media` header frame.
    async fn recv_raw(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        if !self.buf.is_empty() {
            return Ok(Some(self.buf.split().to_vec()));
        }
        if self.eof {
            return Ok(None);
        }
        let mut chunk = [0u8; READ_CHUNK];
        match self.recv.read(&mut chunk).await {
            Ok(Some(n)) => Ok(Some(chunk[..n].to_vec())),
            Ok(None) => {
                self.eof = true;
                Ok(None)
            }
            Err(e) => Err(TransportError::Stream(e.to_string())),
        }
    }

    async fn finish(&mut self) -> Result<(), TransportError> {
        // A ClosedStream error just means finish already happened.
        let _ = self.send.finish();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_heuristic() {
        for probe in [
            b"HELP\n".as_slice(),
            b"help\n",
            b"?",
            b"hello there, what are you?",
        ] {
            assert!(looks_like_probe(probe), "{probe:?} should be a probe");
        }
        // A believable frame prefix is not a probe.
        let mut framed = 12u32.to_le_bytes().to_vec();
        framed.extend_from_slice(b"[] a rsntr:X");
        assert!(!looks_like_probe(&framed));
        // Neither is binary garbage or nothing at all.
        assert!(!looks_like_probe(&[0xFF, 0xFE, 0xFD, 0xFC, 0xFB]));
        assert!(!looks_like_probe(b""));
    }

    #[test]
    fn refused_round_trip() {
        let obj = refused_envelope("envelope-version", "0.x only");
        let mut buf = BytesMut::new();
        encode_envelope(&obj, &mut buf).expect("encode");
        let back = decode_envelope(&mut buf).expect("decode").expect("some");
        let EnvelopeObject::Generic(g) = back else {
            panic!("expected Generic(Refused)");
        };
        assert_eq!(g.class, "Refused");
        assert_eq!(
            parse_refused(&g),
            ("envelope-version".to_string(), "0.x only".to_string())
        );
    }

    #[test]
    fn hello_compat_rules() {
        let ok = crate::basic_hello(&["help"], None);
        assert!(hello_incompatibility(&ok).is_none());

        let mut bad_ver = ok.clone();
        bad_ver.envelope_version = "9.0".to_string();
        assert_eq!(
            hello_incompatibility(&bad_ver).map(|(c, _)| c),
            Some("envelope-version")
        );

        let mut bad_enc = ok.clone();
        bad_enc.encodings = vec!["compact-postcard".to_string()];
        assert_eq!(
            hello_incompatibility(&bad_enc).map(|(c, _)| c),
            Some("encoding")
        );
    }

    #[test]
    fn endpoint_id_from_secret_is_deterministic() {
        let a = endpoint_id_from_secret([7u8; 32]);
        let b = endpoint_id_from_secret([7u8; 32]);
        let c = endpoint_id_from_secret([8u8; 32]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
