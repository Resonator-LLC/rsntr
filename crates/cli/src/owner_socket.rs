//! The owner channel's control socket (docs/owner-channel.md sec 3.2).
//!
//! While a node serves, it listens on the unix domain socket
//! `<dir>/rsntr.sock` beside `rsntr.db` and `rsntr.key`. The socket
//! speaks exactly the framed envelope protocol (u32-LE length prefix +
//! one self-contained Turtle document, 256 KiB budget). No hello: the
//! first and only frame a client writes is one request envelope
//! (`rsntr:Query`, `rsntr:Execute`, or `rsntr:Entrain`); the response
//! frames stream back on the same connection, then it closes. Requests
//! dispatch through [`Node::handle_owner`] on the serving process's live
//! connection, so commits fire its sqlite update hook and Sympathetic
//! points vibrate.
//!
//! Filesystem permission is the authority: the socket is mode 0600 in a
//! directory the owner controls; there is no token.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream, unix::OwnedReadHalf, unix::OwnedWriteHalf};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use resonator_node::{Node, NodeError};
use resonator_protocol::{
    EnvelopeObject, ErrorEnvelope, decode_envelope, encode_envelope, mod_matches,
};
use resonator_transport::{RequestStream, TransportError};

/// The control socket's file name inside the node directory.
pub const SOCKET_FILE: &str = "rsntr.sock";

/// The socket path for a node directory.
pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join(SOCKET_FILE)
}

/// `sockaddr_un` headroom: paths at or beyond this many bytes cannot be
/// bound or connected (macOS caps them around 104), so they are reached
/// through a short `/tmp` symlink alias instead.
const SUN_PATH_MAX: usize = 100;

fn needs_alias(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().len() >= SUN_PATH_MAX
}

/// A short-lived `/tmp` symlink to the node directory, so
/// `<dir>/rsntr.sock` stays reachable inside the sockaddr length cap.
/// Only the syscall needs the short path; the symlink is removed on drop
/// and the socket inode itself always lives in the node directory.
struct DirAlias {
    link: PathBuf,
}

impl DirAlias {
    fn new(dir: &Path) -> std::io::Result<Self> {
        let target = std::path::absolute(dir)?;
        let mut n: u64 = 0;
        let link = loop {
            let cand = PathBuf::from(format!("/tmp/rsntr-{}-{n:x}", std::process::id()));
            match std::os::unix::fs::symlink(&target, &cand) {
                Ok(()) => break cand,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(e),
            }
        };
        Ok(Self { link })
    }

    fn sock(&self) -> PathBuf {
        self.link.join(SOCKET_FILE)
    }
}

impl Drop for DirAlias {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.link);
    }
}

/// Connects to a node directory's control socket. This is the owner
/// channel's client-side dial: it fails when no node is serving the
/// directory.
pub async fn connect(dir: &Path) -> std::io::Result<UnixStream> {
    let path = socket_path(dir);
    if needs_alias(&path) {
        let alias = DirAlias::new(dir)?;
        UnixStream::connect(alias.sock()).await
    } else {
        UnixStream::connect(&path).await
    }
}

/// A bound control socket: the accept task plus the path to unlink on
/// shutdown.
pub struct OwnerSocket {
    path: PathBuf,
    task: JoinHandle<()>,
}

impl OwnerSocket {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stops accepting and removes the socket file.
    pub async fn shutdown(self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Binds `<dir>/rsntr.sock` (mode 0600) and starts serving owner
/// requests on `node`. A leftover socket from a crashed process (connect
/// refused) is unlinked before rebinding; a live one is an error.
pub async fn bind(dir: &Path, node: Arc<Node>) -> Result<OwnerSocket> {
    let path = socket_path(dir);
    // The syscalls need a path inside the sockaddr cap; deep node
    // directories go through a short-lived /tmp alias.
    let alias = if needs_alias(&path) {
        Some(DirAlias::new(dir).context("creating the short socket alias")?)
    } else {
        None
    };
    let sys_path = alias.as_ref().map_or_else(|| path.clone(), |a| a.sock());
    if path.exists() {
        match UnixStream::connect(&sys_path).await {
            Ok(_) => bail!(
                "another process is already serving the control socket at {}",
                path.display()
            ),
            // Stale: nobody is listening behind the inode.
            Err(_) => {
                std::fs::remove_file(&path)
                    .with_context(|| format!("unlinking stale socket {}", path.display()))?;
            }
        }
    }
    let listener = UnixListener::bind(&sys_path)
        .with_context(|| format!("binding the control socket {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting {} to 0600", path.display()))?;
    let task = tokio::spawn(accept_loop(listener, node));
    Ok(OwnerSocket { path, task })
}

async fn accept_loop(listener: UnixListener, node: Arc<Node>) {
    loop {
        match listener.accept().await {
            Ok((conn, _addr)) => {
                let node = node.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_conn(conn, node).await {
                        debug!(error = %e, "owner socket connection failed");
                    }
                });
            }
            Err(e) => {
                warn!(error = %e, "owner socket accept failed");
                return;
            }
        }
    }
}

/// One connection: read the single request frame, dispatch it through
/// the owner lane, stream the response frames back, close.
async fn handle_conn(conn: UnixStream, node: Arc<Node>) -> Result<(), NodeError> {
    let (read, write) = conn.into_split();
    let mut stream = SocketStream {
        read,
        write,
        buf: BytesMut::new(),
        eof: false,
    };

    let first = match stream.recv().await {
        Ok(Some(obj)) => obj,
        // Connected and closed without a frame: nothing to answer.
        Ok(None) => return Ok(()),
        Err(e) => {
            let _ = stream
                .send(&protocol_error(format!("bad request frame: {e}")))
                .await;
            return Ok(());
        }
    };

    // The media raw byte feed cannot ride the framed socket (v1); answer
    // before dispatch so no Media header frame ever precedes the error.
    if let EnvelopeObject::Query(st) = &first
        && mod_matches("media", &st.modulation)
    {
        let _ = stream
            .send(&EnvelopeObject::Error(ErrorEnvelope {
                id: Some(st.id.clone()),
                code: "mod-unsupported".to_string(),
                reason: Some(
                    "the media raw feed is not served on the control socket in v1".to_string(),
                ),
            }))
            .await;
        return Ok(());
    }

    node.handle_owner(first, &mut stream).await
}

fn protocol_error(reason: String) -> EnvelopeObject {
    EnvelopeObject::Error(ErrorEnvelope {
        id: None,
        code: "protocol-error".to_string(),
        reason: Some(reason),
    })
}

/// Wraps a dialed connection as a client-side framed stream (the owner
/// channel's socket transport in `channel.rs`). The protocol is
/// symmetric, so the serving-side stream type serves both ends.
pub(crate) fn client_stream(conn: UnixStream) -> SocketStream {
    let (read, write) = conn.into_split();
    SocketStream {
        read,
        write,
        buf: BytesMut::new(),
        eof: false,
    }
}

/// The pipeline's view of one socket connection: framed envelopes both
/// ways, no raw byte side channel.
pub(crate) struct SocketStream {
    read: OwnedReadHalf,
    write: OwnedWriteHalf,
    buf: BytesMut,
    eof: bool,
}

impl RequestStream for SocketStream {
    async fn send(&mut self, obj: &EnvelopeObject) -> Result<(), TransportError> {
        let mut out = BytesMut::new();
        encode_envelope(obj, &mut out)
            .map_err(|e| TransportError::Stream(format!("encoding response frame: {e}")))?;
        self.write
            .write_all(&out)
            .await
            .map_err(|e| TransportError::Stream(format!("socket write: {e}")))
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
                .map_err(|e| TransportError::Stream(format!("socket read: {e}")))?;
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

    async fn send_raw(&mut self, _bytes: &[u8]) -> Result<(), TransportError> {
        Err(TransportError::Stream(
            "raw byte streaming is not served on the control socket".into(),
        ))
    }

    async fn finish(&mut self) -> Result<(), TransportError> {
        let _ = self.write.shutdown().await;
        Ok(())
    }
}
