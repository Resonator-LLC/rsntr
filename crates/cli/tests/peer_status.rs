//! `rsntr peer list` / `peer status`: presence evidence (last beacon age
//! or "never"), undelivered outbox counts keyed by hex or petname, and
//! the bounded --probe reachability answer.

use std::time::Duration;

use rsntr::testutil::TempDir;
use rsntr::{chat, serve, store};

#[tokio::test(flavor = "multi_thread")]
async fn list_and_status_report_evidence_and_pending() {
    let ta = TempDir::new("ps-a");
    let tb = TempDir::new("ps-b");
    let tc = TempDir::new("ps-c");
    let a_id = store::init_dir(ta.path()).expect("init a");
    store::init_dir(tb.path()).expect("init b");
    let c_id = store::init_dir(tc.path()).expect("init c");
    chat::chat_init(tb.path()).expect("chat b");

    // B knows A (serving) and C (never serving; messages to it stay queued).
    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    store::peer_add(
        tb.path(),
        "a",
        &a.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("b learns a");
    store::peer_add(tb.path(), "c", &c_id.to_string(), &[]).expect("b names c");
    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    chat::chat_send(tb.path(), "c", "are you there?", None)
        .await
        .expect("queue to c");
    // A petname-keyed outbox row must count too.
    b.node()
        .db()
        .call(|conn| {
            conn.execute(
                "INSERT INTO _outbox (request_id, peer, mod, signal) \
                 VALUES ('01PS-BYNAME', 'c', 'sql-sqlite', 'SELECT 1')",
                [],
            )
            .expect("row");
            // Fabricate a beacon 90s ago for a; c never beaconed.
            conn.execute(
                "UPDATE _peers SET last_seen = datetime('now', '-90 seconds') WHERE name = 'a'",
                [],
            )
            .expect("last_seen");
        })
        .await
        .expect("db");

    let conn = store::open_db(tb.path()).expect("open");
    let peers = store::peer_list(&conn).expect("peer list");
    let c = peers
        .iter()
        .find(|p| p.endpoint_id == c_id.to_string())
        .expect("c row");
    assert!(
        c.pending_outbox >= 2,
        "hex + petname keyed rows: {}",
        c.pending_outbox
    );
    assert!(
        c.last_seen.is_none() && c.last_seen_age_secs.is_none(),
        "never beaconed"
    );
    let a_row = peers
        .iter()
        .find(|p| p.endpoint_id == a_id.to_string())
        .expect("a row");
    let age = a_row.last_seen_age_secs.expect("age");
    assert!((80..=120).contains(&age), "age {age}");
    assert!(a_row.addr_count > 0);

    let st = store::peer_status(&conn, "c").expect("status c");
    assert!(
        st.outbox_by_status
            .iter()
            .any(|(s, n)| s == "queued" && *n >= 2)
    );
    assert!(st.reachable.is_none(), "probe is the caller's job");
    drop(conn);

    // --probe through the binary: a (serving) reachable, c not.
    for (name, want) in [("a", true), ("c", false)] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
            .args([
                "--json",
                "peer",
                "status",
                name,
                "--probe",
                "--offline",
                "-d",
            ])
            .arg(tb.path())
            .output()
            .expect("run");
        let v: serde_json::Value =
            serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json");
        assert_eq!(v["reachable"], want, "{name}: {v}");
        assert!(v["presence"].as_str().unwrap().contains(if name == "a" {
            "seen"
        } else {
            "never"
        }));
    }

    // delivered_at lands on own messages once the peer applied them.
    store::peer_add(
        ta.path(),
        "b",
        &b.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("a learns b");
    chat::chat_init(ta.path()).expect("chat a");
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
    let sent = chat::chat_send(ta.path(), "b", "delivered?", None)
        .await
        .expect("send");
    a.wake_outbox();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let log = chat::chat_log(ta.path(), "b", 5, None).expect("log");
        if let Some(e) = log.iter().find(|e| e.id == sent.message_id)
            && e.status.as_deref() == Some("done")
        {
            assert!(e.delivered_at.is_some(), "delivered_at set on done");
            break;
        }
        assert!(std::time::Instant::now() < deadline, "not delivered");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    a.shutdown().await;
    b.shutdown().await;
}

/// "Is alice up?" is the question this command is run to answer, and
/// `--probe` answered it in its output while its exit code answered a
/// different one: did the probe run. So `rsntr peer status alice
/// --probe && <do the thing>` did the thing for a peer that is down -
/// the same trap `rsntr status` had for a stopped node, which is why
/// the fix is the same shape: a flag that IS the question.
#[tokio::test(flavor = "multi_thread")]
async fn reachable_is_the_predicate_that_probe_could_not_be() {
    let ta = TempDir::new("pr-a");
    let tb = TempDir::new("pr-b");
    let tc = TempDir::new("pr-c");
    store::init_dir(ta.path()).expect("init a");
    store::init_dir(tb.path()).expect("init b");
    let c_id = store::init_dir(tc.path()).expect("init c");

    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    store::peer_add(
        tb.path(),
        "a",
        &a.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("b learns a");
    // c is named but has never served: nothing to answer a dial.
    store::peer_add(tb.path(), "c", &c_id.to_string(), &[]).expect("b names c");

    let run = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
            .args(args)
            .arg(tb.path())
            .output()
            .expect("run peer status");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
        )
    };

    // The predicate: the exit code is the answer.
    let (code, _) = run(&["peer", "status", "a", "--reachable", "--offline", "-d"]);
    assert_eq!(code, 0, "a is serving");
    let (code, _) = run(&["peer", "status", "c", "--reachable", "--offline", "-d"]);
    assert_eq!(code, 1, "c has never served; a dead peer is not exit 0");

    // --json says the same thing, and says why when it is no.
    let (code, out) = run(&[
        "--json",
        "peer",
        "status",
        "c",
        "--reachable",
        "--offline",
        "-d",
    ]);
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("json");
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["reachable"], false, "{v}");
    assert!(v["reason"].is_string(), "names why it could not tell: {v}");

    // The reporting form is unchanged, exit code included: it answers
    // "here is everything I know", which succeeds either way.
    let (code, out) = run(&[
        "--json",
        "peer",
        "status",
        "c",
        "--probe",
        "--offline",
        "-d",
    ]);
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("json");
    assert_eq!(code, 0, "the report still succeeds: {v}");
    assert_eq!(v["reachable"], false, "and still says so in its body: {v}");

    a.shutdown().await;
}
