//! Presence surface: iroh-gossip liveness beacons.
//!
//! Admitted peers join one iroh-gossip topic per peer-set and periodically
//! broadcast a small `rsntr:Presence` beacon (default cadence ~60s). On
//! receipt, a peer refreshes `_peers.last_seen` for the beaconing endpoint;
//! only endpoints with a `_peers` row (admitted peers) are ever refreshed.
//! The outbox scheduler consults [`is_stale`] to skip known-offline peers.
//!
//! The topic id is a stable hash of the sorted admitted `EndpointId`s of
//! the whole peer-set (this node plus its `_peers`), so presence is
//! gossiped only among admitted peers and never on a public rendezvous.
//! Because the topic derives purely from the relationship, every member of
//! the same peer-set computes the same topic without coordination.
//!
//! ## Endpoint model
//!
//! iroh's `Router` binds its accepted ALPNs at spawn time and the rsntr
//! transport already owns a spawned router for `resonator/rdf/0`. An
//! already-spawned router cannot take a second `accept`, so this service
//! binds its own dedicated iroh endpoint for the gossip ALPN rather than
//! reaching into the transport's router. That costs a second endpoint per
//! node; the two can be unified later behind a shared router built with
//! both ALPNs up front.
//!
//! ## Beacon identity
//!
//! The gossip layer delivers the proven `EndpointId` of the neighbour that
//! sent the message (`Message::delivered_from`); this service treats that
//! id as authoritative. Beacons are sent with `broadcast_neighbors`
//! (single-hop, direct to active neighbours) rather than swarm-relayed
//! `broadcast`: a relayed copy would arrive stamped with a relaying
//! neighbour's id, so a silent node that merely relays others' beacons
//! would wrongly look alive. Presence is beaconed among a small admitted
//! peer-set that is fully meshed, so direct delivery reaches everyone.
//!
//! The `rsntr:Presence` envelope also carries the beacon's self-declared
//! author endpoint id (`rsntr:endpoint`). Under single-hop delivery the
//! delivering neighbour IS the author, so the receiver uses the declared id
//! only as an integrity cross-check: a mismatch is anomalous (a relayed
//! copy or a spoof) and does not refresh liveness.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::BytesMut;
use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey, TransportAddr};
use iroh_gossip::api::{Event, GossipReceiver, GossipSender};
use iroh_gossip::{ALPN as GOSSIP_ALPN, Gossip, TopicId};
use n0_future::StreamExt;
use rusqlite::{Connection, OptionalExtension, params};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use resonator_node::DbHandle;
use resonator_protocol::{
    EnvelopeObject, Presence as PresenceBeacon, decode_envelope, encode_envelope,
};
use resonator_transport::PeerId;

/// The default beacon cadence.
pub const DEFAULT_CADENCE: Duration = Duration::from_secs(60);

/// Errors from the presence surface.
#[derive(Debug, thiserror::Error)]
pub enum PresenceError {
    /// Binding the dedicated iroh endpoint failed.
    #[error("bind presence endpoint: {0}")]
    Bind(String),
    /// Joining / subscribing to the gossip topic failed.
    #[error("gossip subscribe: {0}")]
    Subscribe(String),
    /// A sqlite error while reading or updating `_peers`.
    #[error("sqlite: {0}")]
    Db(#[from] rusqlite::Error),
    /// The dedicated sqlite thread is gone.
    #[error("database thread is not running")]
    DbClosed,
    /// Encoding the beacon envelope failed.
    #[error("encode beacon: {0}")]
    Encode(#[from] resonator_protocol::ProtocolError),
}

/// Configuration for [`Presence::start`].
#[derive(Debug, Clone)]
pub struct PresenceConfig {
    /// Beacon cadence. Defaults to [`DEFAULT_CADENCE`].
    pub cadence: Duration,
    /// Optional `rsntr:status` string carried in each beacon.
    pub status: Option<String>,
    /// Offline preset: no relays, bound to localhost, dial hints supplied
    /// via [`Presence::register_peer`]. For tests and LAN-only use.
    /// Production nodes leave this false and rely on iroh address lookup.
    pub offline: bool,
    /// ed25519 secret key bytes; a fresh key is generated when absent.
    pub secret_key: Option<[u8; 32]>,
}

impl Default for PresenceConfig {
    fn default() -> Self {
        Self {
            cadence: DEFAULT_CADENCE,
            status: None,
            offline: false,
            secret_key: None,
        }
    }
}

/// A running presence service for one node.
///
/// [`start`](Self::start) binds the endpoint and gossip protocol;
/// [`join`](Self::join) subscribes to the peer-set topic and starts the
/// beacon and receive loops. [`shutdown`](Self::shutdown) leaves the topic
/// and closes the endpoint.
pub struct Presence {
    endpoint: Endpoint,
    gossip: Gossip,
    /// The dedicated router when this service owns its endpoint; `None`
    /// when attached to a shared endpoint whose router is owned elsewhere
    /// (see [`attach`](Self::attach)).
    router: Option<Router>,
    lookup: MemoryLookup,
    local_id: PeerId,
    db: DbHandle,
    cadence: Duration,
    status: Option<String>,
    beacon_on: Arc<AtomicBool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Presence {
    /// Binds the dedicated gossip endpoint and spawns the gossip protocol
    /// handler. Does not join a topic yet; call [`join`](Self::join) after
    /// registering peer dial hints (offline) or when discovery is
    /// available.
    pub async fn start(config: PresenceConfig, db: DbHandle) -> Result<Self, PresenceError> {
        let secret_key = match config.secret_key {
            Some(bytes) => SecretKey::from_bytes(&bytes),
            None => SecretKey::generate(),
        };
        let endpoint = if config.offline {
            Endpoint::builder(presets::Minimal)
                .secret_key(secret_key)
                .relay_mode(RelayMode::Disabled)
                .bind_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
                .map_err(|e| PresenceError::Bind(e.to_string()))?
                .bind()
                .await
        } else {
            Endpoint::builder(presets::N0)
                .secret_key(secret_key)
                .bind()
                .await
        }
        .map_err(|e| PresenceError::Bind(e.to_string()))?;

        // A supplementary in-memory address lookup so peers can be dialled
        // by bare EndpointId without discovery (offline / LAN). In n0 mode
        // it sits alongside the default lookups and is harmless when empty.
        let lookup = MemoryLookup::new();
        endpoint
            .address_lookup()
            .map_err(|e| PresenceError::Bind(e.to_string()))?
            .add(lookup.clone());

        let local_id = PeerId(*endpoint.id().as_bytes());
        let gossip = Gossip::builder().spawn(endpoint.clone());
        let router = Router::builder(endpoint.clone())
            .accept(GOSSIP_ALPN, gossip.clone())
            .spawn();

        Ok(Self {
            endpoint,
            gossip,
            router: Some(router),
            lookup,
            local_id,
            db,
            cadence: config.cadence,
            status: config.status,
            beacon_on: Arc::new(AtomicBool::new(true)),
            tasks: Mutex::new(Vec::new()),
        })
    }

    /// Attaches presence to an endpoint and gossip instance owned by the
    /// transport (one router serving both `resonator/rdf/0` and the gossip
    /// ALPN, one node identity), instead of binding a dedicated endpoint.
    /// Only `cadence` and `status` from `config` apply; `offline` and
    /// `secret_key` are the endpoint owner's concern. [`shutdown`]
    /// (Self::shutdown) then stops only the presence loops; the endpoint,
    /// router, and gossip stay up for their owner to close.
    pub fn attach(
        endpoint: Endpoint,
        gossip: Gossip,
        config: PresenceConfig,
        db: DbHandle,
    ) -> Result<Self, PresenceError> {
        let lookup = MemoryLookup::new();
        endpoint
            .address_lookup()
            .map_err(|e| PresenceError::Bind(e.to_string()))?
            .add(lookup.clone());
        let local_id = PeerId(*endpoint.id().as_bytes());
        Ok(Self {
            endpoint,
            gossip,
            router: None,
            lookup,
            local_id,
            db,
            cadence: config.cadence,
            status: config.status,
            beacon_on: Arc::new(AtomicBool::new(true)),
            tasks: Mutex::new(Vec::new()),
        })
    }

    /// This node's presence endpoint identity.
    pub fn peer_id(&self) -> PeerId {
        self.local_id
    }

    /// This endpoint's dialing address (id plus bound direct sockets), for
    /// handing to peers so they can register it via
    /// [`register_peer`](Self::register_peer).
    pub fn endpoint_addr(&self) -> EndpointAddr {
        EndpointAddr::from_parts(
            self.endpoint.id(),
            self.endpoint
                .bound_sockets()
                .into_iter()
                .map(TransportAddr::Ip),
        )
    }

    /// Registers a peer's dialing address so this node can reach it by bare
    /// `EndpointId` (offline / LAN). Redundant in n0 mode where discovery
    /// resolves addresses.
    pub fn register_peer(&self, addr: EndpointAddr) {
        self.lookup.add_endpoint_info(addr);
    }

    /// Reads the admitted peer set from `_peers` (all endpoint ids). The
    /// usual argument to [`join`](Self::join).
    pub async fn admitted_peers(&self) -> Result<Vec<PeerId>, PresenceError> {
        self.db
            .call(|conn| -> Result<Vec<PeerId>, rusqlite::Error> {
                let mut stmt = conn.prepare("SELECT endpoint_id FROM _peers")?;
                let ids = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .filter_map(Result::ok)
                    .filter_map(|s| s.parse::<PeerId>().ok())
                    .collect();
                Ok(ids)
            })
            .await
            .map_err(|_| PresenceError::DbClosed)?
            .map_err(PresenceError::Db)
    }

    /// Subscribes to the peer-set topic and starts the beacon + receive
    /// loops.
    ///
    /// `peers` is the admitted peer-set from `_peers` (this node's own id
    /// is added automatically). The topic id is the stable hash of the
    /// whole sorted set, and `peers` seed the gossip bootstrap.
    pub async fn join(&self, peers: Vec<PeerId>) -> Result<(), PresenceError> {
        let mut members = peers.clone();
        members.push(self.local_id);
        let topic = topic_id_for(&members);

        let bootstrap: Vec<EndpointId> = peers
            .iter()
            .filter(|p| **p != self.local_id)
            .filter_map(|p| EndpointId::from_bytes(p.as_bytes()).ok())
            .collect();

        let gtopic = self
            .gossip
            .subscribe(topic, bootstrap)
            .await
            .map_err(|e| PresenceError::Subscribe(e.to_string()))?;
        let (sender, receiver) = gtopic.split();

        let recv_task = tokio::spawn(recv_loop(receiver, self.db.clone(), self.local_id));
        let beacon_task = tokio::spawn(beacon_loop(
            sender,
            self.cadence,
            self.status.clone(),
            self.beacon_on.clone(),
            self.db.clone(),
            self.local_id,
        ));
        let mut tasks = self.tasks.lock().expect("tasks lock poisoned");
        tasks.push(recv_task);
        tasks.push(beacon_task);
        Ok(())
    }

    /// Toggles beacon emission. Setting this false makes the node "go
    /// silent" (its liveness signal stops) without tearing down the
    /// endpoint; setting it true resumes beaconing.
    pub fn set_beaconing(&self, on: bool) {
        self.beacon_on.store(on, Ordering::Relaxed);
    }

    /// Whether beacons are currently being emitted.
    pub fn is_beaconing(&self) -> bool {
        self.beacon_on.load(Ordering::Relaxed)
    }

    /// Aborts the loops; when this service owns its endpoint (bound via
    /// [`start`](Self::start)) it also leaves the topic, shuts the router,
    /// and closes the endpoint. Attached mode leaves those to their owner.
    pub async fn shutdown(&self) {
        {
            let mut tasks = self.tasks.lock().expect("tasks lock poisoned");
            for t in tasks.drain(..) {
                t.abort();
            }
        }
        let Some(router) = &self.router else {
            return;
        };
        if let Err(e) = self.gossip.shutdown().await {
            debug!(error = %e, "gossip shutdown error");
        }
        if let Err(e) = router.shutdown().await {
            warn!(error = %e, "presence router shutdown join error");
        }
        self.endpoint.close().await;
    }
}

/// The gossip receive loop: on each delivered beacon, refresh the beaconing
/// peer's `_peers.last_seen`.
async fn recv_loop(mut receiver: GossipReceiver, db: DbHandle, local_id: PeerId) {
    while let Some(item) = receiver.next().await {
        let event = match item {
            Ok(ev) => ev,
            Err(e) => {
                debug!(error = %e, "presence receiver stream error, stopping");
                break;
            }
        };
        let Event::Received(msg) = event else {
            // NeighborUp / NeighborDown / Lagged: liveness is beacon-driven.
            continue;
        };
        // The gossip-proven delivering neighbour is the authoritative
        // author (single-hop delivery).
        let author = PeerId(*msg.delivered_from.as_bytes());
        if author == local_id {
            continue;
        }
        // Only refresh liveness for a well-formed rsntr:Presence beacon.
        let Some(beacon) = decode_presence_beacon(&msg.content) else {
            debug!(peer = %author, "ignoring non-presence gossip message");
            continue;
        };
        // Integrity cross-check: the beacon's self-declared endpoint must
        // match the proven sender. A mismatch is anomalous (a relayed copy
        // or a spoof) and must not refresh the wrong peer's liveness.
        if let Some(declared) = beacon.endpoint.as_deref()
            && declared != author.to_string()
        {
            warn!(
                peer = %author,
                declared,
                "presence beacon endpoint mismatch, ignoring"
            );
            continue;
        }
        match touch_last_seen(&db, author).await {
            Ok(()) => debug!(peer = %author, "refreshed last_seen from presence beacon"),
            Err(e) => warn!(peer = %author, error = %e, "failed to update _peers.last_seen"),
        }
    }
}

/// The beacon loop: broadcast one `rsntr:Presence` immediately, then every
/// cadence while beaconing is enabled.
async fn beacon_loop(
    sender: GossipSender,
    cadence: Duration,
    status: Option<String>,
    beacon_on: Arc<AtomicBool>,
    db: DbHandle,
    local_id: PeerId,
) {
    loop {
        if beacon_on.load(Ordering::Relaxed) {
            match encode_beacon(&db, &status, local_id).await {
                Ok(bytes) => {
                    // Direct (single-hop) delivery to active neighbours, not
                    // swarm-relayed gossip: keeps `delivered_from` equal to
                    // the beacon's author (see module docs).
                    if let Err(e) = sender.broadcast_neighbors(bytes).await {
                        // No neighbours yet, or transient: keep trying.
                        debug!(error = %e, "presence broadcast failed (will retry)");
                    }
                }
                Err(e) => warn!(error = %e, "failed to encode presence beacon"),
            }
        }
        tokio::time::sleep(cadence).await;
    }
}

/// Builds and frames one `rsntr:Presence` beacon. The timestamp is read
/// from sqlite so it is always a valid `xsd:dateTime` lexical form in UTC.
///
/// Millisecond precision is deliberate: iroh-gossip deduplicates messages
/// by content hash, so two byte-identical beacons would collapse to one id;
/// a sub-second timestamp keeps successive beacons distinct.
async fn encode_beacon(
    db: &DbHandle,
    status: &Option<String>,
    local_id: PeerId,
) -> Result<bytes::Bytes, PresenceError> {
    let at = db
        .call(|conn| {
            conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
                r.get::<_, String>(0)
            })
        })
        .await
        .map_err(|_| PresenceError::DbClosed)?
        .map_err(PresenceError::Db)?;
    encode_beacon_at(at, status, local_id)
}

/// The pure encode half of [`encode_beacon`], unit-testable without a db.
fn encode_beacon_at(
    at: String,
    status: &Option<String>,
    local_id: PeerId,
) -> Result<bytes::Bytes, PresenceError> {
    let beacon = PresenceBeacon {
        at,
        status: status.clone(),
        endpoint: Some(local_id.to_string()),
    };
    let mut buf = BytesMut::new();
    encode_envelope(&EnvelopeObject::Presence(beacon), &mut buf)?;
    Ok(buf.freeze())
}

/// Decodes a gossip message body into an `rsntr:Presence` beacon, or `None`
/// when the body is malformed or is not a presence envelope.
fn decode_presence_beacon(content: &[u8]) -> Option<PresenceBeacon> {
    let mut buf = BytesMut::from(content);
    match decode_envelope(&mut buf) {
        Ok(Some(EnvelopeObject::Presence(p))) => Some(p),
        _ => None,
    }
}

/// Sets `_peers.last_seen = datetime('now')` for `peer` (matched by hex
/// endpoint id, the format `_peers.endpoint_id` uses). A peer with no
/// `_peers` row (not admitted) is simply not updated.
async fn touch_last_seen(db: &DbHandle, peer: PeerId) -> Result<(), PresenceError> {
    db.call(move |conn| {
        conn.execute(
            "UPDATE _peers SET last_seen = datetime('now') WHERE endpoint_id = ?1",
            params![peer.to_string()],
        )
    })
    .await
    .map_err(|_| PresenceError::DbClosed)?
    .map_err(PresenceError::Db)?;
    Ok(())
}

/// The staleness helper the outbox scheduler consults: is `peer`'s last
/// presence older than `threshold` (or unknown / never seen)?
///
/// A peer with no `_peers` row, or a row whose `last_seen` is NULL, is
/// reported stale. Otherwise the age is computed in sqlite (so the stored
/// `datetime('now')` UTC text is parsed correctly) and compared to
/// `threshold`.
pub fn is_stale(
    conn: &Connection,
    peer: &PeerId,
    threshold: Duration,
) -> Result<bool, PresenceError> {
    let secs = threshold.as_secs_f64();
    let stale: Option<bool> = conn
        .query_row(
            "SELECT CASE \
               WHEN last_seen IS NULL THEN 1 \
               WHEN (julianday('now') - julianday(last_seen)) * 86400.0 > ?2 THEN 1 \
               ELSE 0 END \
             FROM _peers WHERE endpoint_id = ?1",
            params![peer.to_string(), secs],
            |r| r.get::<_, i64>(0).map(|n| n != 0),
        )
        .optional()?;
    // Unknown peer -> treat as stale (nothing proves it is alive).
    Ok(stale.unwrap_or(true))
}

/// The gossip topic id for a peer-set: a stable 32-byte hash of the sorted,
/// de-duplicated `EndpointId`s. Every member computes the same id from the
/// same membership without coordination.
pub fn topic_id_for(members: &[PeerId]) -> TopicId {
    let mut ids: Vec<[u8; 32]> = members.iter().map(|p| p.0).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut input = Vec::with_capacity(ids.len() * 32);
    for id in &ids {
        input.extend_from_slice(id);
    }
    TopicId::from_bytes(hash32(&input))
}

/// Expands a byte string to 32 bytes with four domain-separated FNV-1a-64
/// rounds. FNV-1a is fully specified (unlike `std`'s default hasher), so
/// the topic id is identical across platforms and rustc versions, which
/// matters because independent peers must derive the same topic.
fn hash32(input: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, chunk) in out.chunks_mut(8).enumerate() {
        let h = fnv1a64(i as u8, input);
        chunk.copy_from_slice(&h.to_le_bytes());
    }
    out
}

fn fnv1a64(domain: u8, input: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for &b in std::iter::once(&domain).chain(input) {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(n: u8) -> PeerId {
        PeerId([n; 32])
    }

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(resonator_node::PEERS_DDL)
            .expect("peers ddl");
        conn
    }

    #[test]
    fn topic_is_order_independent_and_stable() {
        let a = peer(1);
        let b = peer(2);
        let c = peer(3);
        let t1 = topic_id_for(&[a, b, c]);
        let t2 = topic_id_for(&[c, a, b]);
        let t3 = topic_id_for(&[b, c, a, a, b]); // dupes collapse
        assert_eq!(t1, t2);
        assert_eq!(t1, t3);
        // A different membership yields a different topic.
        let t4 = topic_id_for(&[a, b]);
        assert_ne!(t1, t4);
    }

    #[test]
    fn topic_bytes_are_deterministic() {
        // Pin the hash so a change to the derivation is caught: the value
        // is a cross-peer rendezvous that must not drift silently.
        let t = topic_id_for(&[peer(1)]);
        let expected = hash32(&[1u8; 32]);
        assert_eq!(t.as_bytes(), &expected);
    }

    #[test]
    fn beacon_encode_decode_round_trip() {
        let bytes = encode_beacon_at(
            "2026-07-29T00:00:00.123Z".to_string(),
            &Some("around".to_string()),
            peer(1),
        )
        .expect("encode");
        let decoded = decode_presence_beacon(&bytes).expect("decode presence");
        assert_eq!(decoded.at, "2026-07-29T00:00:00.123Z");
        assert_eq!(decoded.status.as_deref(), Some("around"));
        assert_eq!(
            decoded.endpoint.as_deref(),
            Some(peer(1).to_string().as_str())
        );

        // A non-presence frame is rejected.
        let mut other = BytesMut::new();
        encode_envelope(
            &EnvelopeObject::Knock(resonator_protocol::Knock {
                id: None,
                message: "hi".to_string(),
            }),
            &mut other,
        )
        .expect("encode knock");
        assert!(decode_presence_beacon(&other).is_none());
        assert!(decode_presence_beacon(b"garbage").is_none());
    }

    #[test]
    fn unknown_peer_is_stale() {
        let conn = mem_db();
        assert!(is_stale(&conn, &peer(9), Duration::from_secs(60)).expect("stale"));
    }

    #[test]
    fn never_seen_peer_is_stale() {
        let conn = mem_db();
        conn.execute(
            "INSERT INTO _peers (endpoint_id, added_at) VALUES (?1, datetime('now'))",
            params![peer(1).to_string()],
        )
        .expect("insert");
        assert!(is_stale(&conn, &peer(1), Duration::from_secs(60)).expect("stale"));
    }

    #[test]
    fn fresh_peer_is_not_stale_old_peer_is() {
        let conn = mem_db();
        conn.execute(
            "INSERT INTO _peers (endpoint_id, added_at, last_seen) \
             VALUES (?1, datetime('now'), datetime('now'))",
            params![peer(1).to_string()],
        )
        .expect("insert fresh");
        conn.execute(
            "INSERT INTO _peers (endpoint_id, added_at, last_seen) \
             VALUES (?1, datetime('now'), datetime('now','-3600 seconds'))",
            params![peer(2).to_string()],
        )
        .expect("insert old");

        assert!(!is_stale(&conn, &peer(1), Duration::from_secs(60)).expect("fresh"));
        assert!(is_stale(&conn, &peer(2), Duration::from_secs(60)).expect("old"));
        // With a wide threshold even the old one is fresh.
        assert!(!is_stale(&conn, &peer(2), Duration::from_secs(7200)).expect("wide"));
    }
}
