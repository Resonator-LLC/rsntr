//! `rsntr sparql` / `rsntr turtle`: local loads and queries over the
//! owner channel, remote round trips under policy, multi-chunk loads,
//! idempotent re-runs, and denial without a grant.

use std::time::Duration;

use rsntr::testutil::TempDir;
use rsntr::{Prefer, client, rdfcmd, serve, store};

const DOC: &str = r#"
@prefix ex: <http://example.org/> .
ex:alice ex:knows ex:bob .
ex:alice ex:name "Alice" .
ex:bob ex:name "Bob" .
ex:carol ex:name "Carol" .
"#;

fn triple_count(report: &client::QueryReport) -> i64 {
    match &report.outcome {
        client::QueryOutcome::Rows { rows, .. } => rows
            .first()
            .and_then(|r| r.cells.first())
            .map(|(_, v)| match v {
                resonator_protocol::Value::Integer(i) => *i,
                // SPARQL results carry typed literals, e.g.
                // "4"^^<http://www.w3.org/2001/XMLSchema#integer>.
                resonator_protocol::Value::Text(t) => t
                    .trim_start_matches('"')
                    .split('"')
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or_else(|| panic!("unparsable count literal {t:?}")),
                other => panic!("expected a count, got {other:?}"),
            })
            .unwrap_or(0),
        other => panic!("expected rows, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn local_load_query_construct_and_idempotent_reload() {
    let tmp = TempDir::new("rdf-local");
    store::init_dir(tmp.path()).expect("init");

    let outcome = rdfcmd::turtle_load(tmp.path(), Prefer::Local, None, DOC, true, 64)
        .await
        .expect("load");
    let rdfcmd::LoadOutcome::Loaded { triples, chunks } = outcome else {
        panic!("load refused: {outcome:?}");
    };
    assert_eq!(triples, 4);
    assert!(chunks > 1, "a tiny chunk budget must split the load");

    let count = rdfcmd::sparql_report(
        tmp.path(),
        Prefer::Local,
        None,
        "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }",
        true,
        None,
    )
    .await
    .expect("count");
    assert_eq!(triple_count(&count), 4);

    // CONSTRUCT comes back as triples.
    let graph = rdfcmd::sparql_report(
        tmp.path(),
        Prefer::Local,
        None,
        "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }",
        true,
        None,
    )
    .await
    .expect("construct");
    match &graph.outcome {
        client::QueryOutcome::Graph { triples, .. } => assert_eq!(triples.len(), 4),
        other => panic!("expected a graph, got {other:?}"),
    }

    // Reload: RDF is a set, so the store is unchanged.
    let again = rdfcmd::turtle_load(tmp.path(), Prefer::Local, None, DOC, true, 64)
        .await
        .expect("reload");
    assert!(matches!(again, rdfcmd::LoadOutcome::Loaded { .. }));
    let count = rdfcmd::sparql_report(
        tmp.path(),
        Prefer::Local,
        None,
        "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }",
        true,
        None,
    )
    .await
    .expect("recount");
    assert_eq!(triple_count(&count), 4, "reload must be a no-op");

    // turtle dump is the CONSTRUCT sugar.
    let dump = rdfcmd::turtle_dump(tmp.path(), Prefer::Local, None, true)
        .await
        .expect("dump");
    match &dump.outcome {
        client::QueryOutcome::Graph { triples, .. } => assert_eq!(triples.len(), 4),
        other => panic!("expected a graph, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_load_and_query_under_policy() {
    let ta = TempDir::new("rdf-rem-a");
    let tb = TempDir::new("rdf-rem-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    store::init_dir(tb.path()).expect("init b");

    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    let ticket = b.ready_ticket(Duration::from_secs(3)).await;
    store::peer_add(ta.path(), "b", &ticket, &[]).expect("a learns b");

    // B admits A with read+write on everything.
    let a_hex = a_id.to_string();
    b.node()
        .db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO _peers (endpoint_id, added_at) VALUES (?1, datetime('now'))",
                [&a_hex],
            )
            .expect("admit");
            for action in ["read", "write"] {
                conn.execute(
                    "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
                     VALUES (?1, '*', ?2, 'allow')",
                    [&a_hex, &action.to_string()],
                )
                .expect("policy");
            }
        })
        .await
        .expect("db call");

    // Remote multi-chunk load into B.
    let outcome = rdfcmd::turtle_load(ta.path(), Prefer::Local, Some("b"), DOC, true, 64)
        .await
        .expect("remote load");
    let rdfcmd::LoadOutcome::Loaded { triples, chunks } = outcome else {
        panic!("remote load refused: {outcome:?}");
    };
    assert_eq!(triples, 4);
    assert!(chunks > 1);

    // Remote SELECT and CONSTRUCT see the loaded graph.
    let count = rdfcmd::sparql_report(
        ta.path(),
        Prefer::Local,
        Some("b"),
        "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }",
        true,
        None,
    )
    .await
    .expect("remote count");
    assert_eq!(triple_count(&count), 4);
    let dump = rdfcmd::turtle_dump(ta.path(), Prefer::Local, Some("b"), true)
        .await
        .expect("remote dump");
    match &dump.outcome {
        client::QueryOutcome::Graph { triples, .. } => assert_eq!(triples.len(), 4),
        other => panic!("expected a graph, got {other:?}"),
    }

    // Idempotent re-run against the remote store too.
    rdfcmd::turtle_load(ta.path(), Prefer::Local, Some("b"), DOC, true, 64)
        .await
        .expect("remote reload");
    let count = rdfcmd::sparql_report(
        ta.path(),
        Prefer::Local,
        Some("b"),
        "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }",
        true,
        None,
    )
    .await
    .expect("remote recount");
    assert_eq!(triple_count(&count), 4);

    // A peer without the write grant is refused.
    let tc = TempDir::new("rdf-rem-c");
    let c_id = store::init_dir(tc.path()).expect("init c");
    store::peer_add(tc.path(), "b", &ticket, &[]).expect("c learns b");
    let c_hex = c_id.to_string();
    b.node()
        .db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO _peers (endpoint_id, added_at) VALUES (?1, datetime('now'))",
                [&c_hex],
            )
            .expect("admit c");
        })
        .await
        .expect("db call");
    let refused = rdfcmd::turtle_load(tc.path(), Prefer::Local, Some("b"), DOC, true, 64)
        .await
        .expect("refused load completes the protocol");
    match refused {
        rdfcmd::LoadOutcome::Refused(report) => {
            assert!(
                matches!(report.outcome, client::QueryOutcome::Denied(_)),
                "expected a denial, got {:?}",
                report.outcome
            );
        }
        other => panic!("expected Refused, got {other:?}"),
    }

    b.shutdown().await;
}
