//! Client-side round trips: query, help, projection, entrain, media.
//!
//! Each function binds a short-lived client endpoint with the node's own
//! key (so the serving side sees the admitted identity), dials the peer
//! with any `_peers.addrs` hints, sends one request on a fresh bi stream,
//! and drives the protocol crate's readers over the answer.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};

use resonator_protocol::{
    Decision, Denied, Done, EnvelopeObject, ErrorEnvelope, Knock, Request, RequestKind,
    ResponseEvent, ResponseReader, Row, Value, mod_matches,
};
use resonator_transport::{IrohConfig, IrohTransport, PeerId, RequestStream, Transport};

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

    let config = IrohConfig {
        hello,
        secret_key: Some(secret),
        offline,
        relays: Vec::new(),
        gossip: false,
        blobs: None,
    };
    let (transport, _incoming) = IrohTransport::bind(config)
        .await
        .map_err(|e| anyhow::anyhow!("binding the client endpoint: {e}"))?;
    if !addrs.is_empty() {
        transport.add_peer_addrs(peer_id, addrs);
    }
    Ok((transport, peer_id))
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

    let (transport, peer_id) = bind_client(dir, peer, offline).await?;
    let result = async {
        let (mut stream, _peer_hello) = transport
            .open(peer_id)
            .await
            .map_err(|e| anyhow::anyhow!("connecting to {peer}: {e}"))?;
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
    transport.shutdown().await;

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
    let (transport, peer_id) = bind_client(dir, peer, offline).await?;
    let result = async {
        let (mut stream, _peer_hello) = transport
            .open(peer_id)
            .await
            .map_err(|e| anyhow::anyhow!("connecting to {peer}: {e}"))?;
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
    transport.shutdown().await;
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

    let (transport, peer_id) = bind_client(dir, peer, offline).await?;
    let result = async {
        let (mut stream, _peer_hello) = transport
            .open(peer_id)
            .await
            .map_err(|e| anyhow::anyhow!("connecting to {peer}: {e}"))?;
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
    transport.shutdown().await;

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

    let (transport, peer_id) = bind_client(dir, peer, offline).await?;
    let result = async {
        let (mut stream, _peer_hello) = transport
            .open(peer_id)
            .await
            .map_err(|e| anyhow::anyhow!("connecting to {peer}: {e}"))?;
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
    transport.shutdown().await;
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

    let (transport, peer_id) = bind_client(&dir, &peer, offline).await?;
    let result = async {
        let (mut stream, _peer_hello) = transport
            .open(peer_id)
            .await
            .map_err(|e| anyhow::anyhow!("connecting to {peer}: {e}"))?;
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
    transport.shutdown().await;
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
pub async fn run_media_channel(
    dir: &Path,
    peer: &str,
    source: &str,
    offline: bool,
    tx: tokio::sync::mpsc::Sender<MediaChunk>,
) -> Result<()> {
    let request = Request::new(RequestKind::Query, "media", source);
    let envelope = request.to_envelope();

    let (transport, peer_id) = bind_client(dir, peer, offline).await?;
    let result = async {
        let (mut stream, _peer_hello) = transport
            .open(peer_id)
            .await
            .map_err(|e| anyhow::anyhow!("connecting to {peer}: {e}"))?;
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
    transport.shutdown().await;
    result
}

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
    let request = Request::new(RequestKind::Query, "audio-duplex", source);
    let envelope = request.to_envelope();

    let (transport, peer_id) = bind_client(dir, peer, offline).await?;
    let result = async {
        let (mut stream, _peer_hello) = transport
            .open(peer_id)
            .await
            .map_err(|e| anyhow::anyhow!("connecting to {peer}: {e}"))?;
        stream
            .send(&envelope)
            .await
            .map_err(|e| anyhow::anyhow!("sending the audio-duplex request: {e}"))?;
        // No finish: the write half carries the upstream audio.

        let header = stream
            .recv()
            .await
            .map_err(|e| anyhow::anyhow!("receiving the audio-duplex header: {e}"))?;
        match header {
            Some(EnvelopeObject::AudioDuplex(d)) => {
                eprintln!(
                    "audio-duplex open: accepts {} / downstream {}",
                    d.accepts,
                    d.content_type.as_deref().unwrap_or("(none)")
                );
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
    transport.shutdown().await;
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
