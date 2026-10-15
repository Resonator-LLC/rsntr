//! Client-side round trips: query, help, projection, entrain, media.
//!
//! Each function binds a short-lived client endpoint with the node's own
//! key (so the serving side sees the admitted identity), dials the peer
//! with any `_peers.addrs` hints, sends one request on a fresh bi stream,
//! and drives the protocol crate's readers over the answer.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};

use resonator_protocol::{
    Decision, Denied, Done, EnvelopeObject, ErrorEnvelope, Knock, Request, RequestKind,
    ResponseEvent, ResponseReader, Row, Value, mod_matches,
};
use resonator_transport::{
    IrohConfig, IrohTransport, PathReport, PeerId, RequestStream, Transport, TransportError,
};

use crate::store;

/// The terminal shape of one request, decoded.
#[derive(Debug)]
pub enum QueryOutcome {
    /// Header + rows + Done (sql-sqlite, sparql SELECT/ASK, updates).
    Rows {
        columns: Vec<String>,
        rows: Vec<Row>,
        done: Done,
    },
    /// `rsntr:Graph` frames + Done (sparql CONSTRUCT/DESCRIBE).
    Graph {
        triples: Vec<oxrdf::Triple>,
        done: Done,
    },
    /// The serving side's authenticator said no.
    Denied(Denied),
    /// A protocol- or engine-level error frame.
    Failed(ErrorEnvelope),
    /// A `rsntr:Help` response: plain-text usage guidance, one frame.
    Help { text: String, topics: Vec<String> },
}

/// One executed request: the ULID request id plus its outcome.
#[derive(Debug)]
pub struct QueryReport {
    pub id: String,
    pub outcome: QueryOutcome,
}

/// Classifies a SQL statement for the envelope class: read shapes go out
/// as `rsntr:Query`, everything else as `rsntr:Execute`. A labeling
/// heuristic only; the serving side derives the real kind from its own
/// prepare-time footprint.
pub fn classify_sql(sql: &str) -> RequestKind {
    let first = sql
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match first.as_str() {
        "select" | "with" | "values" | "explain" => RequestKind::Query,
        _ => RequestKind::Execute,
    }
}

/// Classifies a SPARQL text: SELECT/ASK/CONSTRUCT/DESCRIBE are queries,
/// SPARQL Update forms are executes. Skips leading PREFIX/BASE
/// declarations to find the query form.
pub fn classify_sparql(text: &str) -> RequestKind {
    for word in text.split_whitespace() {
        match word.to_ascii_lowercase().as_str() {
            "select" | "ask" | "construct" | "describe" => return RequestKind::Query,
            "insert" | "delete" | "load" | "clear" | "create" | "drop" | "with" | "move"
            | "copy" | "add" => return RequestKind::Execute,
            _ => {}
        }
    }
    RequestKind::Query
}

/// What to do when a peer cannot be dialed: the phrasing every dial
/// failure ends with, so an agent never has to guess.
pub fn dial_hint() -> &'static str {
    "the peer may be down or its stored addresses stale: try `rsntr peer refresh <name>`, \
     re-add its current ticket (`rsntr peer add <name> <ticket>`), or have it knock you"
}

/// What `rsntr peer refresh` found.
#[derive(Debug, Clone)]
pub struct RefreshReport {
    pub endpoint_id: String,
    /// `_peers.addrs` before the dial.
    pub before: Vec<std::net::SocketAddr>,
    /// `_peers.addrs` after the observed addresses merged in.
    pub after: Vec<std::net::SocketAddr>,
    /// The direct addresses the live connection reported.
    pub observed: Vec<std::net::SocketAddr>,
}

/// Bounded dial timeout of [`refresh_peer`].
pub const REFRESH_DIAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// `rsntr peer refresh <peer>`: dials the peer once (a help round trip,
/// bounded) with an address sink installed, so the addresses the live
/// connection observes merge into `_peers.addrs` (fresh first). Fails
/// with the [`dial_hint`] phrasing when the peer is unreachable at every
/// known address.
pub async fn refresh_peer(dir: &Path, peer: &str, offline: bool) -> Result<RefreshReport> {
    // Not proxied, deliberately: this command exists to learn a peer's
    // addresses from a dial this process makes, and the serving process's
    // view is what `peer refresh` is being run to check against. It is a
    // short dial, so the displacement window is a second
    // (docs/relay-identity-sharing.md).
    let (transport, peer_id) = bind_client(dir, peer, offline).await?;
    let before = {
        let conn = store::open_db(dir)?;
        store::resolve_peer(&conn, peer)?.1
    };
    let observed: Arc<std::sync::Mutex<Vec<std::net::SocketAddr>>> = Arc::default();
    {
        let observed = observed.clone();
        let sink_path = store::db_path(dir);
        transport.set_addr_sink(Arc::new(
            move |p: PeerId, addrs: Vec<std::net::SocketAddr>| {
                if p != peer_id {
                    return;
                }
                observed
                    .lock()
                    .expect("observed lock poisoned")
                    .extend(addrs.iter().copied());
                let path = sink_path.clone();
                tokio::task::spawn_blocking(move || {
                    if let Ok(mut conn) = store::open_db_at(&path) {
                        let _ = store::merge_peer_addrs(&mut conn, &p.to_string(), &addrs);
                    }
                });
            },
        ));
    }
    let request = Request::new(RequestKind::Query, "help", "");
    let envelope = request.to_envelope();
    let dialed = tokio::time::timeout(REFRESH_DIAL_TIMEOUT, async {
        let (mut stream, _hello) = transport.open(peer_id).await?;
        stream.send(&envelope).await?;
        stream.finish().await?;
        while stream.recv().await?.is_some() {}
        Ok::<(), resonator_transport::TransportError>(())
    })
    .await;
    let result = match dialed {
        Ok(Ok(())) => {
            // The path watcher reports asynchronously; give it a beat.
            let until = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
            while observed.lock().expect("observed lock poisoned").is_empty()
                && tokio::time::Instant::now() < until
            {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Ok(())
        }
        Ok(Err(e)) => Err(anyhow::anyhow!(
            "{peer} is unreachable at all {} known address(es): {e}; {}",
            before.len(),
            dial_hint()
        )),
        Err(_elapsed) => Err(anyhow::anyhow!(
            "{peer} is unreachable at all {} known address(es) (no answer within {}s); {}",
            before.len(),
            REFRESH_DIAL_TIMEOUT.as_secs(),
            dial_hint()
        )),
    };
    // A dial still in flight can make the close linger; bound it.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), transport.shutdown()).await;
    result?;
    // Let the sink's blocking merge land before reading back.
    tokio::task::yield_now().await;
    let after = {
        let conn = store::open_db(dir)?;
        store::resolve_peer(&conn, peer)?.1
    };
    let mut observed = observed.lock().expect("observed lock poisoned").clone();
    observed.sort();
    observed.dedup();
    Ok(RefreshReport {
        endpoint_id: peer_id.to_string(),
        before,
        after,
        observed,
    })
}

/// Binds a client endpoint from the node directory's key and hello, and
/// registers the target's stored dial hints. The caller opens streams and
/// must `shutdown` the returned transport.
async fn bind_client(
    dir: &Path,
    peer: &str,
    offline: bool,
) -> Result<(Arc<IrohTransport>, PeerId)> {
    let conn = store::open_db(dir)?;
    let (peer_id, addrs) = store::resolve_peer(&conn, peer)?;
    let hello = store::hello_from_db(&conn);
    let secret = store::load_secret(dir)?;
    drop(conn);

    let (transport, _incoming) = IrohTransport::bind(client_config(hello, secret, offline))
        .await
        .map_err(|e| anyhow::anyhow!("binding the client endpoint: {e}"))?;
    if !addrs.is_empty() {
        transport.add_peer_addrs(peer_id, addrs);
    }
    Ok((transport, peer_id))
}

/// A stream to a peer, however it got there.
///
/// Direct means this process dialled the peer itself, with an endpoint
/// carrying the node's own key. Proxied means the process already
/// serving this directory did the dialling and is copying bytes for us,
/// so the identity has exactly one endpoint even while this command
/// runs (docs/relay-identity-sharing.md).
///
/// The two are interchangeable to everything above: raw bytes are
/// positional on both, so the peer protocol reads the same either way.
pub enum PeerLink {
    Direct(resonator_transport::IrohRequestStream),
    Proxied(crate::owner::OwnerStream),
}

impl RequestStream for PeerLink {
    async fn send(&mut self, obj: &EnvelopeObject) -> Result<(), TransportError> {
        match self {
            Self::Direct(s) => s.send(obj).await,
            Self::Proxied(s) => s.send(obj).await,
        }
    }

    async fn recv(&mut self) -> Result<Option<EnvelopeObject>, TransportError> {
        match self {
            Self::Direct(s) => s.recv().await,
            Self::Proxied(s) => s.recv().await,
        }
    }

    async fn send_raw(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        match self {
            Self::Direct(s) => s.send_raw(bytes).await,
            Self::Proxied(s) => s.send_raw(bytes).await,
        }
    }

    async fn recv_raw(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        match self {
            Self::Direct(s) => s.recv_raw().await,
            Self::Proxied(s) => s.recv_raw().await,
        }
    }

    async fn closed(&mut self) {
        match self {
            Self::Direct(s) => s.closed().await,
            // The tunnel gives no separate closure signal; a write error
            // is the only notice, which is the trait's own default.
            Self::Proxied(_) => std::future::pending().await,
        }
    }

    async fn finish(&mut self) -> Result<(), TransportError> {
        match self {
            Self::Direct(s) => s.finish().await,
            Self::Proxied(s) => s.finish().await,
        }
    }
}

/// Asks how a conversation is travelling, without holding the
/// conversation. Separable from the stream on purpose: a running feed
/// is sampled while it runs, by another task.
#[derive(Clone)]
pub struct PathProbe {
    transport: Option<Arc<IrohTransport>>,
    peer_id: PeerId,
    dir: PathBuf,
    target: String,
}

impl PathProbe {
    /// True when the serving process owns the connection and this
    /// process bound no endpoint of its own.
    pub fn is_proxied(&self) -> bool {
        self.transport.is_none()
    }

    /// The current path, read from whichever process owns the
    /// connection. For a proxied session that is the serving process,
    /// asked over its own short connection so the running session is
    /// not disturbed.
    pub async fn sample(&self) -> Option<PathReport> {
        match &self.transport {
            Some(t) => t.peer_path(self.peer_id).await,
            None => ask_daemon_for_path(&self.dir, &self.target).await,
        }
    }
}

/// One client-side conversation with a peer: the stream, the peer's
/// hello, and a probe for how it is travelling.
pub struct PeerSession {
    pub stream: PeerLink,
    pub hello: resonator_protocol::Hello,
    pub probe: PathProbe,
}

impl PeerSession {
    /// The stream and the probe, so a long session can be read and
    /// sampled at the same time.
    pub fn split(self) -> (PeerLink, PathProbe) {
        (self.stream, self.probe)
    }

    /// Releases whatever this session holds. A proxied session owns no
    /// endpoint, so there is nothing to release but the tunnel.
    pub async fn shutdown(self) {
        self.probe.shutdown().await;
    }
}

impl PathProbe {
    /// Drops the endpoint behind a direct session. A no-op when the
    /// serving process owns the connection.
    pub async fn shutdown(self) {
        if let Some(t) = self.transport {
            t.shutdown().await;
        }
    }
}

/// True when this process should reach peers through the process
/// serving `dir` rather than by binding its own endpoint.
///
/// The rule is simply "somebody is serving this directory": that is
/// exactly the case where a second endpoint for this identity would
/// displace the first on its relay. `RSNTR_NO_PEER_PROXY=1` forces the
/// direct path for debugging.
fn proxy_wanted(dir: &Path) -> bool {
    if std::env::var_os("RSNTR_NO_PEER_PROXY").is_some() {
        return false;
    }
    crate::lease::is_held(dir)
}

/// Asks the serving process to open a stream to `target`, returning the
/// tunnel and the peer's hello. `Ok(None)` means "no daemon took this",
/// which is a reason to dial directly; `Err` means the peer itself could
/// not be reached, which is not.
async fn proxied_stream(
    dir: &Path,
    target: &str,
) -> Result<Option<(crate::owner::OwnerStream, resonator_protocol::Hello)>> {
    let Ok(mut stream) = crate::owner::connect(dir).await else {
        return Ok(None);
    };
    let request = Request::new(RequestKind::Query, crate::owner::PROXY_MOD, target);
    if stream.send(&request.to_envelope()).await.is_err() {
        return Ok(None);
    }
    match stream.recv().await {
        Ok(Some(EnvelopeObject::Hello(h))) => Ok(Some((stream, h))),
        // An older daemon does not know the modulation; dial directly.
        Ok(Some(EnvelopeObject::Error(e))) if e.code == "mod-unsupported" => Ok(None),
        // A dial the daemon made on our behalf is still a dial, and the
        // manual promises every dial error ends with the same remedy.
        // The direct branch below appends it; this one did not, so once
        // the proxy became the normal path, the usual failure was the
        // one that stopped saying what to do about it.
        Ok(Some(EnvelopeObject::Error(e))) if e.code == "connection-error" => bail!(
            "{}; {}",
            e.reason
                .as_deref()
                .unwrap_or("the serving process could not reach the peer"),
            dial_hint()
        ),
        Ok(Some(EnvelopeObject::Error(e))) => bail!(
            "{}: {}",
            e.code,
            e.reason.as_deref().unwrap_or("(no reason given)")
        ),
        Ok(Some(other)) => bail!("unexpected proxy response frame: {other:?}"),
        Ok(None) | Err(_) => Ok(None),
    }
}

/// Reads the serving process's view of the path to `target`.
async fn ask_daemon_for_path(dir: &Path, target: &str) -> Option<PathReport> {
    let mut stream = crate::owner::connect(dir).await.ok()?;
    let request = Request::new(RequestKind::Query, crate::owner::PROXY_PATH_MOD, target);
    stream.send(&request.to_envelope()).await.ok()?;
    let EnvelopeObject::Generic(g) = stream.recv().await.ok()?? else {
        return None;
    };
    let payload = g.props.iter().find_map(|(p, o)| {
        (p.as_str() == resonator_protocol::vocab::prop::SIGNAL.as_str())
            .then(|| match o {
                oxrdf::Term::Literal(l) => Some(l.value().to_string()),
                _ => None,
            })
            .flatten()
    })?;
    let v: serde_json::Value = serde_json::from_str(&payload).ok()?;
    Some(PathReport {
        relay: v["relay"].as_bool()?,
        remote: v["remote"].as_str().map(str::to_string),
        rtt_ms: v["rtt_ms"].as_u64(),
        paths: v["paths"].as_u64().unwrap_or(0) as usize,
        direct: v["direct"].as_u64().unwrap_or(0) as usize,
    })
}

/// Opens a conversation with `peer` from the node directory `dir`,
/// through the serving process when there is one and directly when
/// there is not.
/// What `--offline` cannot promise once the dial is proxied.
///
/// The flag asks THIS process to bind an endpoint with no relay and no
/// address lookup. A proxied dial is made by the serving process on the
/// endpoint it already has, so there is nothing left for the flag to
/// govern - and it is silently dropped, which leaves a caller who
/// passed it believing the request stayed off every relay. Since a node
/// is normally served, that is the usual case rather than the odd one.
///
/// A `warn!` and not a print: the default filter shows it (the CLI
/// builds `EnvFilter::new("warn,...")`), `RUST_LOG` silences it, and
/// the embedding hosts that share this code get an event rather than
/// bytes on their stderr.
pub const OFFLINE_PROXIED_NOTICE: &str = "--offline is not applied to this dial: the node directory is served, so the \
     serving process makes the dial on the endpoint it already has and that \
     endpoint's own binding decides relays and lookup (RSNTR_NO_PEER_PROXY=1 dials \
     from this process instead; docs/relay-identity-sharing.md)";

pub async fn open_peer(dir: &Path, peer: &str, offline: bool) -> Result<PeerSession> {
    if proxy_wanted(dir)
        && let Some((stream, hello)) = proxied_stream(dir, peer).await?
    {
        if offline {
            tracing::warn!(peer, dir = %dir.display(), "{OFFLINE_PROXIED_NOTICE}");
        }
        let conn = store::open_db(dir)?;
        let (peer_id, _) = store::resolve_peer(&conn, peer)?;
        drop(conn);
        return Ok(PeerSession {
            stream: PeerLink::Proxied(stream),
            hello,
            probe: PathProbe {
                transport: None,
                peer_id,
                dir: dir.to_path_buf(),
                target: peer.to_string(),
            },
        });
    }
    let (transport, peer_id) = bind_client(dir, peer, offline).await?;
    let (stream, hello) = transport
        .open(peer_id)
        .await
        .map_err(|e| anyhow::anyhow!("connecting to {peer}: {e}; {}", dial_hint()))?;
    Ok(PeerSession {
        stream: PeerLink::Direct(stream),
        hello,
        probe: PathProbe {
            transport: Some(transport),
            peer_id,
            dir: dir.to_path_buf(),
            target: peer.to_string(),
        },
    })
}

/// The configuration of every short-lived client endpoint: the node's
/// own key (so the serving side sees the admitted identity), no gossip,
/// no blob store, and no address publishing (the serving endpoint owns
/// this key's record; a client that published would displace it).
pub(crate) fn client_config(
    hello: resonator_protocol::Hello,
    secret: [u8; 32],
    offline: bool,
) -> IrohConfig {
    IrohConfig {
        hello,
        secret_key: Some(secret),
        offline,
        relays: Vec::new(),
        gossip: false,
        blobs: None,
        publish: false,
        // Never pinned: a client shares the node's key, and the serving
        // process already holds any pinned port.
        bind_ports: resonator_transport::BindPorts::default(),
    }
}

/// Sends `signal` in `modulation` (with positional text `params`) to
/// `peer` (petname or hex endpoint id) from the node directory `dir`.
/// The request kind is classified from the text per the modulation.
pub async fn run_query(
    dir: &Path,
    peer: &str,
    modulation: &str,
    signal: &str,
    params: &[String],
    offline: bool,
    timeout_ms: Option<i64>,
) -> Result<QueryReport> {
    let kind = if mod_matches("sparql", modulation) {
        classify_sparql(signal)
    } else {
        classify_sql(signal)
    };
    run_statement(
        dir,
        peer,
        kind,
        modulation,
        signal,
        params.iter().map(|p| Value::Text(p.clone())).collect(),
        offline,
        timeout_ms,
    )
    .await
}

/// Sends one statement in an explicit modulation with typed positional
/// params. The general form behind [`run_query`] and the projection
/// invocation path.
#[allow(clippy::too_many_arguments)]
pub async fn run_statement(
    dir: &Path,
    peer: &str,
    kind: RequestKind,
    modulation: &str,
    text: &str,
    params: Vec<Value>,
    offline: bool,
    timeout_ms: Option<i64>,
) -> Result<QueryReport> {
    let mut request = Request::new(kind, modulation, text);
    request.params = params;
    request.options.timeout_ms = timeout_ms;
    let id = request.id_string();
    let envelope = request.to_envelope();

    let (mut stream, probe) = open_peer(dir, peer, offline).await?.split();
    let result = async {
        stream
            .send(&envelope)
            .await
            .map_err(|e| anyhow::anyhow!("sending the request: {e}"))?;
        stream
            .finish()
            .await
            .map_err(|e| anyhow::anyhow!("half-closing the request stream: {e}"))?;

        let mut reader = ResponseReader::new(&id);
        let mut columns: Vec<String> = Vec::new();
        let mut rows: Vec<Row> = Vec::new();
        let mut triples: Vec<oxrdf::Triple> = Vec::new();
        let mut graphs_seen = false;
        let mut terminal: Option<QueryOutcome> = None;
        while let Some(frame) = stream
            .recv()
            .await
            .map_err(|e| anyhow::anyhow!("receiving the response: {e}"))?
        {
            match reader.accept(frame).context("response choreography")? {
                ResponseEvent::Header(h) => columns = h.columns,
                ResponseEvent::Rows(batch) => rows.extend(batch),
                ResponseEvent::Graph(g) => {
                    graphs_seen = true;
                    triples.extend(g.payload);
                }
                // Passthrough frames from mods this client has no view
                // of; ignored, the trailer still closes the stream.
                ResponseEvent::Generic(_) => {}
                ResponseEvent::Done(done) => {
                    terminal = Some(if graphs_seen {
                        QueryOutcome::Graph {
                            triples: std::mem::take(&mut triples),
                            done,
                        }
                    } else {
                        QueryOutcome::Rows {
                            columns: std::mem::take(&mut columns),
                            rows: std::mem::take(&mut rows),
                            done,
                        }
                    });
                }
                ResponseEvent::Denied(d) => terminal = Some(QueryOutcome::Denied(d)),
                ResponseEvent::Error(e) => terminal = Some(QueryOutcome::Failed(e)),
                ResponseEvent::Help { signal, topics } => {
                    terminal = Some(QueryOutcome::Help {
                        text: signal,
                        topics,
                    })
                }
                ResponseEvent::Media(_) => {
                    bail!("statement response carried a media header; use `rsntr watch`")
                }
            }
        }
        reader.finish().context("response stream truncated")?;
        terminal.context("response stream ended without a terminal frame")
    }
    .await;
    probe.shutdown().await;

    Ok(QueryReport {
        id,
        outcome: result?,
    })
}

/// Sends a `help` query to `peer` and collects the usage guidance; the
/// serving side answers with a single `rsntr:Help` frame.
/// One sent knock: the request ULID plus the answering Decision frame.
/// `decision` is None when the peer answered nothing at all (the rate
/// limiter drops knocks silently).
#[derive(Debug)]
pub struct KnockReport {
    pub id: String,
    pub decision: Option<Decision>,
}

/// Knocks on `peer` (connection-protocol.md section 4): the one frame an
/// unadmitted key may send. The answer is a Decision: "allow" (admitted,
/// talk normally now), "pending" (parked for the owner), or "deny".
pub async fn knock(dir: &Path, peer: &str, message: &str, offline: bool) -> Result<KnockReport> {
    let id = ulid::Ulid::new().to_string();
    let envelope = EnvelopeObject::Knock(Knock {
        id: Some(id.clone()),
        message: message.to_string(),
    });
    let (mut stream, probe) = open_peer(dir, peer, offline).await?.split();
    let result = async {
        stream
            .send(&envelope)
            .await
            .map_err(|e| anyhow::anyhow!("sending the knock: {e}"))?;
        stream
            .finish()
            .await
            .map_err(|e| anyhow::anyhow!("half-closing the knock stream: {e}"))?;
        stream
            .recv()
            .await
            .map_err(|e| anyhow::anyhow!("reading the knock answer: {e}"))
    }
    .await;
    probe.shutdown().await;
    match result? {
        Some(EnvelopeObject::Decision(d)) => Ok(KnockReport {
            id,
            decision: Some(d),
        }),
        Some(EnvelopeObject::Denied(d)) => {
            bail!(
                "denied: {}",
                d.reason.unwrap_or_else(|| "(no reason)".into())
            )
        }
        Some(other) => bail!("unexpected answer to a knock: {other:?}"),
        None => Ok(KnockReport { id, decision: None }),
    }
}

pub async fn run_help(
    dir: &Path,
    peer: &str,
    topic: Option<String>,
    offline: bool,
) -> Result<QueryReport> {
    let request = Request::new(RequestKind::Query, "help", topic.unwrap_or_default());
    let id = request.id_string();
    let envelope = request.to_envelope();

    let (mut stream, probe) = open_peer(dir, peer, offline).await?.split();
    let result = async {
        stream
            .send(&envelope)
            .await
            .map_err(|e| anyhow::anyhow!("sending the help request: {e}"))?;
        stream
            .finish()
            .await
            .map_err(|e| anyhow::anyhow!("half-closing the request stream: {e}"))?;

        let mut reader = ResponseReader::new(&id);
        let mut terminal: Option<QueryOutcome> = None;
        while let Some(frame) = stream
            .recv()
            .await
            .map_err(|e| anyhow::anyhow!("receiving the help response: {e}"))?
        {
            match reader.accept(frame).context("response choreography")? {
                ResponseEvent::Help { signal, topics } => {
                    terminal = Some(QueryOutcome::Help {
                        text: signal,
                        topics,
                    })
                }
                ResponseEvent::Denied(d) => terminal = Some(QueryOutcome::Denied(d)),
                ResponseEvent::Error(e) => terminal = Some(QueryOutcome::Failed(e)),
                other => bail!("help response carried an unexpected frame: {other:?}"),
            }
        }
        reader.finish().context("response stream truncated")?;
        terminal.context("response stream ended without a terminal frame")
    }
    .await;
    probe.shutdown().await;

    Ok(QueryReport {
        id,
        outcome: result?,
    })
}

/// The terminal shape of one projection fetch.
#[derive(Debug)]
pub enum ProjectionOutcome {
    /// The caller's policy-filtered projection.
    Projection(resonator_protocol::Projection),
    /// The serving side refused.
    Denied(Denied),
    /// An error frame (`point-unknown` for an unoffered path).
    Failed(ErrorEnvelope),
}

/// Fetches the projection at `path` (empty = root) from `peer`. The
/// response is a single `rsntr:Projection` frame.
pub async fn run_projection(
    dir: &Path,
    peer: &str,
    path: &str,
    offline: bool,
) -> Result<ProjectionOutcome> {
    let request = Request::new(RequestKind::Query, "projection", path);
    let envelope = request.to_envelope();

    let (mut stream, probe) = open_peer(dir, peer, offline).await?.split();
    let result = async {
        stream
            .send(&envelope)
            .await
            .map_err(|e| anyhow::anyhow!("sending the projection request: {e}"))?;
        stream
            .finish()
            .await
            .map_err(|e| anyhow::anyhow!("half-closing the request stream: {e}"))?;

        let frame = stream
            .recv()
            .await
            .map_err(|e| anyhow::anyhow!("receiving the projection: {e}"))?;
        match frame {
            Some(EnvelopeObject::Projection(p)) => Ok(ProjectionOutcome::Projection(p)),
            Some(EnvelopeObject::Denied(d)) => Ok(ProjectionOutcome::Denied(d)),
            Some(EnvelopeObject::Error(e)) => Ok(ProjectionOutcome::Failed(e)),
            Some(other) => bail!("unexpected projection response frame: {other:?}"),
            None => bail!("stream ended before the projection frame"),
        }
    }
    .await;
    probe.shutdown().await;
    result
}

/// One item of an entrainment stream, delivered over a channel by
/// [`run_entrain`].
#[derive(Debug)]
pub enum EntrainItem {
    /// The acknowledgment: the caller is entrained.
    Entrained,
    /// One vibration from the entrained point.
    Vibration(resonator_protocol::Vibration),
    /// The node confirmed our damp; the entrainment is over.
    Damped,
    /// The serving side refused the entrainment.
    Denied(Denied),
    /// An error frame (`point-unknown`, or a `limit-exceeded` damp).
    Failed(ErrorEnvelope),
}

/// Entrains to `point` on `peer`, forwarding each stream event over `tx`
/// until the entrainment ends: the node damps it, the connection dies, or
/// `damp` fires (ctrl-c), which sends an in-band `rsntr:Damp` and waits
/// for the confirming Done.
pub async fn run_entrain(
    dir: std::path::PathBuf,
    peer: String,
    point: String,
    offline: bool,
    tx: tokio::sync::mpsc::Sender<EntrainItem>,
    mut damp: tokio::sync::oneshot::Receiver<()>,
) -> Result<()> {
    let id = ulid_string();
    let envelope = EnvelopeObject::Entrain(resonator_protocol::Entrain {
        id: id.clone(),
        point: point.clone(),
    });

    let (mut stream, probe) = open_peer(&dir, &peer, offline).await?.split();
    let result = async {
        // The send side stays open: an in-band Damp may need to go out.
        stream
            .send(&envelope)
            .await
            .map_err(|e| anyhow::anyhow!("sending the entrain request: {e}"))?;

        let mut reader = resonator_protocol::EntrainmentReader::new(&id, &point);
        let mut damp_armed = true;
        loop {
            tokio::select! {
                frame = stream.recv() => {
                    let Some(frame) = frame
                        .map_err(|e| anyhow::anyhow!("receiving from the entrain stream: {e}"))?
                    else {
                        // Connection-scoped end of entrainment.
                        reader.finish().context("entrain stream truncated")?;
                        break;
                    };
                    use resonator_protocol::EntrainmentEvent as Ev;
                    match reader.accept(frame).context("entrainment choreography")? {
                        Ev::Entrained => {
                            if tx.send(EntrainItem::Entrained).await.is_err() {
                                break;
                            }
                        }
                        Ev::Vibration(v) => {
                            if tx.send(EntrainItem::Vibration(v)).await.is_err() {
                                break;
                            }
                        }
                        Ev::Damped => {
                            let _ = tx.send(EntrainItem::Damped).await;
                            break;
                        }
                        Ev::Denied(d) => {
                            let _ = tx.send(EntrainItem::Denied(d)).await;
                            break;
                        }
                        Ev::Error(e) => {
                            let _ = tx.send(EntrainItem::Failed(e)).await;
                            break;
                        }
                    }
                }
                fired = &mut damp, if damp_armed => {
                    damp_armed = false;
                    // A dropped sender is not a damp request; only an
                    // explicit fire sends the in-band Damp.
                    if fired.is_ok() {
                        stream
                            .send(&EnvelopeObject::Damp(resonator_protocol::Damp {
                                id: Some(id.clone()),
                                point: point.clone(),
                            }))
                            .await
                            .map_err(|e| anyhow::anyhow!("sending the damp: {e}"))?;
                        // Keep reading: the confirming Done ends the loop.
                    }
                }
            }
        }
        Ok(())
    }
    .await;
    probe.shutdown().await;
    result
}

/// One item from a streaming media feed, delivered over a channel by
/// [`run_media_channel`]. The `Header` arrives first, then zero or more
/// `Data` chunks in order; `Denied`/`Failed` arrive instead of a header
/// and are the only item.
#[derive(Debug)]
pub enum MediaChunk {
    /// The `rsntr:Media` header: the media type of the `Data` to come.
    Header { content_type: String },
    /// One raw feed chunk, in order.
    Data(Vec<u8>),
    /// The serving side's policy said no.
    Denied(Denied),
    /// A protocol- or engine-level error frame.
    Failed(ErrorEnvelope),
}

/// Opens `source` on `peer` and streams the feed over `tx`: a
/// [`MediaChunk::Header`] first, then each raw chunk, closing the channel
/// at end of feed. A slow receiver applies backpressure through the
/// bounded channel; a dropped receiver ends the feed.
///
/// `paths`, when given, receives how the feed is travelling: once as
/// soon as it starts, then again whenever the selected path changes.
/// A live feed never ends, so a report only at close would never
/// arrive for the case that most needs it; and a path that changes
/// mid-run is itself the answer to "did this connection ever upgrade
/// to direct".
///
/// Returns the last sample, taken just before the connection closes.
/// `None` only when there was no connection left to ask.
pub async fn run_media_channel(
    dir: &Path,
    peer: &str,
    source: &str,
    offline: bool,
    tx: tokio::sync::mpsc::Sender<MediaChunk>,
    paths: Option<tokio::sync::mpsc::Sender<PathReport>>,
) -> Result<Option<PathReport>> {
    let request = Request::new(RequestKind::Query, "media", source);
    let envelope = request.to_envelope();

    let (mut stream, probe) = open_peer(dir, peer, offline).await?.split();
    let sampler = paths.map(|sink| {
        let probe = probe.clone();
        tokio::spawn(async move {
            let mut last: Option<PathReport> = None;
            loop {
                if let Some(now) = probe.sample().await
                    && last.as_ref() != Some(&now)
                {
                    last = Some(now.clone());
                    if sink.send(now).await.is_err() {
                        return;
                    }
                }
                tokio::time::sleep(PATH_REPORT_INTERVAL).await;
            }
        })
    });
    let result = async {
        stream
            .send(&envelope)
            .await
            .map_err(|e| anyhow::anyhow!("sending the media request: {e}"))?;
        stream
            .finish()
            .await
            .map_err(|e| anyhow::anyhow!("half-closing the request stream: {e}"))?;

        let header = stream
            .recv()
            .await
            .map_err(|e| anyhow::anyhow!("receiving the media header: {e}"))?;
        match header {
            Some(EnvelopeObject::Media(m)) => {
                if tx
                    .send(MediaChunk::Header {
                        content_type: m.content_type,
                    })
                    .await
                    .is_err()
                {
                    return Ok(());
                }
                while let Some(chunk) = stream
                    .recv_raw()
                    .await
                    .map_err(|e| anyhow::anyhow!("receiving the media feed: {e}"))?
                {
                    if tx.send(MediaChunk::Data(chunk)).await.is_err() {
                        break;
                    }
                }
                Ok(())
            }
            Some(EnvelopeObject::Denied(d)) => {
                let _ = tx.send(MediaChunk::Denied(d)).await;
                Ok(())
            }
            Some(EnvelopeObject::Error(e)) => {
                let _ = tx.send(MediaChunk::Failed(e)).await;
                Ok(())
            }
            Some(other) => bail!("unexpected media response frame: {other:?}"),
            None => bail!("stream ended before the media header"),
        }
    }
    .await;
    if let Some(s) = sampler {
        s.abort();
    }
    // Sampled before shutdown, while the connection still exists: this
    // is the path that carried the feed, not a fresh probe's path.
    let path = probe.sample().await;
    probe.shutdown().await;
    result.map(|()| path)
}

/// How often a running feed re-checks which path is carrying it.
/// Hole punching resolves in seconds, so this is fast enough to catch
/// an upgrade and slow enough to cost nothing.
const PATH_REPORT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// What a talk session ended as, for the CLI's exit code.
#[derive(Debug)]
pub enum TalkOutcome {
    /// The session ran; the feed and/or stdin ended normally.
    Done,
    Denied(resonator_protocol::Denied),
    Failed(ErrorEnvelope),
}

/// Opens the audio-duplex source `source` on `peer` and pumps: stdin ->
/// the wire (in the header's `rsntr:accepts` format - the caller's job),
/// the wire -> stdout. Stdin EOF sends the wire Fin (the source's stdin
/// sees EOF); the session ends when the downstream ends. The header is
/// reported on stderr; stdout stays pure feed.
pub async fn run_talk(dir: &Path, peer: &str, source: &str, offline: bool) -> Result<TalkOutcome> {
    run_duplex(dir, peer, source, offline, Some("audio-duplex")).await
}

/// The generic duplex byte-stream client behind [`run_talk`] and `rsntr
/// pipe open`: same wire choreography (an `audio-duplex` request; the
/// content types are labels), with the stderr announcement optional so
/// pipe sessions stay quiet.
pub async fn run_duplex(
    dir: &Path,
    peer: &str,
    source: &str,
    offline: bool,
    announce: Option<&str>,
) -> Result<TalkOutcome> {
    let request = Request::new(RequestKind::Query, "audio-duplex", source);
    let envelope = request.to_envelope();

    let (mut stream, probe) = open_peer(dir, peer, offline).await?.split();
    let result = async {
        stream
            .send(&envelope)
            .await
            .map_err(|e| anyhow::anyhow!("sending the duplex request: {e}"))?;
        // No finish: the write half carries the upstream bytes.

        let header = stream
            .recv()
            .await
            .map_err(|e| anyhow::anyhow!("receiving the duplex header: {e}"))?;
        match header {
            Some(EnvelopeObject::AudioDuplex(d)) => {
                if let Some(label) = announce {
                    eprintln!(
                        "{label} open: accepts {} / downstream {}",
                        d.accepts,
                        d.content_type.as_deref().unwrap_or("(none)")
                    );
                }
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut stdin = tokio::io::stdin();
                let mut stdout = tokio::io::stdout();
                let mut inbuf = vec![0u8; 8 * 1024];
                let mut stdin_open = true;
                loop {
                    tokio::select! {
                        r = stdin.read(&mut inbuf), if stdin_open => match r {
                            Ok(0) | Err(_) => {
                                let _ = stream.finish().await;
                                stdin_open = false;
                            }
                            Ok(n) => {
                                if stream.send_raw(&inbuf[..n]).await.is_err() {
                                    break;
                                }
                            }
                        },
                        chunk = stream.recv_raw() => match chunk {
                            Ok(Some(c)) => {
                                stdout.write_all(&c).await?;
                                stdout.flush().await?;
                            }
                            Ok(None) | Err(_) => break,
                        },
                    }
                }
                Ok(TalkOutcome::Done)
            }
            Some(EnvelopeObject::Denied(d)) => Ok(TalkOutcome::Denied(d)),
            Some(EnvelopeObject::Error(e)) => Ok(TalkOutcome::Failed(e)),
            Some(other) => bail!("unexpected audio-duplex response frame: {other:?}"),
            None => bail!("stream ended before the audio-duplex header"),
        }
    }
    .await;
    probe.shutdown().await;
    result
}

/// A fresh ULID string for requests built outside [`Request`].
fn ulid_string() -> String {
    Request::new(RequestKind::Query, "", "").id_string()
}

/// What `rsntr fetch` produced.
#[derive(Debug)]
pub enum FetchOutcome {
    /// The verified bytes were written to the given path.
    Written {
        path: std::path::PathBuf,
        bytes: u64,
    },
    /// The verified bytes, for stdout (no `-o`).
    Bytes(Vec<u8>),
    /// The provider was unreachable (dial failure/timeout): exit 3, the
    /// fetch is retryable whenever the provider is back (chat protocol
    /// sec 6.3).
    Unreachable(String),
}

/// Fetches a blob by BLAKE3 hash from `peer` over iroh-blobs with
/// verified (bao) streaming: to `out` when given, else into memory for
/// stdout. Not chat-specific; it fetches any `rsntr:BlobRef` the
/// envelope handed you.
pub async fn run_fetch(
    dir: &Path,
    peer: &str,
    hash: &str,
    out: Option<&Path>,
    offline: bool,
    dial_timeout: std::time::Duration,
) -> Result<FetchOutcome> {
    // Fail on garbage before any dial.
    resonator_transport::parse_blob_hash(hash).map_err(|e| anyhow::anyhow!("{e}"))?;
    // Not proxied: blobs ride their own ALPN, and the peer proxy tunnels
    // the rdf stream only. A large fetch is a long command, so this is
    // the one remaining long displacement window
    // (docs/relay-identity-sharing.md).
    let (transport, peer_id) = bind_client(dir, peer, offline).await?;
    let result = async {
        let fetched = match out {
            Some(path) => tokio::time::timeout(
                dial_timeout,
                transport.blob_fetch_to_path(peer_id, hash, path),
            )
            .await
            .map(|r| {
                r.map(|bytes| FetchOutcome::Written {
                    path: path.to_path_buf(),
                    bytes,
                })
            }),
            None => tokio::time::timeout(dial_timeout, transport.blob_fetch_bytes(peer_id, hash))
                .await
                .map(|r| r.map(FetchOutcome::Bytes)),
        };
        match fetched {
            Err(_elapsed) => Ok(FetchOutcome::Unreachable(format!(
                "no answer from {peer} within {}s",
                dial_timeout.as_secs()
            ))),
            Ok(Err(resonator_transport::TransportError::Dial(e))) => {
                Ok(FetchOutcome::Unreachable(format!("dialing {peer}: {e}")))
            }
            Ok(Err(e)) => Err(anyhow::anyhow!("fetch failed: {e}")),
            Ok(Ok(outcome)) => Ok(outcome),
        }
    }
    .await;
    transport.shutdown().await;
    result
}

/// Fetches a page from `peer`'s `pages` modulation as a hologram
/// (docs/hologram-protocol.md): a `rsntr:Hologram` header then base64
/// `rsntr:Chunk` frames in `seq` order. Returns `(content_type, bytes)`,
/// verifying the reassembled size against the header's `rsntr:size`.
pub async fn run_pages_fetch(
    dir: &Path,
    peer: &str,
    name: &str,
    offline: bool,
) -> Result<PagesOutcome> {
    let request = Request::new(RequestKind::Query, "pages", format!("hologram {name}"));
    let id = request.id_string();
    let envelope = request.to_envelope();

    let (mut stream, probe) = open_peer(dir, peer, offline).await?.split();
    let result = async {
        stream
            .send(&envelope)
            .await
            .map_err(|e| anyhow::anyhow!("sending the pages request: {e}"))?;
        stream
            .finish()
            .await
            .map_err(|e| anyhow::anyhow!("half-closing the request stream: {e}"))?;

        let mut reader = ResponseReader::new(&id);
        let mut content_type = "text/html; charset=utf-8".to_string();
        let mut declared_size: Option<usize> = None;
        let mut chunks: Vec<(i64, Vec<u8>)> = Vec::new();
        let mut terminal: Option<PagesOutcome> = None;
        while let Some(frame) = stream
            .recv()
            .await
            .map_err(|e| anyhow::anyhow!("receiving the page: {e}"))?
        {
            match reader.accept(frame).context("response choreography")? {
                ResponseEvent::Generic(g) => match g.class.as_str() {
                    "Hologram" => {
                        for (pred, obj) in &g.props {
                            match local_name(pred.as_str()) {
                                "contentType" => {
                                    if let Some(t) = term_text(obj) {
                                        content_type = t;
                                    }
                                }
                                "size" => {
                                    declared_size = term_text(obj).and_then(|s| s.parse().ok());
                                }
                                _ => {}
                            }
                        }
                    }
                    "Chunk" => {
                        let mut seq = 0i64;
                        let mut data = Vec::new();
                        for (pred, obj) in &g.props {
                            match local_name(pred.as_str()) {
                                "seq" => {
                                    seq = term_text(obj).and_then(|s| s.parse().ok()).unwrap_or(0)
                                }
                                "data" => {
                                    use base64::Engine as _;
                                    if let Some(b64) = term_text(obj) {
                                        data = base64::engine::general_purpose::STANDARD
                                            .decode(b64.as_bytes())
                                            .map_err(|e| {
                                                anyhow::anyhow!("bad chunk base64: {e}")
                                            })?;
                                    }
                                }
                                _ => {}
                            }
                        }
                        chunks.push((seq, data));
                    }
                    _ => {}
                },
                ResponseEvent::Done(_) => {
                    chunks.sort_by_key(|(seq, _)| *seq);
                    let bytes: Vec<u8> = chunks.iter().flat_map(|(_, d)| d.clone()).collect();
                    if let Some(size) = declared_size
                        && size != bytes.len()
                    {
                        bail!(
                            "page truncated: header says {size} bytes, got {}",
                            bytes.len()
                        );
                    }
                    terminal = Some(PagesOutcome::Page {
                        content_type: content_type.clone(),
                        bytes,
                    });
                }
                ResponseEvent::Denied(d) => terminal = Some(PagesOutcome::Denied(d)),
                ResponseEvent::Error(e) => terminal = Some(PagesOutcome::Failed(e)),
                _ => {}
            }
        }
        reader.finish().context("response stream truncated")?;
        terminal.context("response ended without a terminal frame")
    }
    .await;
    probe.shutdown().await;
    result
}

/// The terminal shape of a page fetch.
#[derive(Debug)]
pub enum PagesOutcome {
    Page {
        content_type: String,
        bytes: Vec<u8>,
    },
    Denied(Denied),
    Failed(ErrorEnvelope),
}

/// The local name of a `rsntr:` predicate IRI (after the last `#`).
fn local_name(iri: &str) -> &str {
    iri.rsplit(['#', '/']).next().unwrap_or(iri)
}

/// The lexical value of a Term (literal value or node IRI).
fn term_text(t: &oxrdf::Term) -> Option<String> {
    match t {
        oxrdf::Term::NamedNode(n) => Some(n.as_str().to_string()),
        oxrdf::Term::Literal(l) => Some(l.value().to_string()),
        oxrdf::Term::BlankNode(_) => None,
    }
}

/// Fetches a blob by hash from `peer`, dialing exactly `addrs` (the
/// provider's serving addresses, carried in-band). Used by the remote
/// mod-install path so the target reaches the installer's blob-serving
/// daemon rather than a short-lived request endpoint sharing its key.
pub async fn fetch_blob_with_addrs(
    dir: &Path,
    peer: &str,
    hash: &str,
    addrs: &[std::net::SocketAddr],
    offline: bool,
) -> Result<Vec<u8>> {
    resonator_transport::parse_blob_hash(hash).map_err(|e| anyhow::anyhow!("{e}"))?;
    let peer_id: PeerId = peer
        .parse()
        .map_err(|e| anyhow::anyhow!("bad peer id {peer:?}: {e}"))?;
    let secret = store::load_secret(dir)?;
    let hello = {
        let conn = store::open_db(dir)?;
        store::hello_from_db(&conn)
    };
    let (transport, _incoming) = IrohTransport::bind(client_config(hello, secret, offline))
        .await
        .map_err(|e| anyhow::anyhow!("binding the client endpoint: {e}"))?;
    if !addrs.is_empty() {
        transport.add_peer_addrs(peer_id, addrs.to_vec());
    }
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        transport.blob_fetch_bytes(peer_id, hash),
    )
    .await;
    let out = match result {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(e)) => Err(anyhow::anyhow!("fetching the blob: {e}")),
        Err(_) => Err(anyhow::anyhow!("fetching the blob from {peer} timed out")),
    };
    transport.shutdown().await;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_sql_reads_and_writes() {
        assert_eq!(classify_sql("SELECT 1"), RequestKind::Query);
        assert_eq!(
            classify_sql("  with x as (select 1) select * from x"),
            RequestKind::Query
        );
        assert_eq!(classify_sql("VALUES (1)"), RequestKind::Query);
        assert_eq!(
            classify_sql("INSERT INTO t VALUES (1)"),
            RequestKind::Execute
        );
        assert_eq!(classify_sql("update t set a = 1"), RequestKind::Execute);
        assert_eq!(classify_sql(""), RequestKind::Execute);
    }

    #[test]
    fn classify_sparql_forms() {
        assert_eq!(
            classify_sparql("SELECT ?s WHERE { ?s ?p ?o }"),
            RequestKind::Query
        );
        assert_eq!(
            classify_sparql("PREFIX ex: <http://ex/> ASK { ex:a ?p ?o }"),
            RequestKind::Query
        );
        assert_eq!(
            classify_sparql("CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }"),
            RequestKind::Query
        );
        assert_eq!(
            classify_sparql("PREFIX ex: <http://ex/> INSERT DATA { ex:a ex:b ex:c }"),
            RequestKind::Execute
        );
        assert_eq!(
            classify_sparql("DELETE WHERE { ?s ?p ?o }"),
            RequestKind::Execute
        );
    }
}
