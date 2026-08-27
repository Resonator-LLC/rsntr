//! Daemonized serving: `rsntr serve` (detached by default), `rsntr
//! stop`, and `rsntr status`.
//!
//! The detach path re-execs `rsntr serve <dir> --foreground` as a
//! session leader with stdio pointed at `<dir>/rsntr.log`, waits for the
//! control socket to answer, and reports one JSON-able summary. The
//! foreground process owns the pid file (`<dir>/rsntr.pid`) and the
//! `_rsntr.serving_ticket` row; both appear at startup and are removed
//! on graceful shutdown, so `stop` and `status` read them as the
//! daemon's own record rather than the parent's guess.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::{owner_socket, store};

/// Pid file name inside a node directory.
pub const PID_FILE: &str = "rsntr.pid";
/// Daemon log file name inside a node directory.
pub const LOG_FILE: &str = "rsntr.log";
/// The `_rsntr` key holding the live ticket while a node serves.
pub const SERVING_TICKET_KEY: &str = "serving_ticket";

/// Path to the pid file of a node directory.
pub fn pid_path(dir: &Path) -> PathBuf {
    dir.join(PID_FILE)
}

/// Path to the daemon log of a node directory.
pub fn log_path(dir: &Path) -> PathBuf {
    dir.join(LOG_FILE)
}

/// What `rsntr serve` (detached) reports.
#[derive(Debug)]
pub struct DetachReport {
    /// A daemon was already serving; nothing was spawned.
    pub already_running: bool,
    /// The daemon's pid, when known.
    pub pid: Option<i32>,
    pub socket: PathBuf,
    pub endpoint_id: String,
    /// The live dialing ticket, once the daemon published it.
    pub ticket: Option<String>,
}

/// What `rsntr stop` did.
#[derive(Debug)]
pub struct StopReport {
    /// True when a daemon was actually signalled down.
    pub stopped: bool,
    /// True when nothing was serving in the first place.
    pub already_stopped: bool,
    pub pid: Option<i32>,
    /// SIGKILL was needed after the graceful window.
    pub forced: bool,
}

/// What `rsntr status` reports.
#[derive(Debug)]
pub struct StatusReport {
    pub serving: bool,
    pub pid: Option<i32>,
    pub endpoint_id: String,
    pub addrs: Vec<String>,
    pub ticket: Option<String>,
    pub peers: i64,
    pub pending_inbox: i64,
}

/// True when `pid` names a live process.
fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// The recorded daemon pid, when the file exists, parses, and the
/// process is alive and looks like an rsntr serve (guards against pid
/// reuse; the cmdline check is Linux-only and passes elsewhere).
fn read_live_pid(dir: &Path) -> Option<i32> {
    let raw = std::fs::read_to_string(pid_path(dir)).ok()?;
    let pid: i32 = raw.trim().parse().ok()?;
    if !pid_alive(pid) {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let text = String::from_utf8_lossy(&cmdline);
        if !(text.contains("rsntr") && text.contains("serve")) {
            return None;
        }
    }
    Some(pid)
}

/// Reads one `_rsntr` value straight off the database (no channel).
fn read_rsntr(dir: &Path, key: &str) -> Option<String> {
    let conn = store::open_db(dir).ok()?;
    resonator_node::get_rsntr(&conn, key).filter(|v| !v.is_empty())
}

/// True when a daemon answers on the control socket.
async fn socket_alive(dir: &Path) -> bool {
    owner_socket::connect(dir).await.is_ok()
}

/// `rsntr serve <dir>` without `--foreground`: spawn the daemon (unless
/// one is already serving), wait for the socket, report. Idempotent.
pub async fn serve_detached(
    dir: &Path,
    offline: bool,
    web: Option<std::net::SocketAddr>,
    new_web_token: bool,
) -> Result<DetachReport> {
    let endpoint_id = store::node_id(dir)?.to_string();
    let socket = owner_socket::socket_path(dir);

    if socket_alive(dir).await {
        return Ok(DetachReport {
            already_running: true,
            pid: read_live_pid(dir),
            socket,
            endpoint_id,
            ticket: read_rsntr(dir, SERVING_TICKET_KEY),
        });
    }

    let exe = std::env::current_exe().context("locating the rsntr binary")?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(dir))
        .with_context(|| format!("opening {}", log_path(dir).display()))?;
    let log_err = log.try_clone().context("cloning the log handle")?;

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("serve").arg(dir).arg("--foreground");
    if offline {
        cmd.arg("--offline");
    }
    if let Some(addr) = web {
        cmd.arg("--web").arg(addr.to_string());
    }
    if new_web_token {
        cmd.arg("--new-web-token");
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log_err);
    // A session of its own: the daemon must survive this process's
    // terminal and never receive its ctrl-c.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().context("spawning the serve daemon")?;
    let pid = child.id() as i32;

    // Wait for the control socket to answer; a child that exits first
    // failed to start (its reason is in the log).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if socket_alive(dir).await {
            break;
        }
        if let Some(status) = child.try_wait().context("checking the daemon")? {
            let tail = std::fs::read_to_string(log_path(dir))
                .ok()
                .map(|t| {
                    t.lines()
                        .rev()
                        .take(5)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            bail!("the serve daemon exited at startup ({status}); its log ends with:\n{tail}");
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "the serve daemon (pid {pid}) did not open {} within 15s; see {}",
                socket.display(),
                log_path(dir).display()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // The ticket appears once the endpoint has learned an address
    // (ready_ticket waits up to 3s in the daemon).
    let ticket_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let ticket = loop {
        if let Some(t) = read_rsntr(dir, SERVING_TICKET_KEY) {
            break Some(t);
        }
        if tokio::time::Instant::now() >= ticket_deadline {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    Ok(DetachReport {
        already_running: false,
        pid: Some(pid),
        socket,
        endpoint_id,
        ticket,
    })
}

/// `rsntr stop <dir>`: SIGTERM the recorded daemon, wait for it to die,
/// escalate to SIGKILL after the graceful window. Idempotent.
pub async fn stop(dir: &Path) -> Result<StopReport> {
    let Some(pid) = read_live_pid(dir) else {
        // No live recorded pid. A live socket without one means a
        // foreground serve from before pid files, or someone else's.
        if socket_alive(dir).await {
            bail!(
                "a node is serving {} but {} names no live process; stop it by hand",
                dir.display(),
                pid_path(dir).display()
            );
        }
        // Sweep leftovers from a crash.
        let _ = std::fs::remove_file(pid_path(dir));
        let sock = owner_socket::socket_path(dir);
        if sock.exists() {
            let _ = std::fs::remove_file(&sock);
        }
        return Ok(StopReport {
            stopped: false,
            already_stopped: true,
            pid: None,
            forced: false,
        });
    };

    unsafe { libc::kill(pid, libc::SIGTERM) };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut forced = false;
    loop {
        if !pid_alive(pid) {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            forced = true;
            // Give the kernel a beat, then stop waiting either way.
            tokio::time::sleep(Duration::from_millis(200)).await;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Graceful shutdown removes these itself; a forced kill cannot.
    if forced {
        let _ = std::fs::remove_file(pid_path(dir));
        let _ = std::fs::remove_file(owner_socket::socket_path(dir));
    }
    Ok(StopReport {
        stopped: true,
        already_stopped: false,
        pid: Some(pid),
        forced,
    })
}

/// `rsntr status <dir>`: one cheap probe an agent can run at session
/// start. Works with or without a daemon.
pub async fn status(dir: &Path) -> Result<StatusReport> {
    let endpoint_id = store::node_id(dir)?.to_string();
    let serving = socket_alive(dir).await;
    let conn = store::open_db(dir)?;
    let addrs = if serving {
        resonator_node::get_rsntr(&conn, "serving_addrs")
            .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let ticket = if serving {
        resonator_node::get_rsntr(&conn, SERVING_TICKET_KEY).filter(|t| !t.is_empty())
    } else {
        None
    };
    let peers: i64 = conn.query_row("SELECT count(*) FROM _peers", [], |r| r.get(0))?;
    let pending_inbox: i64 = conn.query_row(
        "SELECT count(*) FROM _inbox WHERE decision IS NULL",
        [],
        |r| r.get(0),
    )?;
    Ok(StatusReport {
        serving,
        pid: if serving { read_live_pid(dir) } else { None },
        endpoint_id,
        addrs,
        ticket,
        peers,
        pending_inbox,
    })
}
