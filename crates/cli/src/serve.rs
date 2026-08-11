//! `rsntr serve`: the serving pipeline behind the iroh transport, driven
//! from a node directory. Exposed as a library function so tests can run
//! two nodes in-process.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use tokio::task::JoinHandle;

use resonator_authenticator::Chain;
use resonator_node::{DbHandle, Node, NodeConfig};
use resonator_surfaces::{OutboxConfig, OutboxHandle, OutboxWorker, Presence, PresenceConfig};
use resonator_transport::{IrohConfig, IrohTransport, PeerId, parse_ticket};

use crate::store;

/// A running node: transport + pipeline + the serve task, plus the send
/// side (outbox worker) and liveness (presence gossip) surfaces.
pub struct RunningNode {
    transport: Arc<IrohTransport>,
    node: Arc<Node>,
    task: JoinHandle<()>,
    outbox: OutboxHandle,
    presence: Arc<Presence>,
    /// The owner channel's control socket (docs/owner-channel.md sec
    /// 3.2); `None` when binding failed (serving continues without it).
    #[cfg(unix)]
    owner_socket: Option<crate::owner_socket::OwnerSocket>,
}

impl RunningNode {
    /// This node's endpoint id.
    pub fn peer_id(&self) -> PeerId {
        self.transport.peer_id()
    }

    /// The socket addresses the endpoint is bound to.
    pub fn direct_addrs(&self) -> Vec<SocketAddr> {
        self.transport.direct_addrs()
    }

    /// The live dialing ticket of the serving endpoint itself. Unlike a
    /// `rsntr ticket` mint, this names the endpoint actually accepting
    /// connections (same key, same port), so it pastes straight into a
    /// remote `rsntr peer add`.
    pub fn ticket(&self) -> String {
        self.transport.ticket()
    }

    /// [`ticket`](Self::ticket), but waits up to `wait` for the endpoint
    /// to learn at least one direct address first, so the printed ticket
    /// is dialable immediately.
    pub async fn ready_ticket(&self, wait: Duration) -> String {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let t = self.ticket();
            if matches!(parse_ticket(&t), Ok((_, addrs)) if !addrs.is_empty()) {
                return t;
            }
            if tokio::time::Instant::now() >= deadline {
                return t;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// The pipeline (the tests' window into the serving database).
    pub fn node(&self) -> &Arc<Node> {
        &self.node
    }

    /// The transport (for `add_peer_addrs` in offline setups).
    pub fn transport(&self) -> &Arc<IrohTransport> {
        &self.transport
    }

    /// The presence surface (test observability, beacon toggling).
    pub fn presence(&self) -> &Arc<Presence> {
        &self.presence
    }

    /// Nudges the outbox worker to scan `_outbox` now (writes made on
    /// other connections, e.g. the CLI process, are otherwise picked up on
    /// the poll cadence).
    pub fn wake_outbox(&self) {
        self.outbox.wake();
    }

    /// The control socket path, when one is bound.
    #[cfg(unix)]
    pub fn owner_socket_path(&self) -> Option<&std::path::Path> {
        self.owner_socket.as_ref().map(|s| s.path())
    }

    /// Stops the surfaces and the serve loop, closes the endpoint,
    /// removes the control socket.
    pub async fn shutdown(self) {
        #[cfg(unix)]
        if let Some(sock) = self.owner_socket {
            sock.shutdown().await;
        }
        self.outbox.shutdown().await;
        self.presence.shutdown().await;
        self.task.abort();
        self.transport.shutdown().await;
    }
}

/// Opens the node directory and starts serving. `offline` binds to
/// localhost with no relays and no address lookup (tests, LAN demos);
/// production uses the n0 defaults.
pub async fn start_node(dir: &Path, offline: bool) -> Result<RunningNode> {
    start_node_with(dir, offline, PresenceConfig::default()).await
}

/// [`start_node`] with an explicit presence configuration (`cadence` and
/// `status`; the endpoint fields are governed by `offline` and the node
/// key). Tests use a fast cadence so beacons flow within seconds.
pub async fn start_node_with(
    dir: &Path,
    offline: bool,
    presence_config: PresenceConfig,
) -> Result<RunningNode> {
    let dpath = store::db_path(dir);
    if !dpath.exists() {
        bail!(
            "no node database at {}; run `rsntr init {}` first",
            dpath.display(),
            dir.display()
        );
    }
    // The serving connection needs the rdf_* SQL surface for the sparql
    // modulation; open through the node crate, then apply the CLI's
    // cross-process settings.
    let conn = resonator_node::open_node_db(&dpath)
        .map_err(|e| anyhow::anyhow!("opening {}: {e}", dpath.display()))?;
    store::configure(&conn)?;
    let hello = store::hello_from_db(&conn);
    let secret = store::load_secret(dir)?;

    let db = DbHandle::spawn(conn);

    // Blob provider on the shared endpoint, accept-gated by _peers (chat
    // protocol sec 6.2); beyond the gate the hash is the capability.
    let gate_db = db.clone();
    let blobs = resonator_transport::BlobsConfig {
        store_dir: dir.join("blobs"),
        gate: Some(Arc::new(move |peer: resonator_transport::PeerId| {
            let db = gate_db.clone();
            Box::pin(async move {
                db.call(move |conn| resonator_node::peer_known(conn, &peer.to_string()))
                    .await
                    .unwrap_or(false)
            })
        })),
    };

    let config = IrohConfig {
        hello,
        secret_key: Some(secret),
        offline,
        gossip: true,
        blobs: Some(blobs),
    };
    let (transport, incoming) = IrohTransport::bind(config)
        .await
        .map_err(|e| anyhow::anyhow!("binding the endpoint: {e}"))?;

    // Publish the live listen addresses so local surfaces (rsntr chat
    // watch) can self-dial the serving endpoint. Unspecified addresses
    // are rewritten to loopback so they are dialable as stored.
    {
        let addrs: Vec<String> = transport
            .direct_addrs()
            .iter()
            .map(|a| {
                let mut a = *a;
                if a.ip().is_unspecified() {
                    a.set_ip(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
                }
                a.to_string()
            })
            .collect();
        let json = serde_json::to_string(&addrs)?;
        db.call(move |conn| resonator_node::set_rsntr(conn, "serving_addrs", &json))
            .await
            .map_err(|e| anyhow::anyhow!("writing serving_addrs: {e}"))?
            .map_err(|e| anyhow::anyhow!("writing serving_addrs: {e}"))?;
    }

    let node = Arc::new(Node::new(
        db,
        Chain::with_builtin_tiers(),
        NodeConfig::default(),
    ));

    // Dial addresses are read from `_peers` at dial time, not snapshotted
    // at start: a `rsntr peer add` from another process while this node
    // runs must be dialable by the outbox worker and web relaying
    // without a restart. A dedicated read connection, never the DbHandle:
    // the db thread may itself be blocked inside a remote_query/
    // iroh_remote vtab call whose dial consults this source, and a
    // db.call here would deadlock until the vtab deadline.
    {
        let source_path = dpath.clone();
        transport.set_addr_source(Arc::new(move |peer: PeerId| {
            let path = source_path.clone();
            Box::pin(async move {
                tokio::task::spawn_blocking(move || {
                    let conn = rusqlite::Connection::open(&path).ok()?;
                    store::resolve_peer(&conn, &peer.to_string())
                        .ok()
                        .map(|(_, addrs)| addrs)
                })
                .await
                .ok()
                .flatten()
                .unwrap_or_default()
            })
        }));
    }

    // The mirror direction: live connections report the peer's observed
    // direct addresses, which merge into `_peers.addrs` for KNOWN peers
    // (fresh first, capped). Ports change every serve run, so this is
    // what keeps dials fast across peer restarts without re-running
    // `peer add`. Same dedicated-connection rule as the source above.
    {
        let sink_path = dpath.clone();
        transport.set_addr_sink(Arc::new(move |peer: PeerId, addrs: Vec<SocketAddr>| {
            let path = sink_path.clone();
            tokio::task::spawn_blocking(move || {
                let Ok(conn) = rusqlite::Connection::open(&path) else {
                    return;
                };
                match store::merge_peer_addrs(&conn, &peer.to_string(), &addrs) {
                    Ok(true) => {
                        tracing::debug!(peer = %peer, ?addrs, "refreshed dial addresses from live connection");
                    }
                    Ok(false) => {}
                    Err(e) => tracing::debug!(peer = %peer, error = %e, "address refresh failed"),
                }
            });
        }));
    }

    // The SQL vtab surfaces on the serving connection: owner-lane
    // statements (control socket, in-process channel while serving) can
    // read and write admitted peers as plain SQL through remote_query()
    // and iroh_remote tables. Peer names resolve on a dedicated read
    // connection for the same no-deadlock reason as the addr source.
    {
        let resolver_path = dpath.clone();
        let resolver: resonator_surfaces::PeerResolver = Arc::new(move |peer: &str| {
            let conn = rusqlite::Connection::open(&resolver_path).ok()?;
            store::resolve_peer(&conn, peer).ok().map(|(id, _addrs)| id)
        });
        let ctx = Arc::new(
            resonator_surfaces::RemoteContext::new(
                transport.clone(),
                tokio::runtime::Handle::current(),
            )
            .with_resolver(resolver),
        );
        node.db()
            .call(move |conn| resonator_surfaces::register_remote_vtabs(conn, ctx))
            .await
            .map_err(|e| anyhow::anyhow!("registering the remote vtabs: {e}"))?
            .map_err(|e| anyhow::anyhow!("registering the remote vtabs: {e}"))?;
    }

    // The extism mods host: load the enabled `_modulations` rows and
    // register the handler (a refused row is logged, never fatal; its
    // requests answer mod-unsupported).
    #[cfg(feature = "mods")]
    match resonator_mods::ModsHost::install(&node).await {
        Ok(refused) => {
            for (name, reason) in refused {
                tracing::warn!(mod_name = %name, %reason, "mod refused at load");
            }
        }
        Err(e) => tracing::warn!(error = %e, "mods host failed to load"),
    }

    // Outbox worker: sqlite has one update_hook per connection and the
    // node's vibration hook owns it, so the worker's wake is composed in
    // as the node's table observer instead of a second hook.
    let outbox = OutboxWorker::spawn(
        node.db().clone(),
        transport.clone(),
        OutboxConfig {
            install_update_hook: false,
            ..OutboxConfig::default()
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("starting the outbox worker: {e}"))?;
    let waker = outbox.waker();
    node.set_table_observer(move |table| {
        if table == "_outbox" {
            waker.wake();
        }
    });

    // The owner channel's chat attachment import runs on the serving
    // process's live blob store (docs/owner-channel.md sec 5.3).
    {
        let importer_transport = transport.clone();
        node.set_blob_importer(Arc::new(move |path: std::path::PathBuf| {
            let transport = importer_transport.clone();
            Box::pin(async move {
                transport
                    .blob_add_path(&path)
                    .await
                    .map_err(|e| e.to_string())
            })
        }));
    }

    // The control socket: owner commands dispatched on this process's
    // live connection, so update hooks fire and Sympathetic points
    // vibrate. Serving continues without it if binding fails.
    #[cfg(unix)]
    let owner_socket = match crate::owner_socket::bind(dir, node.clone()).await {
        Ok(sock) => Some(sock),
        Err(e) => {
            tracing::warn!(error = %e, "control socket unavailable");
            None
        }
    };

    let task = tokio::spawn(node.clone().run(incoming));

    // Presence gossip on the same endpoint and identity (the transport's
    // router serves the gossip ALPN alongside resonator/rdf/0).
    let gossip = transport
        .gossip()
        .expect("bind was configured with gossip")
        .clone();
    let presence = Arc::new(
        Presence::attach(
            transport.endpoint().clone(),
            gossip,
            presence_config,
            node.db().clone(),
        )
        .map_err(|e| anyhow::anyhow!("attaching presence: {e}"))?,
    );
    // Offline / LAN: seed the shared endpoint's lookup with the stored
    // dial hints so gossip can bootstrap without discovery.
    for (peer, addrs) in store::peer_dial_hints(node.db()).await? {
        if let Ok(id) = iroh::EndpointId::from_bytes(peer.as_bytes()) {
            presence.register_peer(iroh::EndpointAddr::from_parts(
                id,
                addrs.into_iter().map(iroh::TransportAddr::Ip),
            ));
        }
    }
    let admitted = presence
        .admitted_peers()
        .await
        .map_err(|e| anyhow::anyhow!("reading the admitted peer set: {e}"))?;
    presence
        .join(admitted)
        .await
        .map_err(|e| anyhow::anyhow!("joining the presence topic: {e}"))?;

    Ok(RunningNode {
        transport,
        node,
        task,
        outbox,
        presence,
        #[cfg(unix)]
        owner_socket,
    })
}

/// Default bind address of `rsntr serve --web`.
#[cfg(feature = "web")]
pub const DEFAULT_WEB_ADDR: &str = resonator_web::DEFAULT_ADDR;

/// Starts the web interface (`rsntr serve --web`, docs/web-api.md) on a
/// running node: same pipeline, the node's own EndpointId as the owner
/// peer, `?peer=` relaying over the node's transport. The capability
/// token is persisted in the node directory (`rsntr.web-token`) so it is
/// stable across serve runs; `rotate_token` mints a replacement. The
/// returned server carries the fragment URL to print.
#[cfg(feature = "web")]
pub async fn start_web(
    running: &RunningNode,
    dir: &Path,
    addr: SocketAddr,
    rotate_token: bool,
) -> Result<resonator_web::WebServer> {
    let token = crate::store::load_or_mint_web_token(dir, rotate_token)?;
    resonator_web::serve_web(
        running.node().clone(),
        &running.peer_id().to_string(),
        Some(running.transport().clone()),
        resonator_web::WebConfig {
            addr,
            token: Some(token),
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("starting the web interface: {e}"))
}
