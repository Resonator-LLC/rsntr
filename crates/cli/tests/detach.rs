//! Daemonized serve lifecycle over the real binary: detach by default
//! with auto-init, idempotent re-serve, status, graceful stop, and
//! idempotent re-stop.

#![cfg(unix)]

use std::process::Command;
use std::time::{Duration, Instant};

use rsntr::testutil::TempDir;

/// Kills a spawned daemon on drop, so a failing assert never leaks one.
struct DaemonGuard(Option<i32>);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

fn rsntr(args: &[&str]) -> (i32, serde_json::Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .arg("--json")
        .args(args)
        .output()
        .expect("running rsntr");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "rsntr {args:?} did not print one JSON object ({e}); stdout: {stdout:?}, stderr: {:?}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out.status.code().unwrap_or(-1), json)
}

fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn wait_until(what: &str, dur: Duration, f: impl Fn() -> bool) {
    let deadline = Instant::now() + dur;
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn detached_serve_lifecycle() {
    let tmp = TempDir::new("detach");
    let dir = tmp.path().to_str().expect("utf8 dir");

    // Detach is the default; a fresh directory auto-inits.
    let (code, first) = rsntr(&["serve", dir, "--offline"]);
    assert_eq!(code, 0, "serve failed: {first}");
    let pid = first["pid"].as_i64().expect("pid") as i32;
    let mut guard = DaemonGuard(Some(pid));
    assert_eq!(first["ok"], true);
    assert_eq!(first["already_running"], false);
    assert!(first["endpoint_id"].as_str().unwrap().len() == 64);
    assert!(
        first["ticket"].as_str().is_some(),
        "the detach parent reads the published ticket: {first}"
    );
    assert!(tmp.path().join("rsntr.db").exists(), "auto-init");
    assert!(tmp.path().join("rsntr.key").exists(), "auto-init key");
    assert!(tmp.path().join("rsntr.sock").exists(), "control socket");
    assert!(tmp.path().join("rsntr.pid").exists(), "pid file");
    assert!(tmp.path().join("rsntr.log").exists(), "daemon log");
    assert!(pid_alive(pid), "daemon survives the CLI exiting");

    // Idempotent: a second serve reports the running daemon.
    let (code, second) = rsntr(&["serve", dir, "--offline"]);
    assert_eq!(code, 0);
    assert_eq!(second["already_running"], true);
    assert_eq!(second["pid"].as_i64(), Some(pid as i64));

    // Status sees it.
    let (code, status) = rsntr(&["status", dir]);
    assert_eq!(code, 0);
    assert_eq!(status["serving"], true);
    assert_eq!(status["pid"].as_i64(), Some(pid as i64));
    assert_eq!(status["endpoint_id"], first["endpoint_id"]);

    // Graceful stop: process gone, socket and pid file removed.
    let (code, stop) = rsntr(&["stop", dir]);
    assert_eq!(code, 0, "stop failed: {stop}");
    assert_eq!(stop["stopped"], true);
    assert_eq!(stop["forced"], false, "SIGTERM must be enough: {stop}");
    wait_until("daemon exit", Duration::from_secs(10), || !pid_alive(pid));
    guard.0 = None;
    wait_until("socket removal", Duration::from_secs(5), || {
        !tmp.path().join("rsntr.sock").exists()
    });
    wait_until("pid file removal", Duration::from_secs(5), || {
        !tmp.path().join("rsntr.pid").exists()
    });

    // Idempotent again: nothing left to stop.
    let (code, again) = rsntr(&["stop", dir]);
    assert_eq!(code, 0);
    assert_eq!(again["already_stopped"], true);

    let (code, status) = rsntr(&["status", dir]);
    assert_eq!(code, 0);
    assert_eq!(status["serving"], false);
}

fn dir_of(tmp: &TempDir) -> &str {
    tmp.path().to_str().expect("utf8 dir")
}

#[test]
fn status_counts_peers_and_inbox() {
    let tmp = TempDir::new("status-counts");
    // No daemon: init through the binary, then count via status.
    let (code, init) = rsntr(&["init", dir_of(&tmp)]);
    assert_eq!(code, 0, "init failed: {init}");
    {
        let conn = rsntr::store::open_db(tmp.path()).expect("open");
        conn.execute(
            "INSERT INTO _peers (endpoint_id, added_at) VALUES (?1, datetime('now'))",
            [&"ab".repeat(32)],
        )
        .expect("peer");
        conn.execute(
            "INSERT INTO _inbox (request_id, peer, sql, params, received_at) \
             VALUES ('01TEST', ?1, '', 'hello?', datetime('now'))",
            [&"ab".repeat(32)],
        )
        .expect("inbox row");
    }
    let (code, status) = rsntr(&["status", dir_of(&tmp)]);
    assert_eq!(code, 0);
    assert_eq!(status["serving"], false);
    assert_eq!(status["peers"].as_i64(), Some(1));
    assert_eq!(status["pending_inbox"].as_i64(), Some(1));
}
