//! `rsntr peer remove` against a serving node: the removal rides the
//! owner socket, and the peer is really refused afterwards rather than
//! merely missing from `peer list`.

use std::process::Command;
use std::time::Duration;

use rsntr::client::{self, QueryOutcome};
use rsntr::testutil::TempDir;
use rsntr::{serve, store};

async fn ask(dir: &std::path::Path) -> QueryOutcome {
    client::run_query(
        dir,
        "a",
        "sql-sqlite",
        "SELECT body FROM notes",
        &[],
        true,
        None,
    )
    .await
    .expect("query ran")
    .outcome
}

fn rsntr(args: &[&str], dir: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(args)
        .arg("-d")
        .arg(dir)
        .output()
        .expect("rsntr runs")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_removed_peer_is_refused_and_stops_being_messaged() {
    let ta = TempDir::new("prm-a");
    let tb = TempDir::new("prm-b");
    store::init_dir(ta.path()).expect("init a");
    let b_id = store::init_dir(tb.path()).expect("init b");
    let b_hex = b_id.to_string();

    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    store::peer_add(
        tb.path(),
        "a",
        &a.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("b learns a");

    // A admits B over its own socket, lets it read one table, and has a
    // message waiting for it (B is not serving, so it stays undelivered).
    store::peer_add(ta.path(), "b", &b_hex, &[]).expect("a admits b");
    {
        let b_hex = b_hex.clone();
        a.node()
            .db()
            .call(move |conn| {
                conn.execute_batch(
                    "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);
                     INSERT INTO notes (body) VALUES ('hello b');",
                )
                .expect("seed notes");
                conn.execute(
                    "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
                     VALUES (?1, 'notes', 'read', 'allow')",
                    [&b_hex],
                )
                .expect("grant");
                conn.execute(
                    "INSERT INTO _outbox (request_id, peer, mod, signal) \
                     VALUES ('01PRM-WAITING', ?1, 'sql-sqlite', 'SELECT 1')",
                    [&b_hex],
                )
                .expect("waiting message");
            })
            .await
            .expect("db call");
    }

    // Positive control: before removal B is admitted and served, so a
    // refusal afterwards means something.
    match ask(tb.path()).await {
        QueryOutcome::Rows { rows, .. } => assert_eq!(rows.len(), 1),
        other => panic!("B should be served before removal: {other:?}"),
    }

    let out = rsntr(&["--json", "peer", "remove", "b"], ta.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(v["ok"], true);
    assert_eq!(v["endpoint_id"], b_hex);
    assert_eq!(v["name"], "b");
    assert_eq!(v["removed"]["peer"], 1);
    assert_eq!(v["removed"]["policy"], 1);
    assert_eq!(v["removed"]["outbox_cancelled"], 1);

    // It went through the serving process, on the owner ledger.
    let (audited, status) = a
        .node()
        .db()
        .call(|conn| {
            let audited: i64 = conn
                .query_row(
                    "SELECT count(*) FROM _audit WHERE signal LIKE 'DELETE FROM _peers%' \
                     AND direction = 'local' AND decided_by = 'owner' AND decision = 'allow'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let status: String = conn
                .query_row(
                    "SELECT status || '/' || coalesce(error, '') FROM _outbox \
                     WHERE request_id = '01PRM-WAITING'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            (audited, status)
        })
        .await
        .expect("db call");
    assert_eq!(audited, 1, "the removal ran on the serving connection");
    assert_eq!(status, "error/peer removed");

    // The same request that was served a moment ago is now refused.
    match ask(tb.path()).await {
        QueryOutcome::Denied(d) => {
            let reason = d.reason.unwrap_or_default();
            assert!(reason.contains("unknown peer"), "{reason}");
        }
        other => panic!("B should be refused after removal: {other:?}"),
    }

    // Nothing left: the `rm` alias reports it as an ordinary failure.
    let again = rsntr(&["--json", "peer", "rm", "b"], ta.path());
    assert_eq!(again.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&again.stdout).expect("json");
    assert_eq!(v["ok"], false);

    a.shutdown().await;
}
