//! The zero-toolchain page host: publish HTML, fetch it as a hologram
//! over the transport (byte-identical, verified against the header
//! size), gate on the caller's read policy, and chunk large pages.

use std::time::Duration;

use rsntr::client::{self, PagesOutcome};
use rsntr::pagecmd;
use rsntr::testutil::TempDir;
use rsntr::{Prefer, serve, store};

async fn admit(node: &serve::RunningNode, peer_hex: &str) {
    let peer = peer_hex.to_string();
    node.node()
        .db()
        .call(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO _peers (endpoint_id, added_at) VALUES (?1, datetime('now'))",
                [&peer],
            )
            .expect("admit");
        })
        .await
        .expect("db call");
}

async fn grant(node: &serve::RunningNode, peer_hex: &str, table: &str, action: &str) {
    let (peer, table, action) = (peer_hex.to_string(), table.to_string(), action.to_string());
    node.node()
        .db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
                 VALUES (?1, ?2, ?3, 'allow')",
                [&peer, &table, &action],
            )
            .expect("grant");
        })
        .await
        .expect("db call");
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_fetch_gate_and_chunk() {
    let ta = TempDir::new("pg-a");
    let tb = TempDir::new("pg-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    store::init_dir(tb.path()).expect("init b");
    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    store::peer_add(
        ta.path(),
        "b",
        &b.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("a learns b");
    let a_hex = a_id.to_string();
    admit(&b, &a_hex).await;

    // Publish a small page locally on B.
    let html = "<!doctype html><title>hi</title><h1>hello from B</h1>";
    pagecmd::page_publish(
        tb.path(),
        Prefer::Auto,
        "home",
        html,
        "text/html; charset=utf-8",
        None,
        false,
    )
    .await
    .expect("publish");

    // No read grant yet: a remote fetch is denied.
    let denied = client::run_pages_fetch(ta.path(), "b", "home", true)
        .await
        .expect("fetch completes");
    assert!(
        matches!(denied, PagesOutcome::Denied(_)),
        "expected denial, got {denied:?}"
    );

    // Grant read on `pages`: the fetch reassembles byte-identical HTML.
    grant(&b, &a_hex, "pages", "read").await;
    match client::run_pages_fetch(ta.path(), "b", "home", true)
        .await
        .expect("fetch")
    {
        PagesOutcome::Page {
            content_type,
            bytes,
        } => {
            assert_eq!(content_type, "text/html; charset=utf-8");
            assert_eq!(bytes, html.as_bytes());
        }
        other => panic!("expected the page, got {other:?}"),
    }

    // A missing page is point-unknown.
    match client::run_pages_fetch(ta.path(), "b", "nope", true)
        .await
        .expect("fetch")
    {
        PagesOutcome::Failed(e) => assert_eq!(e.code, "point-unknown"),
        other => panic!("expected point-unknown, got {other:?}"),
    }

    // Large HTML (> the 256 KiB frame, so > one chunk): chunked publish,
    // then a multi-Chunk hologram fetch verified against its size.
    let big = format!("<!doctype html><body>{}</body>", "x".repeat(400 * 1024));
    let report = pagecmd::page_publish(
        tb.path(),
        Prefer::Auto,
        "big",
        &big,
        "text/html",
        None,
        false,
    )
    .await
    .expect("publish big");
    let pagecmd::PublishOutcome::Published { chunks, .. } = report else {
        panic!("big publish denied: {report:?}");
    };
    assert!(chunks > 1, "big publish chunks: {chunks}");
    match client::run_pages_fetch(ta.path(), "b", "big", true)
        .await
        .expect("fetch big")
    {
        PagesOutcome::Page { bytes, .. } => assert_eq!(bytes, big.as_bytes()),
        other => panic!("expected the big page, got {other:?}"),
    }

    a.shutdown().await;
    b.shutdown().await;
}

/// Removing a page that is not there is not removing a page.
///
/// `page rm` reported `"removed": true` and exit 0 whatever the delete
/// matched, because the statement had run and nothing had refused it.
/// That is the answer to a different question, and the reading it
/// invites - "the page is gone" - is the one a person reaches for this
/// command to confirm. On `--peer` it is worse: a typo in the name and
/// a page still being served look identical from here.
#[tokio::test(flavor = "multi_thread")]
async fn removing_a_page_that_is_not_there_says_so() {
    let ta = TempDir::new("pgrm-a");
    let tb = TempDir::new("pgrm-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    store::init_dir(tb.path()).expect("init b");
    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    store::peer_add(
        ta.path(),
        "b",
        &b.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("a learns b");
    let a_hex = a_id.to_string();
    admit(&b, &a_hex).await;
    grant(&b, &a_hex, "pages", "write").await;

    // Local, through the library: nothing matched.
    let matched = pagecmd::page_rm(tb.path(), Prefer::Auto, "never-published", None, true)
        .await
        .expect("rm completes")
        .expect("not denied");
    assert_eq!(matched, Some(0), "the delete matched no page");

    // And on a peer, where the reader has no other window on that node.
    let matched = pagecmd::page_rm(ta.path(), Prefer::Auto, "never-published", Some("b"), true)
        .await
        .expect("remote rm completes")
        .expect("not denied");
    assert_eq!(matched, Some(0), "the remote delete matched no page");

    // The CLI turns that into the same refusal every sibling rm gives.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(["--json", "page", "rm", "never-published", "-d"])
        .arg(tb.path())
        .output()
        .expect("run page rm");
    assert_eq!(
        out.status.code(),
        Some(1),
        "exit 1 like `hook rm` on a miss"
    );
    let v: serde_json::Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
        .unwrap_or_else(|e| panic!("json: {e}; stderr {}", String::from_utf8_lossy(&out.stderr)));
    assert_eq!(v["ok"], false, "{v}");
    assert!(
        v["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("never-published"),
        "names the page it could not find: {v}"
    );

    // A page that IS there still removes, and still says so.
    pagecmd::page_publish(
        tb.path(),
        Prefer::Auto,
        "real",
        "<p>x</p>",
        "text/html",
        None,
        false,
    )
    .await
    .expect("publish");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(["--json", "page", "rm", "real", "-d"])
        .arg(tb.path())
        .output()
        .expect("run page rm");
    assert_eq!(out.status.code(), Some(0), "a real removal is exit 0");
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json");
    assert_eq!(v["removed"], true, "{v}");

    a.shutdown().await;
    b.shutdown().await;
}
