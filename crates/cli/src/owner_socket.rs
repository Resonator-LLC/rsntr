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
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use resonator_node::{Node, NodeError};

/// The control socket's file name inside the node directory.
pub const SOCKET_FILE: &str = "rsntr.sock";

/// The socket path for a node directory.
pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join(SOCKET_FILE)
}

/// Bytes of `sockaddr_un.sun_path`, NUL included: 104 on Darwin, 108 on
/// Linux and Android. A path that does not fit (with its NUL) cannot be
/// bound or connected directly.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub const SUN_PATH_LEN: usize = 104;
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub const SUN_PATH_LEN: usize = 108;

fn path_bytes(path: &Path) -> usize {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().len()
}

/// True when `path` is too long for `sun_path` and must go through an
/// alias for the syscall.
pub fn needs_alias(path: &Path) -> bool {
    path_bytes(path) + 1 > SUN_PATH_LEN
}

/// True when the node directory's socket path can be bound at all:
/// directly, or through a short alias in the temp dir. When false the
/// serving process skips the unix socket (the loopback TCP owner
/// channel covers it); iOS Files-app folders are the typical case.
pub fn fits(dir: &Path) -> bool {
    let path = socket_path(dir);
    if !needs_alias(&path) {
        return true;
    }
    !needs_alias(&alias_candidate(0).join(SOCKET_FILE))
}

fn alias_candidate(n: u64) -> PathBuf {
    std::env::temp_dir().join(format!("r{:x}{n:x}", std::process::id()))
}

/// A short-lived symlink in the temp dir to the node directory, so
/// `<dir>/rsntr.sock` stays reachable inside the sockaddr length cap.
/// Only the syscall needs the short path; the symlink is removed on drop
/// and the socket inode itself always lives in the node directory. The
/// alias is per process (pid-named), so any writable temp dir works,
/// including an app's private one.
struct DirAlias {
    link: PathBuf,
}

impl DirAlias {
    fn new(dir: &Path) -> std::io::Result<Self> {
        let target = std::path::absolute(dir)?;
        let mut n: u64 = 0;
        let link = loop {
            let cand = alias_candidate(n);
            if needs_alias(&cand.join(SOCKET_FILE)) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "the node directory path is too long for a unix socket ({} bytes; \
                         the cap is {}) and the temp dir alias does not fit either",
                        path_bytes(&target.join(SOCKET_FILE)),
                        SUN_PATH_LEN - 1
                    ),
                ));
            }
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
/// refused) is unlinked before rebinding; a live one is an error (the
/// serving lease decides who serves; a live socket here is a squatter).
pub async fn bind(
    dir: &Path,
    node: Arc<Node>,
    transport: Option<Arc<resonator_transport::IrohTransport>>,
) -> Result<OwnerSocket> {
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
    let task = tokio::spawn(accept_loop(listener, node, transport));
    Ok(OwnerSocket { path, task })
}

async fn accept_loop(
    listener: UnixListener,
    node: Arc<Node>,
    transport: Option<Arc<resonator_transport::IrohTransport>>,
) {
    loop {
        match listener.accept().await {
            Ok((conn, _addr)) => {
                let node = node.clone();
                let transport = transport.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_conn(conn, node, transport).await {
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

/// One connection: the owner channel's shared lifecycle over the socket.
async fn handle_conn(
    conn: UnixStream,
    node: Arc<Node>,
    transport: Option<Arc<resonator_transport::IrohTransport>>,
) -> Result<(), NodeError> {
    let (read, write) = conn.into_split();
    crate::owner::serve_connection(crate::owner::OwnerStream::new(read, write), node, transport)
        .await
}

/// Wraps a dialed connection as a client-side framed stream.
pub fn client_stream(conn: UnixStream) -> crate::owner::OwnerStream {
    let (read, write) = conn.into_split();
    crate::owner::OwnerStream::new(read, write)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_boundaries_are_exact() {
        // sun_path holds SUN_PATH_LEN bytes including the NUL: a path of
        // SUN_PATH_LEN - 1 bytes is the longest that binds directly.
        let fitting = format!("/{}", "a".repeat(SUN_PATH_LEN - 2));
        assert_eq!(fitting.len(), SUN_PATH_LEN - 1);
        assert!(!needs_alias(Path::new(&fitting)));
        let overflowing = format!("/{}", "a".repeat(SUN_PATH_LEN - 1));
        assert_eq!(overflowing.len(), SUN_PATH_LEN);
        assert!(needs_alias(Path::new(&overflowing)));
        assert!(needs_alias(Path::new(&format!("/{}", "a".repeat(200)))));
    }
}
