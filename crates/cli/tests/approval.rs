//! The cross-peer approval flow, end to end over two live offline
//! nodes: an unmatched remote write parks in `_inbox` and answers the
//! stable "pending human decision" reason; the owner allows (with
//! --remember / --grant); the requester's retry succeeds. Also: an
//! explicit deny row stays a hard deny, and a lingering unanswered
//! knock must not stop statements from parking (the json_extract
//! regression).

use std::path::PathBuf;
use std::time::Duration;

use resonator_authenticator::PENDING_REASON;
use rsntr::client::{self, QueryOutcome};
use rsntr::testutil::TempDir;
use rsntr::{Prefer, inboxcmd, modcmd, serve, store};

fn camera_wasm_path() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/camera-mod/target/wasm32-unknown-unknown/release/camera_mod.wasm");
    p.exists().then_some(p)
}

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

fn denied_reason(outcome: &QueryOutcome) -> Option<String> {
    match outcome {
        QueryOutcome::Denied(d) => Some(d.reason.clone().unwrap_or_default()),
        _ => None,
    }
}

const PROJECTION_INSERT: &str = "INSERT OR REPLACE INTO _projection \
     (point_iri, kind, label, ord, modulation, signal, resource, note) \
     VALUES ('urn:appr:pt', 'radiant', 'approved point', 500, 'sql-sqlite', \
     'SELECT 1', NULL, 'approval test')";

#[tokio::test(flavor = "multi_thread")]
async fn statement_parks_owner_allows_retry_succeeds() {
    let ta = TempDir::new("appr-a");
    let tb = TempDir::new("appr-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    store::init_dir(tb.path()).expect("init b");
    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    store::peer_add(
        ta.path(),
        "b",
        &b.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("a learns b");
    let a_hex = a_id.to_string();

    // The json_extract regression setup: an unanswered KNOCK row for A
    // sits in B's inbox before any statement arrives.
    let knock = client::knock(ta.path(), "b", "let me in please", true)
        .await
        .expect("knock");
    assert_eq!(knock.decision.expect("answered").decision, "pending");
    admit(&b, &a_hex).await; // owner admits out of band; knock row stays

    // Unmatched remote write: parks, answers the stable pending reason.
    let first = client::run_query(
        ta.path(),
        "b",
        "sql-sqlite",
        PROJECTION_INSERT,
        &[],
        true,
        None,
    )
    .await
    .expect("first attempt completes");
    assert_eq!(
        denied_reason(&first.outcome).as_deref(),
        Some(PENDING_REASON),
        "expected the park, got {:?}",
        first.outcome
    );

    // A statement row exists (sql non-empty), despite the knock row.
    let rows = inboxcmd::inbox_list_with(tb.path(), Prefer::Auto, false)
        .await
        .expect("inbox list");
    let stmt = rows
        .iter()
        .find(|r| r.kind == "statement" && r.peer == a_hex)
        .expect("parked statement row");

    // A retry while pending answers pending again without stacking.
    let again = client::run_query(
        ta.path(),
        "b",
        "sql-sqlite",
        PROJECTION_INSERT,
        &[],
        true,
        None,
    )
    .await
    .expect("retry completes");
    assert_eq!(
        denied_reason(&again.outcome).as_deref(),
        Some(PENDING_REASON)
    );
    let rows2 = inboxcmd::inbox_list_with(tb.path(), Prefer::Auto, false)
        .await
        .expect("inbox list");
    assert_eq!(
        rows2.iter().filter(|r| r.kind == "statement").count(),
        1,
        "pending retries must not stack rows"
    );

    // The owner allows and remembers; the retry passes and lands.
    let report =
        inboxcmd::inbox_answer_with(tb.path(), Prefer::Auto, &stmt.request_id, true, true, &[])
            .await
            .expect("answer");
    assert!(!report.remembered.is_empty(), "remember writes policy rows");
    let after = client::run_query(
        ta.path(),
        "b",
        "sql-sqlite",
        PROJECTION_INSERT,
        &[],
        true,
        None,
    )
    .await
    .expect("approved retry");
    assert!(
        matches!(after.outcome, QueryOutcome::Rows { .. }),
        "expected success after allow, got {:?}",
        after.outcome
    );
    let point = client::run_projection(ta.path(), "b", "", true)
        .await
        .expect("projection");
    match point {
        client::ProjectionOutcome::Projection(p) => {
            assert!(p.offers.iter().any(|o| o.iri == "urn:appr:pt"));
        }
        other => panic!("expected the projection, got {other:?}"),
    }

    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mod_install_parks_grant_unblocks_and_deny_is_hard() {
    let Some(wasm) = camera_wasm_path() else {
        eprintln!("SKIPPED: camera_mod.wasm unavailable");
        return;
    };
    let ta = TempDir::new("appr2-a");
    let tb = TempDir::new("appr2-b");
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
    store::peer_add(
        tb.path(),
        "a",
        &a.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("b learns a");
    let a_hex = a_id.to_string();
    admit(&b, &a_hex).await;

    // Unmatched: the push parks and answers pending (not a flat deny).
    let push = modcmd::mod_push(
        ta.path(),
        "b",
        "cameras",
        &wasm,
        &[],
        None,
        None,
        None,
        true,
    )
    .await
    .expect("push completes");
    assert_eq!(
        denied_reason(&push.outcome).as_deref(),
        Some(PENDING_REASON),
        "expected the park, got {:?}",
        push.outcome
    );
    let rows = inboxcmd::inbox_list_with(tb.path(), Prefer::Auto, false)
        .await
        .expect("inbox list");
    let parked = rows
        .iter()
        .find(|r| r.kind == "statement" && r.summary.contains("ModInstall"))
        .expect("parked install row");

    // Owner answers allow with the grants; the retry installs.
    inboxcmd::inbox_answer_with(
        tb.path(),
        Prefer::Auto,
        &parked.request_id,
        true,
        false,
        &[
            "cameras=mod-install".to_string(),
            "*=mod:cameras".to_string(),
        ],
    )
    .await
    .expect("answer with grants");
    let retry = modcmd::mod_push(
        ta.path(),
        "b",
        "cameras",
        &wasm,
        &[],
        None,
        None,
        None,
        true,
    )
    .await
    .expect("retry push");
    assert!(
        matches!(retry.outcome, QueryOutcome::Rows { .. }),
        "expected ModInstalled after grant, got {:?}",
        retry.outcome
    );
    let invoked = client::run_query(ta.path(), "b", "cameras", "hologram", &[], true, None)
        .await
        .expect("invoke");
    assert!(
        matches!(
            invoked.outcome,
            QueryOutcome::Rows { .. } | QueryOutcome::Graph { .. }
        ),
        "expected the mod to answer, got {:?}",
        invoked.outcome
    );

    // An explicit deny row is a hard deny for another name: no park.
    b.node()
        .db()
        .call({
            let a_hex = a_hex.clone();
            move |conn| {
                conn.execute(
                    "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
                     VALUES (?1, 'forbidden', 'mod-install', 'deny')",
                    [&a_hex],
                )
                .expect("deny row");
            }
        })
        .await
        .expect("db call");
    let denied = modcmd::mod_push(
        ta.path(),
        "b",
        "forbidden",
        &wasm,
        &[],
        None,
        None,
        None,
        true,
    )
    .await
    .expect("denied push completes");
    let reason = denied_reason(&denied.outcome).expect("a denial");
    assert_ne!(reason, PENDING_REASON, "an explicit deny must not park");
    assert!(reason.contains("denied by policy"), "{reason}");
    let rows = inboxcmd::inbox_list_with(tb.path(), Prefer::Auto, false)
        .await
        .expect("inbox list");
    assert!(
        !rows.iter().any(|r| r.summary.contains("forbidden")),
        "no inbox row for a hard deny"
    );

    a.shutdown().await;
    b.shutdown().await;
}

/// A request parked for the peer's owner came out of the CLI as
/// "denied: pending human decision".
///
/// Every word of that is true and it points the reader the wrong way:
/// nobody refused anything, the owner has not answered yet, and the
/// identical retry succeeds once they do. `--json` has always marked it
/// (`"pending": true`); the line a person reads did not, and the page
/// commands next door already phrased it properly, so "denied" meant
/// two different things depending on which command printed it.
///
/// The DDL refusal at the end is the control: it is a real denial on
/// the same connection, so it proves the assertion can fail.
#[tokio::test(flavor = "multi_thread")]
async fn a_parked_write_reads_as_parked_not_as_denied() {
    let ta = TempDir::new("parkline-a");
    let tb = TempDir::new("parkline-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    store::init_dir(tb.path()).expect("init b");
    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    store::peer_add(
        ta.path(),
        "b",
        &b.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("a learns b");
    admit(&b, &a_id.to_string()).await;

    let query = |sql: &str| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
            .args(["query", "b", sql, "--offline", "-d"])
            .arg(ta.path())
            .output()
            .expect("run query");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };

    let (code, err) = query(PROJECTION_INSERT);
    assert_eq!(code, 2, "a parked request still exits 2: {err}");
    assert!(err.starts_with("pending:"), "reads as parked: {err}");
    assert!(
        err.contains("not denied"),
        "says plainly it is not a refusal: {err}"
    );
    assert!(
        err.contains("inbox answer"),
        "names who unblocks it and how: {err}"
    );

    // The control: a genuine refusal on the same connection.
    let (code, err) = query("CREATE TABLE ddl_control (x)");
    assert_eq!(code, 2, "{err}");
    assert!(
        err.starts_with("denied:"),
        "a real denial is still a denial: {err}"
    );

    b.shutdown().await;
}
