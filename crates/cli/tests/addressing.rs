//! M3 addressing: a serving node publishes `serving_ticket` and
//! `external_addrs` (offline: relay false) and `rsntr status --json`
//! carries them; `peer refresh` merges the live address of a reachable
//! peer and names the dial hint for an unreachable one.

use std::time::Duration;

use rsntr::testutil::TempDir;
use rsntr::{client, serve, store};

#[tokio::test(flavor = "multi_thread")]
async fn serving_publishes_ticket_and_external_addrs() {
    let ta = TempDir::new("addr-a");
    store::init_dir(ta.path()).expect("init a");
    let a = serve::start_node(ta.path(), true).await.expect("serve a");

    let conn = store::open_db(ta.path()).expect("open");
    let ticket = resonator_node::get_rsntr(&conn, rsntr::detach_keys::SERVING_TICKET_KEY)
        .expect("serving_ticket published at start");
    assert_eq!(ticket, a.ticket());
    let ext: serde_json::Value = serde_json::from_str(
        &resonator_node::get_rsntr(&conn, serve::EXTERNAL_ADDRS_KEY).expect("external_addrs"),
    )
    .expect("json");
    assert_eq!(ext["relay"], false, "offline binds have no relay: {ext}");
    assert!(ext["addrs"].is_array());
    drop(conn);

    // `rsntr status --json` against the in-process node (its control
    // socket answers) carries the new fields.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(["--json", "status"])
        .arg(ta.path())
        .output()
        .expect("run status");
    let v: serde_json::Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
        .unwrap_or_else(|e| {
            panic!(
                "status json: {e}; stderr: {}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
    assert_eq!(v["serving"], true, "{v}");
    assert_eq!(v["relay"], false);
    assert!(v["external_addrs"].is_array());
    assert_eq!(v["ticket"].as_str(), Some(a.ticket().as_str()));

    a.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn refresh_merges_live_addrs_or_names_the_hint() {
    let ta = TempDir::new("rf-a");
    let tb = TempDir::new("rf-b");
    let tc = TempDir::new("rf-c");
    store::init_dir(ta.path()).expect("init a");
    let b_id = store::init_dir(tb.path()).expect("init b");
    let c_id = store::init_dir(tc.path()).expect("init c");

    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    // A admits B by id (no addrs needed: B dials A).
    store::peer_add(ta.path(), "b", &b_id.to_string(), &[]).expect("a admits b");
    // B knows A's real address plus a stale one first in line.
    let real = a.direct_addrs()[0];
    let stale: std::net::SocketAddr = "127.0.0.1:1".parse().unwrap();
    store::peer_add(tb.path(), "a", &a.peer_id().to_string(), &[stale, real]).expect("b names a");
    // B knows C only by a dead address.
    store::peer_add(tb.path(), "c", &c_id.to_string(), &[stale]).expect("b names c");

    let r = client::refresh_peer(tb.path(), "a", true)
        .await
        .expect("a is reachable");
    assert_eq!(r.endpoint_id, a.peer_id().to_string());
    assert!(r.observed.contains(&real), "observed {:?}", r.observed);
    assert_eq!(
        r.after.first(),
        Some(&real),
        "fresh address first: {:?}",
        r.after
    );

    let started = std::time::Instant::now();
    let err = client::refresh_peer(tb.path(), "c", true)
        .await
        .expect_err("c is down");
    let text = err.to_string();
    assert!(
        text.contains("unreachable at all 1 known address"),
        "{text}"
    );
    assert!(text.contains("rsntr peer refresh"), "{text}");
    assert!(
        // Well under iroh's own dial timeout even on a loaded test host.
        started.elapsed() < client::REFRESH_DIAL_TIMEOUT + Duration::from_secs(12),
        "bounded"
    );

    // The CLI surface: exit 1 with the hint on stderr.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(["peer", "refresh", "c", "--offline", "-d"])
        .arg(tb.path())
        .output()
        .expect("run refresh");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unreachable"));

    a.shutdown().await;
}
