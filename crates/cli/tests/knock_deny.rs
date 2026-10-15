//! M6: a re-knock while pending answers the same pending id (a status
//! check, no second row); a human deny is durable (policy row), so the
//! next knock is denied by policy without parking; deleting the row
//! lets the peer knock again.

use std::time::Duration;

use rsntr::testutil::TempDir;
use rsntr::{Prefer, client, inboxcmd, serve, store};

fn pending_knocks(dir: &std::path::Path, peer: &str) -> i64 {
    let conn = store::open_db(dir).expect("open");
    conn.query_row(
        "SELECT count(*) FROM _inbox WHERE peer = ?1 AND decision IS NULL AND sql = ''",
        [peer],
        |r| r.get(0),
    )
    .expect("count")
}

/// One knock per key per minute is the production budget; the test
/// refills the persisted bucket so consecutive knocks are answered.
fn refill_knock_budget(dir: &std::path::Path) {
    let conn = store::open_db(dir).expect("open");
    conn.execute("DELETE FROM _knock_budget", [])
        .expect("refill");
}

#[tokio::test(flavor = "multi_thread")]
async fn reknock_is_a_status_check_and_deny_is_durable() {
    let tn = TempDir::new("kd-node");
    let ts = TempDir::new("kd-stranger");
    store::init_dir(tn.path()).expect("init node");
    let s_id = store::init_dir(ts.path()).expect("init stranger");
    let s_hex = s_id.to_string();

    let node = serve::start_node(tn.path(), true).await.expect("serve");
    store::peer_add(
        ts.path(),
        "node",
        &node.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("stranger names the node");

    // First knock parks; the answer names the pending id.
    let first = client::knock(ts.path(), "node", "hi, let me in", true)
        .await
        .expect("knock 1");
    let d1 = first.decision.expect("answered");
    assert_eq!(d1.decision, "pending", "{d1:?}");
    let pending_id = d1.id.clone().expect("id");
    assert_eq!(pending_id, first.id, "the first knock's own id is honored");
    assert!(
        d1.reason.as_deref().unwrap_or("").contains(&pending_id),
        "{d1:?}"
    );
    assert_eq!(pending_knocks(tn.path(), &s_hex), 1);

    // Second knock: same pending id, still one row.
    refill_knock_budget(tn.path());
    let second = client::knock(ts.path(), "node", "still here?", true)
        .await
        .expect("knock 2");
    let d2 = second.decision.expect("answered");
    assert_eq!(d2.decision, "pending");
    assert_eq!(
        d2.id.as_deref(),
        Some(pending_id.as_str()),
        "stable pending id"
    );
    assert_ne!(
        second.id, pending_id,
        "the client minted a fresh id, the node kept the row's"
    );
    assert_eq!(pending_knocks(tn.path(), &s_hex), 1);

    // The owner denies: durable policy row.
    let report =
        inboxcmd::inbox_answer_with(tn.path(), Prefer::Auto, &pending_id, false, false, &[])
            .await
            .expect("deny");
    assert!(report.knock && report.deny_policy_written, "{report:?}");
    {
        let conn = store::open_db(tn.path()).expect("open");
        let note: String = conn
            .query_row(
                "SELECT note FROM _policy WHERE peer_or_group = ?1 AND table_name = '*' \
                 AND action = 'knock' AND effect = 'deny'",
                [&s_hex],
                |r| r.get(0),
            )
            .expect("deny row");
        assert!(
            note.contains(&pending_id) && note.contains("delete this row"),
            "{note}"
        );
    }

    // Third knock: denied by policy, nothing parks.
    refill_knock_budget(tn.path());
    let third = client::knock(ts.path(), "node", "please?", true)
        .await
        .expect("knock 3");
    let d3 = third.decision.expect("answered");
    assert_eq!(d3.decision, "deny", "{d3:?}");
    assert_eq!(d3.decided_by, "policy", "{d3:?}");
    assert_eq!(pending_knocks(tn.path(), &s_hex), 0);

    // Deleting the row reopens the door: the knock parks again, fresh id.
    {
        let conn = store::open_db(tn.path()).expect("open");
        conn.execute(
            "DELETE FROM _policy WHERE peer_or_group = ?1 AND action = 'knock' AND effect = 'deny'",
            [&s_hex],
        )
        .expect("delete");
    }
    refill_knock_budget(tn.path());
    let fourth = client::knock(ts.path(), "node", "one more time", true)
        .await
        .expect("knock 4");
    let d4 = fourth.decision.expect("answered");
    assert_eq!(d4.decision, "pending", "{d4:?}");
    assert_ne!(d4.id.as_deref(), Some(pending_id.as_str()));
    assert_eq!(pending_knocks(tn.path(), &s_hex), 1);

    // The CLI reports the durable deny.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args([
            "--json",
            "inbox",
            "answer",
            d4.id.as_deref().unwrap(),
            "deny",
            "-d",
        ])
        .arg(tn.path())
        .output()
        .expect("run answer");
    let v: serde_json::Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
        .unwrap_or_else(|e| panic!("json: {e}; stderr {}", String::from_utf8_lossy(&out.stderr)));
    assert_eq!(v["deny_policy_written"], true, "{v}");

    node.shutdown().await;
}

/// A parked knock is not admission, and the CLI has to say so with the
/// only two things a script reads: the exit code and `ok`.
///
/// It used to answer exit 0 / `ok:true` for `pending`, so the obvious
/// `rsntr knock <peer> ... && rsntr query <peer> ...` walked straight
/// into the refusal the peer was always going to give. Every other
/// command that parks answers exit 2 with `"pending": true`
/// (docs/agents.md); knock was the one that did not.
///
/// And the id it printed was the client's own ULID, not the `_inbox`
/// row's, so on a re-knock it named a row that exists nowhere - while
/// the manual tells the reader `id` is the pending inbox row.
#[tokio::test(flavor = "multi_thread")]
async fn a_parked_knock_reports_pending_not_admission() {
    let tn = TempDir::new("kp-node");
    let ts = TempDir::new("kp-stranger");
    store::init_dir(tn.path()).expect("init node");
    store::init_dir(ts.path()).expect("init stranger");

    let node = serve::start_node(tn.path(), true).await.expect("serve");
    store::peer_add(
        ts.path(),
        "node",
        &node.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("stranger names the node");

    let knock_json = |dir: &std::path::Path| -> (i32, serde_json::Value) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
            .args(["--json", "knock", "node", "let me in", "--offline", "-d"])
            .arg(dir)
            .output()
            .expect("run knock");
        let v =
            serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap_or_else(|e| {
                panic!("json: {e}; stderr {}", String::from_utf8_lossy(&out.stderr))
            });
        (out.status.code().unwrap_or(-1), v)
    };

    let (code, v) = knock_json(ts.path());
    assert_eq!(code, 2, "a parked knock is not a success: {v}");
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["decision"], "pending", "{v}");
    assert_eq!(v["pending"], true, "the parked marker every parker sets");
    let pending_id = v["id"].as_str().expect("id").to_string();
    assert_eq!(
        v["request_id"].as_str(),
        Some(pending_id.as_str()),
        "a first knock's own id is honored, so both ids agree: {v}"
    );

    // Re-knock: the peer answers with the SAME row id and the client
    // mints a fresh request id. `id` must stay the row's.
    refill_knock_budget(tn.path());
    let (code, v) = knock_json(ts.path());
    assert_eq!(code, 2, "still parked: {v}");
    assert_eq!(
        v["id"].as_str(),
        Some(pending_id.as_str()),
        "id names the pending _inbox row, the one its owner answers: {v}"
    );
    assert_ne!(
        v["request_id"].as_str(),
        Some(pending_id.as_str()),
        "the locally minted id is kept, under its own name: {v}"
    );
    {
        // The id it printed is really the row waiting in the inbox.
        let conn = store::open_db(tn.path()).expect("open");
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM _inbox WHERE request_id = ?1 AND decision IS NULL",
                [&pending_id],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(n, 1, "no _inbox row named {pending_id}");
    }

    // The owner allows: now, and only now, is it exit 0.
    inboxcmd::inbox_answer_with(tn.path(), Prefer::Auto, &pending_id, true, false, &[])
        .await
        .expect("allow");
    refill_knock_budget(tn.path());
    let (code, v) = knock_json(ts.path());
    assert_eq!(code, 0, "admitted: {v}");
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["decision"], "allow", "{v}");
    assert_eq!(v["pending"], false, "{v}");

    node.shutdown().await;
}
