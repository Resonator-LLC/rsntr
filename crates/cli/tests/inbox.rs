//! `rsntr inbox list` / `rsntr inbox answer` over the owner channel:
//! a parked statement is answered with allow-and-remember (feeding the
//! `_decisions` cache and generating `_policy` rows) and a parked knock
//! answer admits the peer.

use resonator_authenticator::{ActionKind, Chain, Decision, Footprint, HumanTier, Tier};
use rsntr::testutil::TempDir;
use rsntr::{Prefer, inboxcmd, store};

#[tokio::test(flavor = "multi_thread")]
async fn inbox_list_and_answer_ride_the_owner_channel() {
    let tmp = TempDir::new("inbox");
    store::init_dir(tmp.path()).expect("init");

    let fp = Footprint::from_tables(ActionKind::Read, [("notes", vec!["id"])]);
    {
        // Opt the human tier in, then park one statement exactly as the
        // serving pipeline's chain would, plus one knock as serve_knock
        // writes it.
        let conn = store::open_db(tmp.path()).expect("open");
        conn.execute(
            "UPDATE _rsntr SET value = ?1 WHERE key = 'auth_chain'",
            [r#"["cache","policy","human"]"#],
        )
        .unwrap();
        let d = HumanTier.decide(
            &conn,
            "aliceid",
            "read",
            &fp,
            "SELECT id FROM notes WHERE id = 1",
        );
        assert!(matches!(d, Decision::Deny { .. }));
        conn.execute(
            "INSERT INTO _inbox (request_id, peer, sql, params, received_at) \
             VALUES ('k1', 'strangerid', '', 'knock: hi', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    }

    let rows = inboxcmd::inbox_list_with(tmp.path(), Prefer::Local, false)
        .await
        .expect("list");
    assert_eq!(rows.len(), 2);
    let statement_row = rows.iter().find(|r| r.kind == "statement").expect("parked");
    assert_eq!(statement_row.peer, "aliceid");
    assert!(statement_row.summary.contains("SELECT id FROM notes"));
    assert!(statement_row.decision.is_none());
    let knock_row = rows.iter().find(|r| r.kind == "knock").expect("knock");
    assert_eq!(knock_row.summary, "knock: hi");

    // Allow-and-remember the statement.
    let report = inboxcmd::inbox_answer_with(
        tmp.path(),
        Prefer::Local,
        &statement_row.request_id,
        true,
        true,
        &[],
    )
    .await
    .expect("answer statement");
    assert!(!report.knock);
    assert_eq!(report.decision, "allow");
    assert_eq!(report.remembered, vec!["notes".to_string()]);

    // Allow the knock: the peer is admitted.
    let report = inboxcmd::inbox_answer_with(tmp.path(), Prefer::Local, "k1", true, false, &[])
        .await
        .expect("answer knock");
    assert!(report.knock);
    assert_eq!(report.peer, "strangerid");

    // Answering an already-decided row is refused.
    assert!(
        inboxcmd::inbox_answer_with(tmp.path(), Prefer::Local, "k1", true, false, &[])
            .await
            .is_err()
    );

    let conn = store::open_db(tmp.path()).expect("reopen");
    // The NEXT identical request (different literal) hits the cache.
    let chain = Chain::with_builtin_tiers();
    let d = chain.decide(
        &conn,
        "aliceid",
        "read",
        &fp,
        "SELECT id FROM notes WHERE id = 2",
    );
    assert_eq!(d.decided_by, "cache");
    assert_eq!(d.decision, Decision::Allow);
    // The generated policy row and the admission are in place.
    let policy: i64 = conn
        .query_row(
            "SELECT count(*) FROM _policy WHERE peer_or_group = 'aliceid' \
             AND table_name = 'notes' AND action = 'read' AND effect = 'allow'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(policy, 1);
    let admitted: i64 = conn
        .query_row(
            "SELECT count(*) FROM _peers WHERE endpoint_id = 'strangerid'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(admitted, 1);
    drop(conn);

    // Nothing pending remains; --all still shows the answered rows.
    assert!(
        inboxcmd::inbox_list_with(tmp.path(), Prefer::Local, false)
            .await
            .expect("relist")
            .is_empty()
    );
    let all = inboxcmd::inbox_list_with(tmp.path(), Prefer::Local, true)
        .await
        .expect("list all");
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|r| r.decision.as_deref() == Some("allow")));
}
