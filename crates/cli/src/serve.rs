//! `rsntr serve`: the serving pipeline behind the iroh transport, driven
//! from a node directory. Exposed as a library function so tests can run
//! two nodes in-process.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::task::JoinHandle;

use resonator_authenticator::Chain;
use resonator_node::{DbHandle, Node, NodeConfig};
use resonator_surfaces::{OutboxConfig, OutboxHandle, OutboxWorker, Presence, PresenceConfig};
use resonator_transport::{IrohConfig, IrohTransport, PeerId, parse_ticket};

use crate::lease::{AppInfo, LeaseInfo, ServeLease};
use crate::store;

/// A running node: transport + pipeline + the serve task, plus the send
/// side (outbox worker) and liveness (presence gossip) surfaces. Holds
/// the directory's serving lease for its lifetime.
pub struct RunningNode {
    transport: Arc<IrohTransport>,
    node: Arc<Node>,
    task: JoinHandle<()>,
    outbox: OutboxHandle,
    presence: Arc<Presence>,
    /// The event-hook dispatcher (`rsntr hook`, crate::hooks).
    hooks: JoinHandle<()>,
    /// The ticket/address freshness task ([`FRESHNESS_CADENCE`]).
    freshness: JoinHandle<()>,
    /// Bound offline (loopback, no relay): readiness never waits for one.
    offline: bool,
    /// The owner channel's control socket (docs/owner-channel.md sec
    /// 3.2); `None` when binding failed (serving continues without it).
    #[cfg(unix)]
    owner_socket: Option<crate::owner_socket::OwnerSocket>,
    /// The owner channel over loopback TCP (crate::owner_tcp), bound on
    /// every platform; `None` when binding failed.
    owner_tcp: Option<crate::owner_tcp::OwnerTcp>,
    /// The serving lease (crate::lease); last field, so it is released
    /// after everything else let go of the directory.
    lease: ServeLease,
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

    /// [`ticket`](Self::ticket), but waits up to `wait` for the ticket to
    /// be dialable first ([`ticket_ready`]): online that means the home
    /// relay is in it (a NAT'd peer is unreachable without one), offline
    /// at least one direct address. Past the deadline the ticket is
    /// returned as is.
    pub async fn ready_ticket(&self, wait: Duration) -> String {
        wait_ready_ticket(&self.transport, self.offline, wait).await
    }

    /// True once the endpoint has a home relay (never when offline).
    pub fn has_relay(&self) -> bool {
        self.transport.has_relay()
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

    /// Who holds the serving lease: this process.
    pub fn lease_info(&self) -> &LeaseInfo {
        self.lease.info()
    }

    /// The loopback owner port, when bound.
    pub fn owner_tcp_port(&self) -> Option<u16> {
        self.owner_tcp.as_ref().map(|t| t.port())
    }

    /// Stops everything and lets go of the directory entirely: control
    /// socket, surfaces, watcher, every entrainment, the endpoint and
    /// the blob store (its file lock), the published serving keys, then
    /// the database connection itself (its WAL locks), then the lease.
    /// After this the process holds no handle on the node directory,
    /// which is what a suspended iOS app must be able to say.
    pub async fn shutdown(self) {
        #[cfg(unix)]
        if let Some(sock) = self.owner_socket {
            sock.shutdown().await;
        }
        if let Some(tcp) = self.owner_tcp {
            tcp.shutdown().await;
        }
        self.outbox.shutdown().await;
        self.presence.shutdown().await;
        self.hooks.abort();
        self.freshness.abort();
        self.node.stop_change_watcher();
        self.node.entrainments().close_all();
        self.task.abort();
        self.transport.shutdown().await;
        // The daemon's published record is only meaningful while it
        // serves; a reader trusting a stale ticket would dial a dead port.
        let _ = self
            .node
            .db()
            .call(|conn| {
                conn.execute(
                    "DELETE FROM _rsntr WHERE key IN \
                     ('serving_addrs', 'serving_ticket', 'external_addrs')",
                    [],
                )
            })
            .await;
        let _ = self.node.db().close().await;
        drop(self.lease);
    }
}

/// How often a serving node re-reads its ticket and external addresses
/// and rewrites `_rsntr.serving_ticket` / `_rsntr.external_addrs` when
/// they changed (a relay learned late, a network change).
pub const FRESHNESS_CADENCE: Duration = Duration::from_secs(60);

/// The `_rsntr` key holding `{"addrs":[...],"relay":bool}` while a node
/// serves: the non-loopback addresses and relay presence a peer would
/// dial, as opposed to `serving_addrs` (loopback, for self-dials).
pub const EXTERNAL_ADDRS_KEY: &str = "external_addrs";

/// Whether a ticket is worth handing out: online it needs the home relay
/// (direct addresses alone strand every NAT'd dialer), offline it needs a
/// direct address (there is nothing else to dial).
pub fn ticket_ready(offline: bool, has_relay: bool, direct_count: usize) -> bool {
    if offline { direct_count > 0 } else { has_relay }
}

/// Polls the live ticket until [`ticket_ready`] or `wait` elapses.
async fn wait_ready_ticket(transport: &IrohTransport, offline: bool, wait: Duration) -> String {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let t = transport.ticket();
        let direct = parse_ticket(&t).map(|(_, a)| a.len()).unwrap_or(0);
        if ticket_ready(offline, transport.has_relay(), direct)
            || tokio::time::Instant::now() >= deadline
        {
            return t;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The `external_addrs` JSON for the current endpoint state.
fn external_addrs_json(transport: &IrohTransport) -> String {
    serde_json::json!({
        "addrs": transport.external_addrs().iter().map(|a| a.to_string()).collect::<Vec<_>>(),
        "relay": transport.has_relay(),
    })
    .to_string()
}

/// Writes the ticket and external addrs when they differ from `last`;
/// returns the values now on record.
async fn publish_freshness(
    transport: &IrohTransport,
    db: &DbHandle,
    last: &mut (String, String),
) -> Result<()> {
    let ticket = transport.ticket();
    let ext = external_addrs_json(transport);
    if (ticket.as_str(), ext.as_str()) == (last.0.as_str(), last.1.as_str()) {
        return Ok(());
    }
    let (t, e) = (ticket.clone(), ext.clone());
    db.call(move |conn| {
        resonator_node::set_rsntr(conn, crate::detach_keys::SERVING_TICKET_KEY, &t)?;
        resonator_node::set_rsntr(conn, EXTERNAL_ADDRS_KEY, &e)
    })
    .await
    .map_err(|e| anyhow::anyhow!("publishing the ticket: {e}"))?
    .map_err(|e| anyhow::anyhow!("publishing the ticket: {e}"))?;
    *last = (ticket, ext);
    Ok(())
}

/// Opens the node directory and starts serving. `offline` binds to
/// localhost with no relays and no address lookup (tests, LAN demos);
/// production uses the n0 defaults.
/// The UDP ports to bind, from the environment.
///
/// `RSNTR_BIND_PORT` pins IPv4 and `RSNTR_BIND_PORT_V6` pins IPv6;
/// unset means an ephemeral port, which is what a node has always had.
/// A node behind a firewall rule needs the port to be known before it
/// starts, and an ephemeral one changes on every restart.
///
/// A value that is not a port is an error rather than a fallback to
/// ephemeral: a typo here would otherwise leave the node running on
/// some other port with the firewall rule pointing at nothing, which is
/// precisely the thing the setting exists to prevent.
pub fn bind_ports_from_env() -> Result<resonator_transport::BindPorts> {
    parse_bind_ports(
        std::env::var("RSNTR_BIND_PORT").ok().as_deref(),
        std::env::var("RSNTR_BIND_PORT_V6").ok().as_deref(),
    )
}

/// The parsing behind [`bind_ports_from_env`], separated so it can be
/// tested without writing to the process environment.
pub fn parse_bind_ports(
    v4: Option<&str>,
    v6: Option<&str>,
) -> Result<resonator_transport::BindPorts> {
    fn port(var: &str, raw: Option<&str>) -> Result<Option<u16>> {
        let Some(raw) = raw else { return Ok(None) };
        if raw.trim().is_empty() {
            return Ok(None);
        }
        let port: u16 = raw
            .trim()
            .parse()
            .with_context(|| format!("{var}={raw:?} is not a port number (1-65535)"))?;
        if port == 0 {
            bail!("{var}=0 would take a random port; unset it instead");
        }
        Ok(Some(port))
    }
    Ok(resonator_transport::BindPorts {
        v4: port("RSNTR_BIND_PORT", v4)?,
        v6: port("RSNTR_BIND_PORT_V6", v6)?,
    })
}

#[cfg(test)]
mod bind_port_tests {
    use super::parse_bind_ports;

    #[test]
    fn unset_or_empty_means_ephemeral() {
        assert_eq!(parse_bind_ports(None, None).unwrap().v4, None);
        assert_eq!(parse_bind_ports(Some(""), Some("  ")).unwrap().v6, None);
    }

    #[test]
    fn a_port_is_taken_per_family() {
        let p = parse_bind_ports(Some("47193"), Some(" 47194 ")).unwrap();
        assert_eq!((p.v4, p.v6), (Some(47193), Some(47194)));
    }

    /// A typo must stop the node rather than quietly fall back to an
    /// ephemeral port: the firewall rule would then name a port nothing
    /// is listening on, which is the failure this setting prevents.
    #[test]
    fn nonsense_and_zero_are_refused_by_name() {
        for bad in ["http", "70000", "-1", "47193x"] {
            let err = parse_bind_ports(Some(bad), None).unwrap_err().to_string();
            assert!(err.contains("RSNTR_BIND_PORT"), "{bad}: {err}");
        }
        let zero = parse_bind_ports(Some("0"), None).unwrap_err().to_string();
        assert!(zero.contains("random port"), "{zero}");
    }
}

pub async fn start_node(dir: &Path, offline: bool) -> Result<RunningNode> {
    start_node_full(dir, offline, PresenceConfig::default(), AppInfo::cli()).await
}

/// [`start_node`] as an embedding app (recorded in the serving lease).
pub async fn start_node_as(dir: &Path, offline: bool, app: AppInfo) -> Result<RunningNode> {
    start_node_full(dir, offline, PresenceConfig::default(), app).await
}

/// [`start_node`] with an explicit presence configuration (`cadence` and
/// `status`; the endpoint fields are governed by `offline` and the node
/// key). Tests use a fast cadence so beacons flow within seconds.
pub async fn start_node_with(
    dir: &Path,
    offline: bool,
    presence_config: PresenceConfig,
) -> Result<RunningNode> {
    start_node_full(dir, offline, presence_config, AppInfo::cli()).await
}

async fn start_node_full(
    dir: &Path,
    offline: bool,
    presence_config: PresenceConfig,
    app: AppInfo,
) -> Result<RunningNode> {
    let dpath = store::db_path(dir);
    if !dpath.exists() {
        bail!(
            "no node database at {}; run `rsntr init {}` first",
            dpath.display(),
            dir.display()
        );
    }
    // The lease first: one process serves a directory. A typed
    // `LeaseError` (downcastable through anyhow) names the holder.
    let lease = ServeLease::acquire(dir, &app).map_err(anyhow::Error::from)?;
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
    // protocol sec 6.2); beyond the gate the hash is the capability. A
    // side connection, never the DbHandle: the gate lives inside the
    // transport, which the serving connection's vtab aux holds, and a
    // DbHandle in it would keep the db thread alive past `close()`.
    let gate_path = dpath.clone();
    let blobs = resonator_transport::BlobsConfig {
        store_dir: dir.join("blobs"),
        gate: Some(Arc::new(move |peer: resonator_transport::PeerId| {
            let path = gate_path.clone();
            Box::pin(async move {
                tokio::task::spawn_blocking(move || {
                    store::open_db_at(&path)
                        .map(|conn| resonator_node::peer_known(&conn, &peer.to_string()))
                        .unwrap_or(false)
                })
                .await
                .unwrap_or(false)
            })
        })),
    };

    let config = IrohConfig {
        hello,
        secret_key: Some(secret),
        offline,
        // Only the serving endpoint pins a port. Client dials keep
        // ephemeral ones: they share this node's key, so a client that
        // pinned the same port would collide with the daemon holding it.
        bind_ports: bind_ports_from_env()?,
        // Empty is n0's relays. A node that should ride its own puts them
        // here; the ticket this node hands out then names them, because
        // `ready_ticket` reads the live endpoint.
        relays: Vec::new(),
        gossip: true,
        blobs: Some(blobs),
        // The one endpoint that publishes this key's address record.
        publish: true,
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
                    let conn = store::open_db_at(&path).ok()?;
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
                let Ok(mut conn) = store::open_db_at(&path) else {
                    return;
                };
                match store::merge_peer_addrs(&mut conn, &peer.to_string(), &addrs) {
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
            let conn = store::open_db_at(&resolver_path).ok()?;
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

    // The reserved `_hooks` table (rsntr hook; crate::hooks), so the
    // observer below has something to watch from the first serve run.
    node.db()
        .call(|conn| conn.execute_batch(crate::hooks::HOOKS_DDL))
        .await
        .map_err(|e| anyhow::anyhow!("creating _hooks: {e}"))?
        .map_err(|e| anyhow::anyhow!("creating _hooks: {e}"))?;

    // Outbox worker: sqlite has one update_hook per connection and the
    // node's vibration hook owns it, so the worker's wake is composed in
    // as the node's table observer instead of a second hook. The hook
    // runner's wake composes into the same single observer slot.
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
    let (hook_tx, hook_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let hooks = crate::hooks::HookRunner::spawn(dir.to_path_buf(), node.db().clone(), hook_rx);
    node.set_table_observer(move |table| {
        if table == "_outbox" {
            waker.wake();
        }
        if matches!(table, "chat_messages" | "_inbox" | "_hooks") {
            let _ = hook_tx.send(table.to_string());
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

    // The blob fetch hook: the remote mod-install path pulls an
    // installer's wasm by hash from the requesting peer. A fresh client
    // endpoint (the proven `rsntr fetch` path) rather than the serving
    // transport: the serving blob provider serves, it does not dial, and
    // the installer must be a known peer for its provider to answer.
    {
        let fetch_dir = dir.to_path_buf();
        node.set_blob_fetcher(Arc::new(
            move |peer: String, hash: String, addrs: Vec<String>| {
                let dir = fetch_dir.clone();
                Box::pin(async move {
                    let parsed: Vec<std::net::SocketAddr> =
                        addrs.iter().filter_map(|a| a.parse().ok()).collect();
                    crate::client::fetch_blob_with_addrs(&dir, &peer, &hash, &parsed, offline)
                        .await
                        .map_err(|e| e.to_string())
                })
            },
        ));
    }

    // The mods reload hook: reinstall the mods host from the current
    // `_modulations` rows and refresh the transport's advertised
    // modulation list, so a hot install/removal takes effect without a
    // restart. A Weak avoids an Arc cycle (the hook lives on the node it
    // reinstalls). Feature-gated with the mods host itself.
    #[cfg(feature = "mods")]
    {
        let weak = std::sync::Arc::downgrade(&node);
        let reload_transport = transport.clone();
        node.set_mod_reloader(std::sync::Arc::new(move || {
            let weak = weak.clone();
            let transport = reload_transport.clone();
            Box::pin(async move {
                let node = weak
                    .upgrade()
                    .ok_or_else(|| "the serving node is gone".to_string())?;
                let refused = resonator_mods::ModsHost::install(&node)
                    .await
                    .map_err(|e| format!("reloading the mods host: {e}"))?;
                let hello = node
                    .db()
                    .call(|conn| store::hello_from_db(conn))
                    .await
                    .map_err(|e| format!("recomputing the hello: {e}"))?;
                transport.set_hello_mods(hello.mods);
                Ok(refused)
            })
        }));
    }

    // The daemon's published ticket and external addresses: written once
    // the ticket is dialable (bounded wait) and re-checked on a cadence,
    // rewritten only on change. Before the control socket binds, so a
    // detach parent that sees the socket answer finds the ticket too.
    let freshness = {
        let transport = transport.clone();
        let db = node.db().clone();
        let mut last = (String::new(), String::new());
        wait_ready_ticket(
            &transport,
            offline,
            Duration::from_secs(if offline { 3 } else { 5 }),
        )
        .await;
        publish_freshness(&transport, &db, &mut last).await?;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(FRESHNESS_CADENCE).await;
                if let Err(e) = publish_freshness(&transport, &db, &mut last).await {
                    tracing::warn!(error = %e, "ticket freshness write failed");
                }
                // WAL hygiene for a database many processes read: a
                // passive checkpoint whenever no reader pins the log.
                let _ = db
                    .call(|conn| conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE)"))
                    .await;
            }
        })
    };

    // The control socket: owner commands dispatched on this process's
    // live connection, so update hooks fire and Sympathetic points
    // vibrate. Serving continues without it if binding fails.
    #[cfg(unix)]
    let owner_socket = if crate::owner_socket::fits(dir) {
        match crate::owner_socket::bind(dir, node.clone(), Some(transport.clone())).await {
            Ok(sock) => Some(sock),
            Err(e) => {
                tracing::warn!(error = %e, "control socket unavailable");
                None
            }
        }
    } else {
        tracing::debug!(
            "node directory path too long for a unix socket; owner channel over loopback TCP only"
        );
        None
    };
    // Loopback TCP with a token file: the owner channel for platforms
    // and paths without a unix socket, bound everywhere so a client
    // never has to guess.
    let owner_tcp = match crate::owner_tcp::bind(
        dir,
        node.clone(),
        &lease.info().app.id,
        Some(transport.clone()),
    )
    .await
    {
        Ok(tcp) => Some(tcp),
        Err(e) => {
            tracing::warn!(error = %e, "loopback owner channel unavailable");
            None
        }
    };

    // The vibration/update hook must exist before start_node returns:
    // run() installs it too, but only after its task gets scheduled — a
    // commit in that window would vibrate nothing and fire no hooks.
    node.install_vibration_hook()
        .await
        .map_err(|e| anyhow::anyhow!("installing the vibration hook: {e}"))?;
    // Writes by other processes (apps, `rsntr --local`) reach this
    // process's vibrations, hooks, feed and outbox through the watcher.
    node.start_change_watcher().await;
    // The serving process is an app of the node too (`rsntr apps list`
    // on a phone shows the desktop daemon).
    {
        let app = resonator_node::AppInfo {
            id: lease.info().app.id.clone(),
            name: lease.info().app.name.clone(),
            version: lease.info().app.version.clone(),
        };
        if let Err(e) = node
            .db()
            .call(move |conn| resonator_node::register_app(conn, &app))
            .await
        {
            tracing::warn!(error = %e, "registering the serving app failed");
        }
    }
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
        hooks,
        freshness,
        offline,
        #[cfg(unix)]
        owner_socket,
        owner_tcp,
        lease,
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

#[cfg(test)]
mod tests {
    use super::ticket_ready;

    #[test]
    fn ticket_ready_matrix() {
        // offline: a direct address is all there is to dial
        assert!(!ticket_ready(true, false, 0));
        assert!(ticket_ready(true, false, 1));
        assert!(ticket_ready(true, true, 1));
        // online: the relay decides; direct addresses alone strand NAT'd dialers
        assert!(!ticket_ready(false, false, 0));
        assert!(!ticket_ready(false, false, 3));
        assert!(ticket_ready(false, true, 0));
        assert!(ticket_ready(false, true, 3));
    }
}
