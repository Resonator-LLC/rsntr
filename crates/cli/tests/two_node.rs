//! Scripted two-node offline integration test: node A (the client) and
//! node B (the server) live in fresh directories, B serves in-process on
//! localhost with no relays, A is admitted via SQL, and the CLI library
//! functions round-trip query (sql-sqlite and sparql), help, and a
//! denial for a write no policy allows.

use resonator_protocol::Value;
use rsntr::client::{self, QueryOutcome};
use rsntr::testutil::TempDir;
use rsntr::{serve, store};

#[tokio::test(flavor = "multi_thread")]
async fn two_node_offline_round_trips() {
    let ta = TempDir::new("two-node-a");
    let tb = TempDir::new("two-node-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    let _b_id = store::init_dir(tb.path()).expect("init b");

    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    assert!(
        !b.direct_addrs().is_empty(),
        "offline serve must bind localhost"
    );

    // The live ticket names the serving endpoint itself: same key, and
    // its addresses are the serving sockets (the POC ticket-gap fix).
    let ticket = b.ready_ticket(std::time::Duration::from_secs(3)).await;
    let (ticket_peer, ticket_addrs) =
        resonator_transport::parse_ticket(&ticket).expect("parse live ticket");
    assert_eq!(ticket_peer, b.peer_id());
    assert!(
        !ticket_addrs.is_empty(),
        "live offline ticket carries dial addrs"
    );

    // A learns B via the live ticket (peer add accepts tickets).
    let (peer_id, stored) =
        store::peer_add(ta.path(), "b", &ticket, &[]).expect("peer add via ticket");
    assert_eq!(peer_id, b.peer_id());
    assert!(!stored.is_empty());

    // B admits A and allows reads on everything; a demo table holds data.
    let a_hex = a_id.to_string();
    b.node()
        .db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO _peers (endpoint_id, added_at) VALUES (?1, datetime('now'))",
                [&a_hex],
            )
            .expect("admit a");
            conn.execute(
                "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
                 VALUES (?1, '*', 'read', 'allow')",
                [&a_hex],
            )
            .expect("allow reads");
            conn.execute_batch(
                "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT);\
                 INSERT INTO notes (title) VALUES ('hello from b');",
            )
            .expect("seed table");
        })
        .await
        .expect("db call");

    // sql-sqlite round trip with a positional parameter.
    let report = client::run_query(
        ta.path(),
        "b",
        "sql-sqlite",
        "SELECT id, title FROM notes WHERE title = ?",
        &["hello from b".to_string()],
        true,
        None,
    )
    .await
    .expect("query");
    match &report.outcome {
        QueryOutcome::Rows {
            columns,
            rows,
            done,
        } => {
            assert_eq!(columns, &["id", "title"]);
            assert_eq!(rows.len(), 1);
            assert_eq!(done.row_count, Some(1));
            assert!(!done.truncated);
            let row = &rows[0];
            assert_eq!(
                row.cells.iter().find(|(n, _)| n == "title").map(|(_, v)| v),
                Some(&Value::Text("hello from b".to_string()))
            );
        }
        other => panic!("expected rows, got {other:?}"),
    }

    // sparql round trip (the store starts empty: header + zero rows).
    let sparql = client::run_query(
        ta.path(),
        "b",
        "sparql",
        "SELECT ?s WHERE { ?s ?p ?o }",
        &[],
        true,
        None,
    )
    .await
    .expect("sparql query");
    match &sparql.outcome {
        QueryOutcome::Rows { rows, done, .. } => {
            assert!(rows.is_empty());
            assert_eq!(done.row_count, Some(0));
        }
        other => panic!("expected empty sparql rows, got {other:?}"),
    }

    // help round trip: served to admitted peers and strangers alike.
    let help = client::run_help(ta.path(), "b", None, true)
        .await
        .expect("help");
    match &help.outcome {
        QueryOutcome::Help { text, .. } => {
            assert!(
                text.contains("resonator node"),
                "help text should carry the owner overview, got {text:?}"
            );
        }
        other => panic!("expected help, got {other:?}"),
    }

    // A write no policy allows is denied (exit-code surface: denied).
    let denied = client::run_query(
        ta.path(),
        "b",
        "sql-sqlite",
        "INSERT INTO notes (title) VALUES ('sneaky')",
        &[],
        true,
        None,
    )
    .await
    .expect("denied write completes the protocol");
    assert!(
        matches!(&denied.outcome, QueryOutcome::Denied(_)),
        "expected a denial, got {:?}",
        denied.outcome
    );

    b.shutdown().await;
}
