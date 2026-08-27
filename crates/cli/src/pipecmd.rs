//! `rsntr pipe`: named binary streams between nodes — the generic face
//! over the media/audio-duplex machinery.
//!
//! A pipe endpoint is a `_media` row with octet-stream content types:
//! the serving node runs its command per connection, the caller's bytes
//! feed its stdin, its stdout feeds the caller (raw, unframed; QUIC
//! flow-controls). `pipe open` is [`crate::client::run_duplex`];
//! gating is the existing `audio-duplex` policy action on the source
//! name (`media` for `--one-way` endpoints read via `rsntr watch`).
//!
//! `pipe accept` needs no pre-registered command: it binds an ephemeral
//! unix socket, registers a temporary `_media` row whose command is the
//! hidden `rsntr pipe-bridge <socket>` helper, and bridges its own
//! stdin/stdout to the next incoming connection; row and socket are
//! removed on exit.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use resonator_protocol::Value;

use crate::channel::{self, OwnerChannel, Prefer};

/// The content type of pipe endpoints in `_media`.
pub const PIPE_CONTENT_TYPE: &str = "application/octet-stream";
/// The `_media.note` marking `pipe accept`'s temporary rows.
const ACCEPT_NOTE: &str = "rsntr pipe accept (ephemeral)";

/// One registered pipe endpoint.
#[derive(Debug)]
pub struct PipeRow {
    pub name: String,
    pub command: String,
    /// False when the endpoint is watch-only (`--one-way`).
    pub duplex: bool,
    pub note: Option<String>,
}

/// `rsntr pipe add <name> <command>`: upserts the `_media` row.
pub async fn pipe_add(
    dir: &Path,
    prefer: Prefer,
    name: &str,
    command: &str,
    one_way: bool,
    note: Option<&str>,
) -> Result<()> {
    let ch = OwnerChannel::open(dir, prefer).await?;
    upsert_row(
        &ch,
        name,
        command,
        one_way,
        note.unwrap_or("rsntr pipe add"),
    )
    .await
}

async fn upsert_row(
    ch: &OwnerChannel,
    name: &str,
    command: &str,
    one_way: bool,
    note: &str,
) -> Result<()> {
    channel::execute(
        ch,
        "INSERT INTO _media (name, command, content_type, accepts, note) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (name) DO UPDATE SET command = ?2, content_type = ?3, \
         accepts = ?4, note = ?5",
        vec![
            Value::Text(name.to_string()),
            Value::Text(command.to_string()),
            Value::Text(PIPE_CONTENT_TYPE.to_string()),
            if one_way {
                Value::Null
            } else {
                Value::Text(PIPE_CONTENT_TYPE.to_string())
            },
            Value::Text(note.to_string()),
        ],
    )
    .await?;
    Ok(())
}

/// `rsntr pipe list`: the octet-stream `_media` rows.
pub async fn pipe_list(dir: &Path, prefer: Prefer) -> Result<Vec<PipeRow>> {
    let ch = OwnerChannel::open(dir, prefer).await?;
    let (_cols, rows, _done) = channel::query_rows(
        &ch,
        "SELECT name, command, accepts, note FROM _media \
         WHERE content_type = ?1 ORDER BY name",
        vec![Value::Text(PIPE_CONTENT_TYPE.to_string())],
    )
    .await?;
    Ok(rows
        .iter()
        .map(|row| PipeRow {
            name: channel::cell_text(row, "name").unwrap_or_default(),
            command: channel::cell_text(row, "command").unwrap_or_default(),
            duplex: channel::cell_text(row, "accepts").is_some(),
            note: channel::cell_text(row, "note"),
        })
        .collect())
}

/// `rsntr pipe rm <name>`: true when the row existed.
pub async fn pipe_rm(dir: &Path, prefer: Prefer, name: &str) -> Result<bool> {
    let ch = OwnerChannel::open(dir, prefer).await?;
    let done = channel::execute(
        &ch,
        "DELETE FROM _media WHERE name = ?1 AND content_type = ?2",
        vec![
            Value::Text(name.to_string()),
            Value::Text(PIPE_CONTENT_TYPE.to_string()),
        ],
    )
    .await?;
    Ok(done.affected_rows.unwrap_or(0) > 0)
}

/// `rsntr pipe accept <name>`: bridge this process's stdin/stdout to the
/// next incoming connection for `name`, via a temporary endpoint. Needs
/// a serving daemon (the daemon runs the bridge command) and, on the
/// dialing side, the `media` grant for `name`.
pub async fn pipe_accept(dir: &Path, prefer: Prefer, name: &str) -> Result<()> {
    let ch = OwnerChannel::open(dir, prefer).await?;

    // Refuse to shadow a real endpoint; replace only our own leftovers.
    let (_cols, rows, _done) = channel::query_rows(
        &ch,
        "SELECT note FROM _media WHERE name = ?1",
        vec![Value::Text(name.to_string())],
    )
    .await?;
    if let Some(row) = rows.first()
        && channel::cell_text(row, "note").as_deref() != Some(ACCEPT_NOTE)
    {
        bail!("a media source named {name:?} already exists; pick another name");
    }

    // The bridge socket: short /tmp path (sockaddr length cap), 0600.
    let sock = PathBuf::from(format!(
        "/tmp/rsntr-pipe-{}-{}.sock",
        std::process::id(),
        ulid::Ulid::new().to_string().to_lowercase()
    ));
    let listener = tokio::net::UnixListener::bind(&sock)
        .with_context(|| format!("binding {}", sock.display()))?;
    std::fs::set_permissions(&sock, {
        use std::os::unix::fs::PermissionsExt;
        std::fs::Permissions::from_mode(0o600)
    })?;

    let exe = std::env::current_exe().context("locating the rsntr binary")?;
    let command = format!("'{}' pipe-bridge '{}'", exe.display(), sock.display());
    upsert_row(&ch, name, &command, false, ACCEPT_NOTE).await?;

    eprintln!("accepting one connection on pipe {name:?}; ctrl-c to give up");
    let session = async {
        let (conn, _addr) = listener.accept().await.context("accepting the bridge")?;
        let (mut conn_read, mut conn_write) = conn.into_split();
        let mut stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let up = async {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match stdin.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if conn_write.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = conn_write.shutdown().await;
        };
        let down = async {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match conn_read.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if stdout.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                        let _ = stdout.flush().await;
                    }
                }
            }
        };
        // The session is over when the peer's downstream closes; a
        // finished stdin only half-closes.
        tokio::join!(up, down);
        Ok::<(), anyhow::Error>(())
    };
    let outcome = tokio::select! {
        r = session => r,
        _ = tokio::signal::ctrl_c() => Ok(()),
    };

    // Cleanup: only our ephemeral row, and the socket inode.
    let _ = channel::execute(
        &ch,
        "DELETE FROM _media WHERE name = ?1 AND note = ?2",
        vec![
            Value::Text(name.to_string()),
            Value::Text(ACCEPT_NOTE.to_string()),
        ],
    )
    .await;
    let _ = std::fs::remove_file(&sock);
    outcome
}

/// The hidden `rsntr pipe-bridge <socket>` helper the serving daemon
/// runs as an accept endpoint's command: stdin -> socket, socket ->
/// stdout, until either side closes.
pub async fn pipe_bridge(sock: &Path) -> Result<()> {
    let conn = tokio::net::UnixStream::connect(sock)
        .await
        .with_context(|| format!("connecting to {}", sock.display()))?;
    let (mut conn_read, mut conn_write) = conn.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let up = async {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match stdin.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if conn_write.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = conn_write.shutdown().await;
    };
    let down = async {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match conn_read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdout.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    let _ = stdout.flush().await;
                }
            }
        }
    };
    tokio::join!(up, down);
    Ok(())
}
