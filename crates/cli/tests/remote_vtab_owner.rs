//! The vtab surfaces through the owner channel: two served directories,
//! and owner-lane SQL on node A reads and writes node B's table via
//! remote_query() and an iroh_remote virtual table registered on A's
//! serving connection (the serve wiring).
#![cfg(unix)]

use resonator_protocol::Value;
use rsntr::channel::{self, OwnerChannel, Prefer};
use rsntr::testutil::TempDir;
use rsntr::{serve, store};

#[tokio::test(flavor = "multi_thread")]
async fn owner_lane_sql_reaches_the_peer_through_the_vtabs() {
    let ta = TempDir::new("vtab-owner-a");
    let tb = TempDir::new("vtab-owner-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    let _b_id = store::init_dir(tb.path()).expect("init b");

    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    let ticket = b.ready_ticket(std::time::Duration::from_secs(3)).await;
    store::peer_add(ta.path(), "b", &ticket, &[]).expect("peer add");

    // B admits A with read+write on notes.
    let a_hex = a_id.to_string();
    b.node()
        .db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO _peers (endpoint_id, added_at) VALUES (?1, datetime('now'))",
                [&a_hex],
            )
            .expect("admit a");
            conn.execute_batch(&format!(
                "INSERT INTO _policy (peer_or_group, table_name, action, effect) VALUES
                   ('{a_hex}', 'notes', 'read', 'allow'),
                   ('{a_hex}', 'notes', 'write', 'allow');
                 CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT);
                 INSERT INTO notes (title) VALUES ('from b');"
            ))
            .expect("policy + seed");
        })
        .await
        .expect("db call");

    // A serves after B is dialable, then its owner channel rides the
    // control socket onto the serving connection where the vtabs live.
    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    let channel = OwnerChannel::open(ta.path(), Prefer::Socket)
        .await
        .expect("owner channel");
    assert!(channel.is_socket());

    // remote_query: an owner-lane read that runs on the peer.
    let (columns, rows, _done) = channel::query_rows(
        &channel,
        "SELECT row FROM remote_query('b', 'SELECT title FROM notes')",
        vec![],
    )
    .await
    .expect("remote_query");
    assert_eq!(columns, vec!["row"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].cells[0],
        ("row".to_string(), Value::Text(r#"["from b"]"#.to_string()))
    );

    // iroh_remote: owner-lane DDL creates the mirror, an INSERT lands on B.
    channel::execute(
        &channel,
        "CREATE VIRTUAL TABLE bnotes USING iroh_remote(peer='b', table=notes)",
        vec![],
    )
    .await
    .expect("create vtab");
    channel::execute(
        &channel,
        "INSERT INTO bnotes (title) VALUES ('from a')",
        vec![],
    )
    .await
    .expect("remote insert");

    let titles: Vec<String> = b
        .node()
        .db()
        .call(|conn| {
            conn.prepare("SELECT title FROM notes ORDER BY id")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        })
        .await
        .expect("b state");
    assert_eq!(titles, vec!["from b", "from a"]);

    a.shutdown().await;
    b.shutdown().await;
}
