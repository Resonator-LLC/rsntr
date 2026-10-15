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

use crate::{lease, owner_socket, store};

/// Pid file name inside a node directory.
pub const PID_FILE: &str = "rsntr.pid";
/// Daemon log file name inside a node directory.
pub const LOG_FILE: &str = "rsntr.log";
/// The `_rsntr` key holding the live ticket while a node serves.
pub use crate::detach_keys::SERVING_TICKET_KEY;

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
    /// The app holding the serving lease when one already served (an
    /// embedding app, or `network.resonator.rsntr` for a daemon).
    pub app: Option<String>,
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
    /// The loopback-rewritten listen sockets (`serving_addrs`): for
    /// self-dials on this host, not for peers.
    pub addrs: Vec<String>,
    /// The non-loopback addresses the ticket advertises
    /// (`external_addrs`), refreshed on a cadence while serving.
    pub external_addrs: Vec<String>,
    /// The ticket carries a home relay (NAT'd dialers can reach it).
    ///
    /// Reachability, not routing. This is a property of this node's own
    /// address set and is true for nearly every online node; it says
    /// nothing about whether any particular connection is relayed or
    /// direct. For that, sample the live connection: `rsntr media`
    /// prints its path, and `IrohTransport::peer_path` returns it.
    pub relay: bool,
    pub ticket: Option<String>,
    pub peers: i64,
    pub pending_inbox: i64,
    /// The app holding the serving lease (`rsntr.serve.json`), when held.
    pub app: Option<String>,
    /// True while some process holds the serving lease.
    pub lease: bool,
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
    crate::owner::probe(dir).await
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
            app: lease::holder(dir).map(|h| h.app.id),
        });
    }
    // An app serving without a control socket (or before it bound one)
    // still holds the lease: nothing to spawn.
    if let Some(holder) = lease::holder(dir) {
        return Ok(DetachReport {
            already_running: true,
            pid: read_live_pid(dir).or(Some(holder.pid as i32)),
            socket,
            endpoint_id,
            ticket: read_rsntr(dir, SERVING_TICKET_KEY),
            app: Some(holder.app.id),
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
        app: None,
    })
}

/// `rsntr stop <dir>`: SIGTERM the recorded daemon, wait for it to die,
/// escalate to SIGKILL after the graceful window. Idempotent.
pub async fn stop(dir: &Path) -> Result<StopReport> {
    let Some(pid) = read_live_pid(dir) else {
        // No live recorded pid. A held lease means an embedding app
        // serves: not ours to signal.
        if let Some(holder) = lease::holder(dir) {
            bail!(
                "{} is served by {} (pid {}), not by an rsntr daemon this command can stop; \
                 stop it from that app",
                dir.display(),
                holder.app.name.as_deref().unwrap_or(&holder.app.id),
                holder.pid
            );
        }
        // A live socket without one means a foreground serve from
        // before pid files, or someone else's.
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
        let _ = std::fs::remove_file(crate::owner::owner_path(dir));
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
        let _ = std::fs::remove_file(crate::owner::owner_path(dir));
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
    let holder = lease::holder(dir);
    let serving = socket_alive(dir).await || holder.is_some();
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
    let (external_addrs, relay) = if serving {
        resonator_node::get_rsntr(&conn, crate::serve::EXTERNAL_ADDRS_KEY)
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .map(|v| {
                (
                    v["addrs"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                    v["relay"].as_bool().unwrap_or(false),
                )
            })
            .unwrap_or_default()
    } else {
        (Vec::new(), false)
    };
    let peers: i64 = conn.query_row("SELECT count(*) FROM _peers", [], |r| r.get(0))?;
    let pending_inbox: i64 = conn.query_row(
        "SELECT count(*) FROM _inbox WHERE decision IS NULL",
        [],
        |r| r.get(0),
    )?;
    Ok(StatusReport {
        serving,
        pid: if serving {
            read_live_pid(dir).or(holder.as_ref().map(|h| h.pid as i32))
        } else {
            None
        },
        app: holder.as_ref().map(|h| h.app.id.clone()),
        lease: holder.is_some(),
        endpoint_id,
        addrs,
        external_addrs,
        relay,
        ticket,
        peers,
        pending_inbox,
    })
}

// ---------------------------------------------------------------------
// `rsntr install-service`: a systemd unit, print-only
// ---------------------------------------------------------------------

/// What `rsntr install-service` reports: the unit text plus the recipe
/// to put it in place. Nothing is written by the command itself.
#[derive(Debug, Clone)]
pub struct ServiceUnit {
    /// `rsntr-<dir name>`.
    pub unit_name: String,
    /// Where the unit belongs (`~/.config/systemd/user/...` with
    /// `--user`, `/etc/systemd/system/...` otherwise).
    pub unit_path: PathBuf,
    pub unit: String,
    /// The shell steps that install and start it, in order.
    pub recipe: Vec<String>,
}

/// The unit text: `ExecStart=<exe> serve <dir> --foreground` (attached,
/// so systemd owns the process), restart on failure, and the target
/// matching `user`. Pure, for tests.
pub fn render_systemd_unit(exe: &Path, dir: &Path, user: bool) -> String {
    render_systemd_unit_as(exe, dir, user, &current_account())
}

/// True when `dir` sits under the invoking user's home directory.
///
/// A node there is that person's, and a system unit is the wrong shape
/// for it even with `User=`: it is placed with sudo into a root-owned
/// path, started before the session exists, and on a machine where every
/// other node is a user unit it is the one nobody actually installs. So
/// a home-directory node defaults to the per-user form and `--system`
/// asks for the other one explicitly.
fn under_home(dir: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return false;
    };
    // Compare resolved paths where possible so a symlinked home still
    // matches; fall back to the literal prefix when it cannot resolve.
    let resolved = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    resolved(dir).starts_with(resolved(&home))
}

/// The account a system unit runs as: whoever invoked the command, since
/// the node directory (and `rsntr.key` in it) belongs to them.
fn current_account() -> String {
    std::env::var("SUDO_USER")
        .or_else(|_| std::env::var("USER"))
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "root".to_string())
}

/// [`render_systemd_unit`] with an explicit account, for tests.
pub fn render_systemd_unit_as(exe: &Path, dir: &Path, user: bool, account: &str) -> String {
    let wanted_by = if user {
        "default.target"
    } else {
        "multi-user.target"
    };
    // A system unit runs as root unless told otherwise, and a node run by
    // root leaves root-owned files (a WAL, a lease, blobs) beside a key
    // its owner must keep reading: name the account explicitly. A user
    // unit already runs as its user, so naming it there would be wrong.
    let account_lines = if user {
        String::new()
    } else {
        format!("User={account}\nGroup={account}\n")
    };
    format!(
        "[Unit]\n\
         Description=rsntr node {dir}\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         {account_lines}\
         ExecStart={exe} serve {dir} --foreground\n\
         WorkingDirectory={dir}\n\
         Restart=always\n\
         RestartSec=5\n\
         KillSignal=SIGTERM\n\
         TimeoutStopSec=15\n\
         \n\
         [Install]\n\
         WantedBy={wanted_by}\n",
        exe = exe.display(),
        dir = dir.display(),
    )
}

/// A unit name from the directory name: `rsntr-<name>` with anything
/// systemd would reject replaced by `-`.
fn unit_name_for(dir: &Path) -> String {
    let raw = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "node".to_string());
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("rsntr-{cleaned}")
}

/// `rsntr install-service <dir> [--user]`: renders the unit for this
/// binary and the absolute node directory, and the recipe to install it
/// (`loginctl enable-linger` included for `--user`, so the node keeps
/// serving without a login session). Print-only by design: the owner
/// reviews and places it.
pub fn install_service(dir: &Path, user: Option<bool>) -> Result<ServiceUnit> {
    let dir = std::path::absolute(dir).with_context(|| format!("resolving {}", dir.display()))?;
    let exe = std::env::current_exe().context("locating the rsntr binary")?;
    let unit_name = unit_name_for(&dir);
    let user = user.unwrap_or_else(|| under_home(&dir));
    let unit = render_systemd_unit(&exe, &dir, user);
    let (unit_path, recipe) = if user {
        let path = PathBuf::from(format!("~/.config/systemd/user/{unit_name}.service"));
        (
            path.clone(),
            vec![
                "mkdir -p ~/.config/systemd/user".to_string(),
                format!(
                    "rsntr install-service {} --user > {}",
                    dir.display(),
                    path.display()
                ),
                "systemctl --user daemon-reload".to_string(),
                format!("systemctl --user enable --now {unit_name}"),
                "loginctl enable-linger \"$USER\"   # keep it running without a login session"
                    .to_string(),
                format!("systemctl --user status {unit_name}"),
            ],
        )
    } else {
        let path = PathBuf::from(format!("/etc/systemd/system/{unit_name}.service"));
        (
            path.clone(),
            vec![
                format!(
                    "rsntr install-service {} | sudo tee {} > /dev/null",
                    dir.display(),
                    path.display()
                ),
                "sudo systemctl daemon-reload".to_string(),
                format!("sudo systemctl enable --now {unit_name}"),
                format!("systemctl status {unit_name}"),
            ],
        )
    };
    Ok(ServiceUnit {
        unit_name,
        unit_path,
        unit,
        recipe,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_text_targets_user_or_system() {
        let exe = Path::new("/usr/local/bin/rsntr");
        let dir = Path::new("/home/agent/node");
        let user = render_systemd_unit_as(exe, dir, true, "agent");
        assert!(
            user.contains("ExecStart=/usr/local/bin/rsntr serve /home/agent/node --foreground")
        );
        assert!(user.contains("WantedBy=default.target"));
        // A node that exits cleanly (a crash caught as a clean exit, a
        // stray `rsntr stop`) must come back; `systemctl stop` still stops.
        assert!(user.contains("Restart=always"));
        // A user unit already runs as its user.
        assert!(!user.contains("User="));
        let system = render_systemd_unit_as(exe, dir, false, "agent");
        assert!(system.contains("WantedBy=multi-user.target"));
        assert!(!system.contains("default.target"));
        assert!(system.contains("Restart=always"));
        // A system unit defaults to root, which would leave root-owned
        // files beside the node key.
        assert!(system.contains("User=agent"), "{system}");
        assert!(system.contains("Group=agent"));
    }

    #[test]
    fn unit_names_are_systemd_safe() {
        assert_eq!(
            unit_name_for(Path::new("/x/agent node!")),
            "rsntr-agent-node-"
        );
        assert_eq!(unit_name_for(Path::new("/x/agent_1.0")), "rsntr-agent_1.0");
    }
}

#[cfg(test)]
mod unit_choice_tests {
    use super::*;

    /// Every node on a developer machine lives under a home directory
    /// and is served by a user unit. The command used to print a system
    /// unit for those, so following it verbatim placed a sudo-installed
    /// unit beside a user-owned key: the wrong shape even once `User=`
    /// stopped it running as root.
    #[test]
    fn a_home_directory_node_defaults_to_a_user_unit() {
        let home = std::env::var("HOME").expect("HOME");
        let dir = PathBuf::from(&home).join(".rsntr/example");
        let unit = install_service(&dir, None).expect("rendering");
        assert!(
            unit.unit.contains("WantedBy=default.target"),
            "user unit: {}",
            unit.unit
        );
        assert!(
            !unit.unit.contains("User="),
            "a user unit names no account: {}",
            unit.unit
        );
        assert!(
            unit.unit_path.starts_with("~/.config/systemd/user"),
            "installed under the user's own systemd: {}",
            unit.unit_path.display()
        );
    }

    /// Outside a home there is no user session to hang it on, so the
    /// system form stays the default there.
    #[test]
    fn a_node_outside_home_defaults_to_a_system_unit() {
        let unit = install_service(Path::new("/srv/rsntr/node"), None).expect("rendering");
        assert!(
            unit.unit.contains("WantedBy=multi-user.target"),
            "system unit: {}",
            unit.unit
        );
        assert!(unit.unit.contains("User="), "names an account");
    }

    /// Both directions stay forceable, because the default is a guess
    /// about intent and the caller may know better.
    #[test]
    fn either_form_can_be_forced() {
        let home = std::env::var("HOME").expect("HOME");
        let in_home = PathBuf::from(&home).join(".rsntr/example");
        let forced_system = install_service(&in_home, Some(false)).expect("rendering");
        assert!(forced_system.unit.contains("WantedBy=multi-user.target"));
        let forced_user = install_service(Path::new("/srv/rsntr/node"), Some(true)).expect("r");
        assert!(forced_user.unit.contains("WantedBy=default.target"));
    }
}
