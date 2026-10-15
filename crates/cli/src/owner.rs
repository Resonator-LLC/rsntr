//! The owner channel's transports (docs/owner-channel.md sec 3.2).
//!
//! A serving process listens for owner requests on two local endpoints
//! that speak one protocol: the unix domain socket `<dir>/rsntr.sock`
//! where the platform has one and the path fits `sun_path`, and a
//! loopback TCP port recorded in `<dir>/rsntr.owner` together with a
//! random token (iOS Files-app folders and Windows have no usable unix
//! socket). Both carry exactly the framed envelope protocol (u32-LE
//! length prefix + one Turtle document, 256 KiB budget): the client
//! writes one request frame (`rsntr:Query`, `rsntr:Execute`, or
//! `rsntr:Entrain`), the response frames stream back on the same
//! connection, then it closes. The TCP variant prefixes the token line;
//! the server answers `+` before any frame.
//!
//! Possession of the directory is the authority on both: the socket is
//! mode 0600 in it, and so is the token file. Clients try the socket,
//! then TCP; a record whose port refuses or whose token is rejected is
//! stale (the process that wrote it is gone).

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use resonator_node::{Node, NodeError};
use resonator_protocol::{
    EnvelopeObject, ErrorEnvelope, decode_envelope, encode_envelope, mod_matches,
};
use resonator_transport::{RequestStream, Transport, TransportError};

/// The loopback TCP record's file name inside a node directory.
pub const OWNER_FILE: &str = "rsntr.owner";

/// How long the TCP handshake may take before the connection is dropped.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Path of the TCP record in a node directory.
pub fn owner_path(dir: &Path) -> PathBuf {
    dir.join(OWNER_FILE)
}

/// What `rsntr.owner` records while a process serves.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OwnerRecord {
    pub port: u16,
    /// 64 hex characters (32 random bytes).
    pub token: String,
    pub pid: u32,
    pub app: String,
}

/// Reads the TCP record, if any.
pub fn read_record(dir: &Path) -> std::io::Result<OwnerRecord> {
    let raw = std::fs::read_to_string(owner_path(dir))?;
    serde_json::from_str(&raw).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// The framed envelope stream over any local byte stream. The protocol
/// is symmetric, so the serving side and the client side share it.
pub struct OwnerStream {
    read: Box<dyn AsyncRead + Unpin + Send>,
    write: Box<dyn AsyncWrite + Unpin + Send>,
    buf: BytesMut,
    eof: bool,
}

impl OwnerStream {
    pub fn new(
        read: impl AsyncRead + Unpin + Send + 'static,
        write: impl AsyncWrite + Unpin + Send + 'static,
    ) -> Self {
        Self {
            read: Box::new(read),
            write: Box::new(write),
            buf: BytesMut::new(),
            eof: false,
        }
    }

    /// The two halves plus whatever was read past the last frame, for
    /// code that must read and write at the same time (the peer proxy
    /// splices in both directions at once). The leftover bytes belong to
    /// the caller: they were already taken off the wire.
    pub fn into_parts(
        self,
    ) -> (
        Box<dyn AsyncRead + Unpin + Send>,
        Box<dyn AsyncWrite + Unpin + Send>,
        BytesMut,
    ) {
        (self.read, self.write, self.buf)
    }
}

impl RequestStream for OwnerStream {
    async fn send(&mut self, obj: &EnvelopeObject) -> Result<(), TransportError> {
        let mut out = BytesMut::new();
        encode_envelope(obj, &mut out)
            .map_err(|e| TransportError::Stream(format!("encoding response frame: {e}")))?;
        self.write
            .write_all(&out)
            .await
            .map_err(|e| TransportError::Stream(format!("owner channel write: {e}")))
    }

    async fn recv(&mut self) -> Result<Option<EnvelopeObject>, TransportError> {
        loop {
            match decode_envelope(&mut self.buf) {
                Ok(Some(obj)) => return Ok(Some(obj)),
                Ok(None) => {}
                Err(e) => return Err(TransportError::Stream(format!("decoding frame: {e}"))),
            }
            if self.eof {
                return Ok(None);
            }
            let n = self
                .read
                .read_buf(&mut self.buf)
                .await
                .map_err(|e| TransportError::Stream(format!("owner channel read: {e}")))?;
            if n == 0 {
                self.eof = true;
                if self.buf.is_empty() {
                    return Ok(None);
                }
                return Err(TransportError::Stream(
                    "connection closed mid-frame".to_string(),
                ));
            }
        }
    }

    /// Raw bytes are positional on this stream exactly as they are on a
    /// peer stream: everything after the framed part. That equivalence
    /// is what lets the owner channel carry a proxied peer session
    /// unchanged once the handshake frames are past.
    async fn send_raw(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.write
            .write_all(bytes)
            .await
            .map_err(|e| TransportError::Stream(format!("owner channel raw write: {e}")))
    }

    /// Anything already read past the framed part first, then straight
    /// off the wire.
    async fn recv_raw(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        if !self.buf.is_empty() {
            return Ok(Some(self.buf.split().to_vec()));
        }
        if self.eof {
            return Ok(None);
        }
        let n = self
            .read
            .read_buf(&mut self.buf)
            .await
            .map_err(|e| TransportError::Stream(format!("owner channel raw read: {e}")))?;
        if n == 0 {
            self.eof = true;
            return Ok(None);
        }
        Ok(Some(self.buf.split().to_vec()))
    }

    async fn finish(&mut self) -> Result<(), TransportError> {
        let _ = self.write.shutdown().await;
        Ok(())
    }
}

/// Dials the process serving `dir`: the unix socket where it exists,
/// else loopback TCP with the recorded token. Fails when nobody serves
/// (or only a stale record remains).
pub async fn connect(dir: &Path) -> std::io::Result<OwnerStream> {
    #[cfg(unix)]
    {
        if crate::owner_socket::socket_path(dir).exists()
            && let Ok(conn) = crate::owner_socket::connect(dir).await
        {
            let (read, write) = conn.into_split();
            return Ok(OwnerStream::new(read, write));
        }
    }
    connect_tcp(dir).await
}

/// Dials the loopback TCP endpoint recorded in `<dir>/rsntr.owner` and
/// completes the token handshake.
pub async fn connect_tcp(dir: &Path) -> std::io::Result<OwnerStream> {
    let record = read_record(dir)?;
    let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, record.port)).await?;
    let _ = stream.set_nodelay(true);
    let (mut read, mut write) = stream.into_split();
    write
        .write_all(format!("{}\n", record.token).as_bytes())
        .await?;
    let mut ack = [0u8; 1];
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, read.read_exact(&mut ack)).await {
        Ok(Ok(_)) if ack == *b"+" => Ok(OwnerStream::new(read, write)),
        Ok(Ok(_)) | Ok(Err(_)) => Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the owner channel refused the token in rsntr.owner (stale record?)",
        )),
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "the owner channel did not answer the handshake",
        )),
    }
}

/// True when some process answers owner requests for `dir`.
pub async fn probe(dir: &Path) -> bool {
    connect(dir).await.is_ok()
}

/// Entrains `point` on the serving process over one fresh owner-channel
/// connection: the request is sent; the caller drives the stream with
/// the returned [`EntrainmentReader`] (Entrained, Vibration, Damped,
/// Denied, Error) and damps by sending `rsntr:Damp` with the returned
/// id, or simply by dropping the stream (closing is damping).
pub async fn entrain(
    dir: &Path,
    point: &str,
) -> anyhow::Result<(OwnerStream, resonator_protocol::EntrainmentReader, String)> {
    let mut stream = connect(dir).await.map_err(|e| {
        anyhow::anyhow!(
            "no node is serving {} (owner channel: {e}); start one with `rsntr serve {}`",
            dir.display(),
            dir.display()
        )
    })?;
    let id = ulid::Ulid::new().to_string();
    stream
        .send(&EnvelopeObject::Entrain(resonator_protocol::Entrain {
            id: id.clone(),
            point: point.to_string(),
        }))
        .await
        .map_err(|e| anyhow::anyhow!("sending the entrain request: {e}"))?;
    let reader = resonator_protocol::EntrainmentReader::new(&id, point);
    Ok((stream, reader, id))
}

/// Constant-time equality for the token line.
pub(crate) fn token_matches(expected: &str, presented: &[u8]) -> bool {
    let e = expected.as_bytes();
    if e.len() != presented.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in e.iter().zip(presented) {
        diff |= a ^ b;
    }
    diff == 0
}

/// One accepted connection, on either transport: read the single
/// request frame, dispatch it through the owner lane on the serving
/// process's live connection, stream the response frames back, close.
pub async fn serve_connection(
    mut stream: OwnerStream,
    node: Arc<Node>,
    transport: Option<Arc<resonator_transport::IrohTransport>>,
) -> Result<(), NodeError> {
    let first = match stream.recv().await {
        Ok(Some(obj)) => obj,
        // Connected and closed without a frame (a probe): nothing to answer.
        Ok(None) => return Ok(()),
        Err(e) => {
            let _ = stream
                .send(&protocol_error(format!("bad request frame: {e}")))
                .await;
            return Ok(());
        }
    };

    // A proxied peer session: the caller wants this process to reach a
    // peer on its behalf, so that one identity means one endpoint even
    // while a client command runs (docs/relay-identity-sharing.md).
    if let EnvelopeObject::Query(st) = &first
        && mod_matches(PROXY_MOD, &st.modulation)
    {
        return proxy_to_peer(stream, node, transport, st.signal.clone(), st.id.clone()).await;
    }
    // What the daemon currently sees on a connection to a peer. Asked on
    // its own short connection so a running proxy session can be sampled
    // without disturbing it.
    if let EnvelopeObject::Query(st) = &first
        && mod_matches(PROXY_PATH_MOD, &st.modulation)
    {
        return answer_peer_path(stream, node, transport, st.signal.clone(), st.id.clone()).await;
    }

    // The media raw byte feed cannot ride the framed channel (v1); answer
    // before dispatch so no Media header frame ever precedes the error.
    if let EnvelopeObject::Query(st) = &first
        && mod_matches("media", &st.modulation)
    {
        let _ = stream
            .send(&EnvelopeObject::Error(ErrorEnvelope {
                id: Some(st.id.clone()),
                code: "mod-unsupported".to_string(),
                reason: Some(
                    "the media raw feed is not served on the owner channel; \
                     a proxied peer session carries it (rsntr:peer-proxy)"
                        .to_string(),
                ),
            }))
            .await;
        return Ok(());
    }

    node.handle_owner(first, &mut stream).await
}

/// The reserved modulation that asks the serving process to open a
/// stream to a peer on the caller's behalf.
///
/// Every client command that reaches a peer directly binds its own iroh
/// endpoint with this node's own key, so a client and its daemon become
/// two endpoints sharing one endpoint id. A relay forwards only to the
/// most recently registered of those, which leaves the daemon deaf to
/// relayed traffic for as long as the client runs
/// (docs/relay-identity-sharing.md). Routing the request through the
/// process that already holds the identity keeps that promise: one
/// identity, one endpoint.
pub const PROXY_MOD: &str = "peer-proxy";

/// Asks the serving process which path it is currently using to a peer.
/// Its own short connection, so a running proxy session can be sampled
/// without being disturbed.
pub const PROXY_PATH_MOD: &str = "peer-path";

/// Copies bytes between the owner connection and a stream to `target`
/// until either end is done.
///
/// After the peer's hello goes back as one frame the connection is a
/// plain byte tunnel: raw bytes are positional on both stream kinds
/// (everything after the framed part), so the caller speaks the ordinary
/// peer protocol over it and nothing here has to understand a single
/// frame. Half-closing the caller's write side finishes the peer stream,
/// which is what a request/response verb does after sending its request.
async fn proxy_to_peer(
    mut stream: OwnerStream,
    node: Arc<Node>,
    transport: Option<Arc<resonator_transport::IrohTransport>>,
    target: String,
    id: String,
) -> Result<(), NodeError> {
    let Some(transport) = transport else {
        let _ = stream
            .send(&EnvelopeObject::Error(ErrorEnvelope {
                id: Some(id),
                code: "mod-unsupported".to_string(),
                reason: Some(
                    "this process serves the directory but holds no transport, \
                     so it cannot reach a peer for you"
                        .to_string(),
                ),
            }))
            .await;
        return Ok(());
    };
    let peer = match resolve_peer_id(&node, &target).await {
        Ok(p) => p,
        Err(reason) => {
            let _ = stream
                .send(&EnvelopeObject::Error(ErrorEnvelope {
                    id: Some(id),
                    code: "bad-request".to_string(),
                    reason: Some(reason),
                }))
                .await;
            return Ok(());
        }
    };
    let (peer_stream, hello) = match transport.open(peer).await {
        Ok(v) => v,
        Err(e) => {
            let _ = stream
                .send(&EnvelopeObject::Error(ErrorEnvelope {
                    id: Some(id),
                    code: "connection-error".to_string(),
                    reason: Some(format!("connecting to {target}: {e}")),
                }))
                .await;
            return Ok(());
        }
    };
    // The peer answered, so the caller can start speaking to it. This
    // frame is the last thing either side interprets.
    if stream.send(&EnvelopeObject::Hello(hello)).await.is_err() {
        return Ok(());
    }

    let (mut from_caller, mut to_caller, pending) = stream.into_parts();
    let (mut to_peer, mut from_peer, spare) = peer_stream.into_parts();

    // Whatever each side had already read past its last frame goes first;
    // it is off the wire and belongs to the tunnel.
    if !pending.is_empty() && to_peer.write_all(&pending).await.is_err() {
        return Ok(());
    }
    if !spare.is_empty() && to_caller.write_all(&spare).await.is_err() {
        return Ok(());
    }

    let up = tokio::spawn(async move {
        let mut chunk = vec![0u8; PROXY_CHUNK];
        loop {
            match from_caller.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if to_peer.write_all(&chunk[..n]).await.is_err() {
                        return;
                    }
                }
            }
        }
        // The caller half-closed: the peer must see the same end of
        // request it would see from a direct client.
        let _ = to_peer.finish();
    });
    let down = tokio::spawn(async move {
        let mut chunk = vec![0u8; PROXY_CHUNK];
        loop {
            match from_peer.read(&mut chunk).await {
                Ok(Some(0)) => continue,
                Ok(Some(n)) => {
                    if to_caller.write_all(&chunk[..n]).await.is_err() {
                        return;
                    }
                }
                Ok(None) | Err(_) => break,
            }
        }
        let _ = to_caller.shutdown().await;
    });
    // The session ends when the peer is done talking. The upward copy is
    // then pointless: a client that is still writing has nobody to hear
    // it.
    let _ = down.await;
    up.abort();
    Ok(())
}

/// Answers `PROXY_PATH_MOD`: how this process is currently reaching a
/// peer, as one `rsntr:Generic` frame, or an error when it has no live
/// connection to sample.
async fn answer_peer_path(
    mut stream: OwnerStream,
    node: Arc<Node>,
    transport: Option<Arc<resonator_transport::IrohTransport>>,
    target: String,
    id: String,
) -> Result<(), NodeError> {
    let report = match (transport, resolve_peer_id(&node, &target).await) {
        (Some(t), Ok(peer)) => t.peer_path(peer).await,
        _ => None,
    };
    let frame = match report {
        // The report rides as JSON in the signal rather than as five new
        // predicates: it is a diagnostic between two processes of the
        // same build, not vocabulary a peer has to understand.
        Some(p) => EnvelopeObject::Generic(resonator_protocol::Generic {
            class: "PeerPath".to_string(),
            props: vec![
                (
                    resonator_protocol::vocab::prop::ID.into_owned(),
                    oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(id)),
                ),
                (
                    resonator_protocol::vocab::prop::SIGNAL.into_owned(),
                    oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(
                        serde_json::json!({
                            "relay": p.relay,
                            "remote": p.remote,
                            "rtt_ms": p.rtt_ms,
                            "paths": p.paths,
                            "direct": p.direct,
                        })
                        .to_string(),
                    )),
                ),
            ],
        }),
        None => EnvelopeObject::Error(ErrorEnvelope {
            id: Some(id),
            code: "engine-error".to_string(),
            reason: Some(format!("no live connection to {target} to sample")),
        }),
    };
    let _ = stream.send(&frame).await;
    Ok(())
}

/// Resolves a petname or 64-hex id against the serving node's `_peers`.
async fn resolve_peer_id(
    node: &Arc<Node>,
    target: &str,
) -> Result<resonator_transport::PeerId, String> {
    let target = target.to_string();
    node.db()
        .call(move |conn| crate::store::resolve_peer(conn, &target).map(|(id, _)| id))
        .await
        .map_err(|e| format!("looking up the peer: {e}"))?
        .map_err(|e| e.to_string())
}

/// Copy buffer for a proxied session. Matches the transport's own read
/// chunk so a media feed moves in the same units it arrives in.
const PROXY_CHUNK: usize = 16 * 1024;

fn protocol_error(reason: String) -> EnvelopeObject {
    EnvelopeObject::Error(ErrorEnvelope {
        id: None,
        code: "protocol-error".to_string(),
        reason: Some(reason),
    })
}

#[cfg(test)]
mod tests {
    use super::token_matches;

    #[test]
    fn token_compare() {
        assert!(token_matches("abc", b"abc"));
        assert!(!token_matches("abc", b"abd"));
        assert!(!token_matches("abc", b"ab"));
        assert!(!token_matches("", b"a"));
    }
}
