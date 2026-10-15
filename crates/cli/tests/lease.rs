//! M2: the serving lease. One process serves a directory; the holder is
//! named; the kernel releases the lease on any exit; `rsntr status`,
//! `rsntr serve`, and `rsntr stop` all read it.

#![cfg(unix)]

use std::process::Command;
use std::time::{Duration, Instant};

use rsntr::testutil::TempDir;
use rsntr::{AppInfo, LeaseError, ServeLease, lease, serve, store};

struct DaemonGuard(Option<i32>);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

fn wait_until(what: &str, dur: Duration, f: impl Fn() -> bool) {
    let deadline = Instant::now() + dur;
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn run(args: &[&str], dir: &std::path::Path) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(args)
        .arg(dir)
        .output()
        .expect("run rsntr");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn second_start_in_process_reports_the_holder() {
    let tmp = TempDir::new("lease-a");
    store::init_dir(tmp.path()).expect("init");
    let a = serve::start_node(tmp.path(), true).await.expect("serve a");
    assert_eq!(a.lease_info().app.id, "network.resonator.rsntr");
    assert!(lease::is_held(tmp.path()));

    let err = serve::start_node(tmp.path(), true)
        .await
        .err()
        .expect("the second start is refused");
    match err.downcast_ref::<LeaseError>() {
        Some(LeaseError::AlreadyServing { holder, .. }) => {
            let h = holder.as_ref().expect("holder record");
            assert_eq!(h.pid, std::process::id());
            assert_eq!(h.app.id, "network.resonator.rsntr");
        }
        other => panic!("expected AlreadyServing, got {other:?} ({err:#})"),
    }
    assert!(err.to_string().contains("already served by"), "{err}");

    a.shutdown().await;
    assert!(!lease::is_held(tmp.path()), "released by shutdown");
    assert!(
        lease::lease_path(tmp.path()).exists(),
        "the lock file stays"
    );
    let b = serve::start_node(tmp.path(), true)
        .await
        .expect("serve again");
    b.shutdown().await;
}

#[test]
fn binary_serve_status_and_stop_honour_a_foreign_holder() {
    let tmp = TempDir::new("lease-b");
    store::init_dir(tmp.path()).expect("init");
    // An embedding app holds the lease without any control socket.
    let held = ServeLease::acquire(tmp.path(), &AppInfo::new("other.app")).expect("hold");

    let (code, _out, err) = run(&["serve", "--foreground", "--offline"], tmp.path());
    assert_ne!(code, 0, "foreground serve must refuse a held lease");
    assert!(
        err.contains("already served by") && err.contains("other.app"),
        "{err}"
    );

    let (code, out, _) = run(&["--json", "status"], tmp.path());
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["serving"], true, "{v}");
    assert_eq!(v["lease"], true);
    assert_eq!(v["app"], "other.app");
    assert_eq!(v["pid"], std::process::id());

    let (code, out, _) = run(&["--json", "serve"], tmp.path());
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["already_running"], true, "{v}");
    assert_eq!(v["app"], "other.app");
    assert!(
        !tmp.path().join("rsntr.pid").exists(),
        "no daemon was spawned"
    );

    let (code, _out, err) = run(&["stop"], tmp.path());
    assert_eq!(code, 1, "stop refuses a foreign holder: {err}");
    assert!(err.contains("other.app"), "{err}");

    drop(held);
    let (code, out, _) = run(&["--json", "status"], tmp.path());
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["serving"], false, "{v}");
    assert_eq!(v["lease"], false);
}

#[test]
fn lease_is_released_when_the_server_is_killed() {
    let tmp = TempDir::new("lease-c");
    store::init_dir(tmp.path()).expect("init");
    let mut child = Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(["serve", "--foreground", "--offline"])
        .arg(tmp.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn serve");
    let guard = DaemonGuard(Some(child.id() as i32));
    wait_until("the lease", Duration::from_secs(15), || {
        lease::is_held(tmp.path())
    });
    let holder = lease::holder(tmp.path()).expect("held");
    assert_eq!(holder.pid, child.id());
    assert_eq!(holder.app.id, "network.resonator.rsntr");

    unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
    drop(guard);
    let _ = child.wait();
    wait_until(
        "the kernel to release the lease",
        Duration::from_secs(3),
        || !lease::is_held(tmp.path()),
    );
    // The stale record names a dead pid; the lock says nobody serves.
    let again = ServeLease::acquire(tmp.path(), &AppInfo::new("next.app")).expect("re-acquire");
    assert_eq!(again.info().app.id, "next.app");
}
