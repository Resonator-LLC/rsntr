//! M8: the `system` modulation. The owner fetch is unfiltered (peers,
//! policy, modulations, rooms, points); a peer sees only the tables its
//! `read` grants cover; an ungranted peer gets the public core (the
//! node, what it serves, public points). Fetched Turtle round-trips
//! through `turtle load -` into a local store and answers SPARQL.

use std::io::Write;
use std::time::Duration;

use rsntr::testutil::TempDir;
use rsntr::{chat, serve, store};

const RSNTR: &str = "http://resonator.network/v3/rsntr#";

fn rsntr_bin() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
}

/// `--json` carries the triples as N-Triples lines.
fn lines(v: &serde_json::Value) -> Vec<String> {
    v["triples"]
        .as_array()
        .map(|ts| {
            ts.iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn count_typed(v: &serde_json::Value, class: &str) -> usize {
    let want = format!("<http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{RSNTR}{class}> .");
    lines(v).iter().filter(|l| l.ends_with(&want)).count()
}

fn count_pred(v: &serde_json::Value, local: &str) -> usize {
    let want = format!(" <{RSNTR}{local}> ");
    lines(v).iter().filter(|l| l.contains(&want)).count()
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_peer_and_stranger_views_and_round_trip() {
    let ta = TempDir::new("sys-a");
    let tb = TempDir::new("sys-b");
    let tc = TempDir::new("sys-c");
    store::init_dir(ta.path()).expect("init a");
    let b_id = store::init_dir(tb.path()).expect("init b");
    let c_id = store::init_dir(tc.path()).expect("init c");
    chat::chat_init(ta.path()).expect("chat a");
    chat::chat_init(tb.path()).expect("chat b");

    // A admits B with read on _peers only, C with nothing; hosts a room.
    store::peer_add(ta.path(), "b", &b_id.to_string(), &[]).expect("a admits b");
    store::peer_add(ta.path(), "c", &c_id.to_string(), &[]).expect("a admits c");
    {
        let conn = store::open_db(ta.path()).expect("open a");
        conn.execute(
            "INSERT INTO _policy (peer_or_group, table_name, action, effect, note) \
             VALUES (?1, '_peers', 'read', 'allow', 'test')",
            [b_id.to_string()],
        )
        .expect("grant");
    }
    chat::room_create(ta.path(), "ops").expect("room");
    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    let ticket = a.ready_ticket(Duration::from_secs(3)).await;
    store::peer_add(tb.path(), "a", &ticket, &[]).expect("b names a");
    store::peer_add(tc.path(), "a", &ticket, &[]).expect("c names a");

    // Owner: everything.
    let out = rsntr_bin()
        .args(["--json", "system", "graph", "-d"])
        .arg(ta.path())
        .output()
        .expect("owner fetch");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let owner: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json");
    assert_eq!(count_typed(&owner, "Node"), 1, "{owner}");
    assert_eq!(count_typed(&owner, "Peer"), 2);
    assert!(count_typed(&owner, "PolicyRule") >= 1);
    assert_eq!(count_typed(&owner, "Room"), 1);
    assert!(count_pred(&owner, "serves") >= 5);
    assert!(
        count_pred(&owner, "offers") >= 1,
        "chat points offered: {owner}"
    );
    // Coupling lists survive intact: a point's field list reaches nodes
    // carrying rsntr:name (the projection's rdf:List cells are kept).
    let ls = lines(&owner);
    assert!(
        ls.iter()
            .any(|l| l.contains(" <{RSNTR}field> ".replace("{RSNTR}", RSNTR).as_str()))
    );
    assert!(
        ls.iter().any(|l| l.contains("rdf-syntax-ns#first>")),
        "field list cells kept"
    );
    assert!(
        !ls.iter()
            .any(|l| l.contains(&format!("<{RSNTR}Projection>"))),
        "frame header dropped"
    );

    // B (read on _peers): peers yes, policy/rooms no.
    let out = rsntr_bin()
        .args([
            "--json",
            "system",
            "graph",
            "--peer",
            "a",
            "--offline",
            "-d",
        ])
        .arg(tb.path())
        .output()
        .expect("b fetch");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let b_view: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json");
    assert_eq!(count_typed(&b_view, "Peer"), 2, "{b_view}");
    assert_eq!(count_typed(&b_view, "PolicyRule"), 0);
    assert_eq!(count_typed(&b_view, "Room"), 0);
    assert_eq!(count_typed(&b_view, "Node"), 1);

    // C (admitted, no grants): the public core only.
    let out = rsntr_bin()
        .args([
            "--json",
            "system",
            "graph",
            "--peer",
            "a",
            "--offline",
            "-d",
        ])
        .arg(tc.path())
        .output()
        .expect("c fetch");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let c_view: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json");
    assert_eq!(count_typed(&c_view, "Node"), 1, "{c_view}");
    assert_eq!(count_typed(&c_view, "Peer"), 0);
    assert_eq!(count_typed(&c_view, "PolicyRule"), 0);

    // Round trip: B pipes A's graph (Turtle) into its own store via
    // `turtle load -`, then SPARQLs the peers.
    let out = rsntr_bin()
        .args(["system", "graph", "--peer", "a", "--offline", "-d"])
        .arg(tb.path())
        .output()
        .expect("turtle fetch");
    assert!(out.status.success());
    let turtle = out.stdout.clone();
    assert!(String::from_utf8_lossy(&turtle).contains("urn:rsntr:peer:"));
    let mut load = rsntr_bin()
        .args(["--json", "turtle", "load", "-", "-d"])
        .arg(tb.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn load");
    load.stdin.take().unwrap().write_all(&turtle).expect("pipe");
    let out = load.wait_with_output().expect("load");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let loaded: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json");
    assert!(loaded["triples"].as_i64().unwrap_or(0) > 5, "{loaded}");

    let out = rsntr_bin()
        .args([
            "--json",
            "sparql",
            &format!("SELECT ?p WHERE {{ ?p a <{RSNTR}Peer> }}"),
            "-d",
        ])
        .arg(tb.path())
        .output()
        .expect("sparql");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json");
    assert_eq!(rows["rows"].as_array().map(Vec::len), Some(2), "{rows}");

    // A wrong signal is a protocol error, not a hang.
    let out = rsntr_bin()
        .args([
            "--json",
            "query",
            "a",
            "--mod",
            "system",
            "nope",
            "--offline",
            "-d",
        ])
        .arg(tb.path())
        .output()
        .expect("bad signal");
    assert_ne!(out.status.code(), Some(0));

    a.shutdown().await;
}
