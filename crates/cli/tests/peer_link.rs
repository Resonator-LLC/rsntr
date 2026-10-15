//! How a client command reaches a peer once the directory is served:
//! the dial is made by the serving process, not this one (one identity,
//! one endpoint - docs/relay-identity-sharing.md). These live in their
//! own test binary rather than beside the addressing tests: they stand
//! up extra endpoints in the process, and the address a live connection
//! reports is exactly what `refresh_merges_live_addrs_or_names_the_hint`
//! asserts on.

use std::time::Duration;

use rsntr::testutil::TempDir;
use rsntr::{serve, store};

/// `--offline` asks THIS process to bind an endpoint with no relay and
/// no address lookup. Once a directory is served, every peer command
/// goes out through the serving process instead (one identity, one
/// endpoint - docs/relay-identity-sharing.md), and the flag has nothing
/// left to govern: it is dropped. Silently, until now, which left the
/// caller who passed it believing the request stayed off every relay -
/// and since a node is normally served, that was the usual case.
#[tokio::test(flavor = "multi_thread")]
async fn offline_says_so_when_the_serving_process_carries_the_dial() {
    let ta = TempDir::new("offn-a");
    let tb = TempDir::new("offn-b");
    let tc = TempDir::new("offn-c");
    let a_id = store::init_dir(ta.path()).expect("init a");
    store::init_dir(tb.path()).expect("init b");
    store::init_dir(tc.path()).expect("init c");

    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    let ticket = b.ready_ticket(Duration::from_secs(3)).await;
    store::peer_add(ta.path(), "b", &ticket, &[]).expect("a learns b");
    store::peer_add(tc.path(), "b", &ticket, &[]).expect("c learns b");
    b.node()
        .db()
        .call({
            let a_hex = a_id.to_string();
            move |conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO _peers (endpoint_id, added_at) VALUES (?1, datetime('now'))",
                    [&a_hex],
                )
                .expect("admit a");
            }
        })
        .await
        .expect("db");

    // A is served, so its client commands ride its serving process.
    let a = serve::start_node(ta.path(), true).await.expect("serve a");

    let help = |dir: &std::path::Path, no_proxy: bool| {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"));
        cmd.args(["help", "b", "--offline", "-d"]).arg(dir);
        if no_proxy {
            cmd.env("RSNTR_NO_PEER_PROXY", "1");
        }
        let out = cmd.output().expect("run help");
        String::from_utf8_lossy(&out.stderr).to_string()
    };

    let served = help(ta.path(), false);
    assert!(
        served.contains("--offline is not applied"),
        "the served directory must say the flag was dropped: {served}"
    );
    assert!(
        served.contains("RSNTR_NO_PEER_PROXY"),
        "and name the way to dial from here instead: {served}"
    );

    // Two controls, so the absence above would have meant something.
    // The same command with the proxy forced off honours the flag.
    let direct = help(ta.path(), true);
    assert!(
        !direct.contains("--offline is not applied"),
        "no notice when this process does the dialling: {direct}"
    );
    // And a directory nobody serves never proxies in the first place.
    let unserved = help(tc.path(), false);
    assert!(
        !unserved.contains("--offline is not applied"),
        "no notice for an unserved directory: {unserved}"
    );

    a.shutdown().await;
    b.shutdown().await;
}

/// "Every dial error ends with the same hint" is a promise the manual
/// makes, and it held only on the branch that stopped being the normal
/// one.
///
/// A peer command run from a served directory is dialled by the serving
/// process, and its failure came back as the bare wire error
/// ("connection-error: connecting to alice: dial failed: timed out").
/// True, and the end of the road for the reader: no refresh, no ticket,
/// no knock. The direct branch had said all three all along, so the two
/// paths answered the same failure differently and the common one said
/// less.
#[tokio::test(flavor = "multi_thread")]
async fn a_dial_the_daemon_made_still_names_the_remedy() {
    let ta = TempDir::new("hint-a");
    let tc = TempDir::new("hint-c");
    let tgone = TempDir::new("hint-gone");
    store::init_dir(ta.path()).expect("init a");
    store::init_dir(tc.path()).expect("init c");
    let gone = store::init_dir(tgone.path()).expect("init gone");

    // A peer that has never served: nothing will answer either path.
    store::peer_add(ta.path(), "gone", &gone.to_string(), &[]).expect("a names gone");
    store::peer_add(tc.path(), "gone", &gone.to_string(), &[]).expect("c names gone");

    let a = serve::start_node(ta.path(), true).await.expect("serve a");

    let query = |dir: &std::path::Path| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
            .args(["query", "gone", "SELECT 1", "--offline", "-d"])
            .arg(dir)
            .output()
            .expect("run query");
        String::from_utf8_lossy(&out.stderr).to_string()
    };

    let hint = "the peer may be down or its stored addresses stale";
    let served = query(ta.path());
    assert!(
        served.contains(hint),
        "a proxied dial failure names the remedy: {served}"
    );
    assert!(
        served.contains("peer refresh"),
        "including what to run: {served}"
    );
    // The control: the direct branch, which always said it. Both paths
    // must answer the same failure the same way.
    let direct = query(tc.path());
    assert!(
        direct.contains(hint),
        "the direct branch still does: {direct}"
    );

    a.shutdown().await;
}
