//! `rsntr serve --web` wiring: the web server runs against the live
//! node with a minted token, and the fragment URL is printable. The
//! API is exercised end to end through the real serve path (offline
//! node, real db dir): /api/sql, /api/sparql, a framed POST /request
//! round-trip, and a CSV import/export round-trip.
#![cfg(feature = "web")]

use bytes::BytesMut;
use resonator_protocol::{
    EnvelopeObject, Request, RequestKind, Value, decode_frame_eof, encode_envelope,
};
use rsntr::testutil::TempDir;
use rsntr::{serve, store};

fn frame_bytes(obj: &EnvelopeObject) -> Vec<u8> {
    let mut buf = BytesMut::new();
    encode_envelope(obj, &mut buf).expect("encode frame");
    buf.to_vec()
}

fn decode_frames(bytes: &[u8]) -> Vec<EnvelopeObject> {
    let mut buf = BytesMut::from(bytes);
    let mut out = Vec::new();
    while let Some(doc) = decode_frame_eof(&mut buf).expect("frame") {
        out.push(EnvelopeObject::from_turtle(&doc).expect("envelope"));
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_with_web_interface() {
    let tmp = TempDir::new("serve-web");
    let id = store::init_dir(tmp.path()).expect("init");

    // DDL never rides the pipeline; seed the user table directly.
    {
        let conn = store::open_db(tmp.path()).expect("open db");
        conn.execute(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT)",
            [],
        )
        .expect("create notes");
    }

    let running = serve::start_node(tmp.path(), true).await.expect("serve");
    let web = serve::start_web(
        &running,
        tmp.path(),
        "127.0.0.1:0".parse().expect("addr"),
        false,
    )
    .await
    .expect("web");
    let base = format!("http://{}", web.addr());

    // 43-char base64url token, carried only in the URL fragment.
    assert_eq!(web.token().len(), 43);
    assert_eq!(web.url(), format!("http://{}/#{}", web.addr(), web.token()));

    let client = reqwest::Client::new();
    let r = client
        .get(format!("{base}/api/meta"))
        .send()
        .await
        .expect("unauthenticated");
    assert_eq!(r.status(), 401);
    let r = client
        .get(format!("{base}/api/meta"))
        .bearer_auth(web.token())
        .send()
        .await
        .expect("meta");
    assert_eq!(r.status(), 200);
    let meta: serde_json::Value = r.json().await.expect("json");
    assert_eq!(meta["node_id"], id.to_string());
    assert!(
        meta["tables"]
            .as_array()
            .expect("tables")
            .iter()
            .any(|t| t["name"] == "_peers" && t["reserved"] == true)
    );

    // /api/sql: insert + read back through the pipeline.
    let r = client
        .post(format!("{base}/api/sql"))
        .bearer_auth(web.token())
        .json(&serde_json::json!({ "sql": "INSERT INTO notes (title) VALUES ('groceries')" }))
        .send()
        .await
        .expect("sql insert");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["ok"], true, "{body}");
    let r = client
        .post(format!("{base}/api/sql"))
        .bearer_auth(web.token())
        .json(&serde_json::json!({
            "sql": "SELECT title FROM notes WHERE title = ?",
            "params": ["groceries"],
        }))
        .send()
        .await
        .expect("sql select");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["rows"], serde_json::json!([["groceries"]]));

    // /api/sparql: Update then SELECT, N-Triples lexical cells.
    let r = client
        .post(format!("{base}/api/sparql"))
        .bearer_auth(web.token())
        .json(&serde_json::json!({
            "query": "INSERT DATA { <http://ex.org/n1> <http://ex.org/title> \"hi\" }",
        }))
        .send()
        .await
        .expect("sparql update");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["affected_rows"], 1, "{body}");
    let r = client
        .post(format!("{base}/api/sparql"))
        .bearer_auth(web.token())
        .json(&serde_json::json!({ "query": "SELECT ?s ?o WHERE { ?s ?p ?o }" }))
        .send()
        .await
        .expect("sparql select");
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["rows"][0][0], "<http://ex.org/n1>");
    assert_eq!(body["rows"][0][1], "\"hi\"");

    // POST /request: one framed Query in, framed Result/Row/Done out.
    let mut request = Request::new(RequestKind::Query, "sql-sqlite", "SELECT title FROM notes");
    request.params = vec![];
    let req_id = request.id_string();
    let r = client
        .post(format!("{base}/request"))
        .bearer_auth(web.token())
        .header("Content-Type", "application/rsntr-frames")
        .body(frame_bytes(&request.to_envelope()))
        .send()
        .await
        .expect("request");
    assert_eq!(r.status(), 200);
    let frames = decode_frames(&r.bytes().await.expect("body"));
    match &frames[0] {
        EnvelopeObject::Result(h) => {
            assert_eq!(h.id, req_id);
            assert_eq!(h.columns, vec!["title"]);
        }
        other => panic!("expected Result, got {other:?}"),
    }
    match &frames[1] {
        EnvelopeObject::Row(rows) => {
            assert_eq!(rows[0].cells[0].1, Value::Text("groceries".into()));
        }
        other => panic!("expected Row, got {other:?}"),
    }
    assert!(matches!(
        frames.last().expect("done"),
        EnvelopeObject::Done(_)
    ));

    // CSV round-trip: create-or-append import, byte-identical export.
    let doc = "name,note\r\nrex,\"a,b\"\r\nfido,\r\n";
    let r = client
        .post(format!("{base}/api/csv/pets"))
        .bearer_auth(web.token())
        .header("Content-Type", "text/csv")
        .body(doc)
        .send()
        .await
        .expect("csv import");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["created"], true);
    assert_eq!(body["rows_inserted"], 2);
    let r = client
        .get(format!("{base}/api/csv/pets"))
        .bearer_auth(web.token())
        .send()
        .await
        .expect("csv export");
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.expect("csv"), doc);

    web.shutdown().await;
    running.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn web_token_stable_across_restarts() {
    let tmp = TempDir::new("serve-web-token");
    store::init_dir(tmp.path()).expect("init");

    let running = serve::start_node(tmp.path(), true).await.expect("serve");
    let web = serve::start_web(
        &running,
        tmp.path(),
        "127.0.0.1:0".parse().expect("addr"),
        false,
    )
    .await
    .expect("web");
    let first = web.token().to_string();
    assert_eq!(first.len(), 43);
    web.shutdown().await;

    // The token file is a 0600 secret like rsntr.key.
    let path = tmp.path().join(store::WEB_TOKEN_FILE);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // Same directory, next run: same token, so signed-in browsers and
    // installed PWAs keep working.
    let web = serve::start_web(
        &running,
        tmp.path(),
        "127.0.0.1:0".parse().expect("addr"),
        false,
    )
    .await
    .expect("web again");
    assert_eq!(web.token(), first);
    web.shutdown().await;

    // Rotation mints a replacement.
    let web = serve::start_web(
        &running,
        tmp.path(),
        "127.0.0.1:0".parse().expect("addr"),
        true,
    )
    .await
    .expect("web rotated");
    assert_ne!(web.token(), first);
    assert_eq!(web.token().len(), 43);
    web.shutdown().await;

    // A clobbered token file is refused, not silently replaced.
    std::fs::write(&path, "not a token\n").expect("clobber");
    match serve::start_web(
        &running,
        tmp.path(),
        "127.0.0.1:0".parse().expect("addr"),
        false,
    )
    .await
    {
        Ok(_) => panic!("clobbered token file was accepted"),
        Err(err) => assert!(err.to_string().contains("does not hold a web token")),
    }

    running.shutdown().await;
}
