//! The owner channel over loopback TCP (crate::owner): the transport
//! for directories a unix socket cannot serve (an iOS Files-app folder,
//! whose path exceeds `sun_path`; Windows), bound by every serving
//! process regardless. A random token in `<dir>/rsntr.owner` (0600)
//! gates the port: possession of the directory is the authority, as
//! for the socket.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use resonator_node::Node;

use crate::owner::{HANDSHAKE_TIMEOUT, OwnerRecord, OwnerStream, owner_path, token_matches};

/// A bound loopback owner endpoint: the accept task plus the record to
/// remove on shutdown.
pub struct OwnerTcp {
    path: PathBuf,
    port: u16,
    task: JoinHandle<()>,
}

impl OwnerTcp {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Stops accepting and removes the record.
    pub async fn shutdown(self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Binds `127.0.0.1:0`, writes `<dir>/rsntr.owner` (tmp+rename, 0600),
/// and serves owner requests on `node` for every connection presenting
/// the token.
pub async fn bind(
    dir: &Path,
    node: Arc<Node>,
    app_id: &str,
    transport: Option<Arc<resonator_transport::IrohTransport>>,
) -> Result<OwnerTcp> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .context("binding the loopback owner port")?;
    let port = listener.local_addr()?.port();
    let token: String = iroh::SecretKey::generate()
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let record = OwnerRecord {
        port,
        token: token.clone(),
        pid: std::process::id(),
        app: app_id.to_string(),
    };
    let path = owner_path(dir);
    let tmp = dir.join(format!(
        "{}.tmp-{}",
        crate::owner::OWNER_FILE,
        std::process::id()
    ));
    std::fs::write(&tmp, format!("{}\n", serde_json::to_string(&record)?))
        .with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restricting {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
    let token = Arc::new(token);
    let task = tokio::spawn(accept_loop(listener, node, token, transport));
    Ok(OwnerTcp { path, port, task })
}

async fn accept_loop(
    listener: TcpListener,
    node: Arc<Node>,
    token: Arc<String>,
    transport: Option<Arc<resonator_transport::IrohTransport>>,
) {
    loop {
        match listener.accept().await {
            Ok((conn, _addr)) => {
                let node = node.clone();
                let token = token.clone();
                let transport = transport.clone();
                tokio::spawn(async move {
                    let Some(stream) = handshake(conn, &token).await else {
                        return;
                    };
                    if let Err(e) = crate::owner::serve_connection(stream, node, transport).await {
                        debug!(error = %e, "owner tcp connection failed");
                    }
                });
            }
            Err(e) => {
                warn!(error = %e, "owner tcp accept failed");
                return;
            }
        }
    }
}

/// Reads the token line and answers `+` when it matches; anything else
/// (wrong token, no line within the timeout) closes the connection
/// without a byte, so a wrong guess learns nothing.
async fn handshake(conn: TcpStream, token: &str) -> Option<OwnerStream> {
    let _ = conn.set_nodelay(true);
    let (mut read, mut write) = conn.into_split();
    let mut line = vec![0u8; token.len() + 1];
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, read.read_exact(&mut line)).await {
        Ok(Ok(_)) => {}
        _ => return None,
    }
    if line.last() != Some(&b'\n') || !token_matches(token, &line[..token.len()]) {
        debug!("owner tcp handshake refused");
        return None;
    }
    if write.write_all(b"+").await.is_err() {
        return None;
    }
    Some(OwnerStream::new(read, write))
}
