//! Integration tests: a real node behind a real bound server, driven
//! with reqwest over the loopback.

use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use tokio_stream::StreamExt;

use resonator_authenticator::Chain;
use resonator_node::{DbHandle, Node, NodeConfig, seed_rsntr_defaults};
use resonator_protocol::{
    EnvelopeObject, Request, RequestKind, Value, decode_frame_eof, encode_envelope,
};
use resonator_web::{WebConfig, WebServer, serve_web};

const OWNER: &str = "abababababababababababababababababababababababababababababababab";

async fn start() -> (WebServer, Arc<Node>) {
    let conn = resonator_node::open_node_db_in_memory().expect("open db");
    seed_rsntr_defaults(&conn).expect("seed _rsntr");
    let node = Arc::new(Node::new(
        DbHandle::spawn(conn),
        Chain::with_builtin_tiers(),
        NodeConfig::default(),
    ));
    node.db()
        .call(|conn| {
            conn.execute_batch(
                "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, body TEXT);
                 INSERT INTO notes (title, body) VALUES ('groceries', 'milk, eggs');
                 INSERT INTO notes (title, body) VALUES ('reading list', NULL);",
            )
            .expect("seed notes");
        })
        .await
        .expect("db call");
    let server = serve_web(
        node.clone(),
        OWNER,
        None,
        WebConfig {
            addr: "127.0.0.1:0".parse().expect("addr"),
            token: None,
        },
    )
    .await
    .expect("serve web");
    (server, node)
}

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
async fn token_auth_and_ui() {
    let (server, _node) = start().await;
    let base = format!("http://{}", server.addr());
    let client = reqwest::Client::new();

    // The UI shell needs no token; the fragment URL carries it.
    assert!(server.url().contains(&format!("/#{}", server.token())));
    let r = client.get(format!("{base}/")).send().await.expect("get /");
    assert_eq!(r.status(), 200);
    assert!(r.text().await.expect("body").contains("<html"));

    // Everything else answers 401 without the token...
    let r = client
        .get(format!("{base}/api/meta"))
        .send()
        .await
        .expect("meta");
    assert_eq!(r.status(), 401);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["error"]["code"], "unauthorized");
    let r = client
        .get(format!("{base}/api/meta"))
        .bearer_auth("wrong-token")
        .send()
        .await
        .expect("meta");
    assert_eq!(r.status(), 401);

    // ... and 200 with it, as Bearer or as the cookie.
    let r = client
        .get(format!("{base}/api/meta"))
        .bearer_auth(server.token())
        .send()
        .await
        .expect("meta");
    assert_eq!(r.status(), 200);
    let meta: serde_json::Value = r.json().await.expect("json");
    assert_eq!(meta["ok"], true);
    assert_eq!(meta["node_id"], OWNER);
    assert!(
        meta["mods"]
            .as_array()
            .expect("mods")
            .iter()
            .any(|m| m == "sql-sqlite")
    );
    assert!(
        meta["tables"]
            .as_array()
            .expect("tables")
            .iter()
            .any(|t| t["name"] == "notes" && t["reserved"] == false)
    );

    let r = client
        .get(format!("{base}/api/meta"))
        .header("Cookie", format!("other=1; rsntr_token={}", server.token()))
        .send()
        .await
        .expect("meta via cookie");
    assert_eq!(r.status(), 200);

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pwa_assets_unauthenticated() {
    let (server, _node) = start().await;
    let base = format!("http://{}", server.addr());
    let client = reqwest::Client::new();

    for (path, ctype) in [
        ("/manifest.webmanifest", "application/manifest+json"),
        ("/sw.js", "text/javascript; charset=utf-8"),
        ("/icon-maskable-512.png", "image/png"),
        ("/icon-192.png", "image/png"),
        ("/icon-512.png", "image/png"),
        ("/apple-touch-icon.png", "image/png"),
        ("/favicon.ico", "image/png"),
    ] {
        let r = client
            .get(format!("{base}{path}"))
            .send()
            .await
            .expect(path);
        assert_eq!(r.status(), 200, "{path}");
        assert_eq!(
            r.headers()
                .get("Content-Type")
                .expect("content-type")
                .to_str()
                .expect("ascii"),
            ctype,
            "{path}"
        );
    }

    // The exemption is method-gated: POSTing a public path is not public.
    let r = client
        .post(format!("{base}/sw.js"))
        .send()
        .await
        .expect("post sw.js");
    assert_eq!(r.status(), 401);

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn session_issues_persistent_cookie() {
    let (server, _node) = start().await;
    let base = format!("http://{}", server.addr());
    let client = reqwest::Client::new();

    // Reaching /api/session requires the token like any other route.
    let r = client
        .post(format!("{base}/api/session"))
        .send()
        .await
        .expect("bare session");
    assert_eq!(r.status(), 401);

    // With Bearer: 204 plus the persistent HttpOnly cookie.
    let r = client
        .post(format!("{base}/api/session"))
        .bearer_auth(server.token())
        .send()
        .await
        .expect("session");
    assert_eq!(r.status(), 204);
    let cookie = r
        .headers()
        .get("set-cookie")
        .expect("set-cookie")
        .to_str()
        .expect("ascii")
        .to_string();
    assert!(cookie.contains(&format!("rsntr_token={}", server.token())));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));
    assert!(cookie.contains("Path=/"));
    assert!(cookie.contains("Max-Age="));

    // The issued cookie alone authenticates.
    let pair = cookie.split(';').next().expect("pair").to_string();
    let r = client
        .get(format!("{base}/api/meta"))
        .header("Cookie", pair)
        .send()
        .await
        .expect("meta via issued cookie");
    assert_eq!(r.status(), 200);

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_refreshes_cookie_only_when_authed() {
    let (server, _node) = start().await;
    let base = format!("http://{}", server.addr());
    let client = reqwest::Client::new();

    // A bare GET / never receives a cookie...
    let r = client.get(format!("{base}/")).send().await.expect("get /");
    assert_eq!(r.status(), 200);
    assert!(r.headers().get("set-cookie").is_none());

    // ... nor does one presenting a wrong token ...
    let r = client
        .get(format!("{base}/"))
        .header("Cookie", "rsntr_token=wrong")
        .send()
        .await
        .expect("get / wrong cookie");
    assert_eq!(r.status(), 200);
    assert!(r.headers().get("set-cookie").is_none());

    // ... while the valid token gets its cookie refreshed.
    let r = client
        .get(format!("{base}/"))
        .header("Cookie", format!("rsntr_token={}", server.token()))
        .send()
        .await
        .expect("get / valid cookie");
    assert_eq!(r.status(), 200);
    let cookie = r
        .headers()
        .get("set-cookie")
        .expect("set-cookie")
        .to_str()
        .expect("ascii");
    assert!(cookie.starts_with(&format!("rsntr_token={}", server.token())));

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn request_sql_round_trip_through_pipeline() {
    let (server, node) = start().await;
    let base = format!("http://{}", server.addr());
    let client = reqwest::Client::new();

    let mut request = Request::new(
        RequestKind::Query,
        "sql-sqlite",
        "SELECT title FROM notes WHERE title = ?",
    );
    request.params = vec![Value::Text("groceries".into())];
    let id = request.id_string();

    let r = client
        .post(format!("{base}/request"))
        .bearer_auth(server.token())
        .header("Content-Type", "application/rsntr-frames")
        .body(frame_bytes(&request.to_envelope()))
        .send()
        .await
        .expect("request");
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.headers().get("Content-Type").expect("ct"),
        "application/rsntr-frames"
    );
    let frames = decode_frames(&r.bytes().await.expect("body"));
    assert!(frames.len() >= 3, "got {frames:?}");
    match &frames[0] {
        EnvelopeObject::Result(h) => {
            assert_eq!(h.id, id);
            assert_eq!(h.columns, vec!["title"]);
        }
        other => panic!("expected Result, got {other:?}"),
    }
    match &frames[1] {
        EnvelopeObject::Row(rows) => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].cells[0].1, Value::Text("groceries".into()));
        }
        other => panic!("expected Row, got {other:?}"),
    }
    match frames.last().expect("done") {
        EnvelopeObject::Done(d) => assert_eq!(d.row_count, Some(1)),
        other => panic!("expected Done, got {other:?}"),
    }

    // The pipeline audited the request (nothing bypasses it).
    let audited: i64 = node
        .db()
        .call(move |conn| {
            conn.query_row(
                "SELECT count(*) FROM _audit WHERE request_id = ?1 AND decision = 'allow'",
                [&id],
                |r| r.get(0),
            )
            .expect("audit count")
        })
        .await
        .expect("db");
    assert_eq!(audited, 1);

    // A policy deny for the owner denies the browser: 403 + Denied frame.
    node.db()
        .call(|conn| {
            conn.execute(
                "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
                 VALUES (?1, 'notes', 'read', 'deny')",
                [OWNER],
            )
            .expect("deny row");
        })
        .await
        .expect("db");
    let denied = Request::new(RequestKind::Query, "sql-sqlite", "SELECT title FROM notes");
    let r = client
        .post(format!("{base}/request"))
        .bearer_auth(server.token())
        .header("Content-Type", "application/rsntr-frames")
        .body(frame_bytes(&denied.to_envelope()))
        .send()
        .await
        .expect("denied request");
    assert_eq!(r.status(), 403);
    let frames = decode_frames(&r.bytes().await.expect("body"));
    assert!(
        matches!(frames.as_slice(), [EnvelopeObject::Denied(_)]),
        "got {frames:?}"
    );

    // Wrong content type and a non-request class are refused.
    let r = client
        .post(format!("{base}/request"))
        .bearer_auth(server.token())
        .header("Content-Type", "text/plain")
        .body("hi")
        .send()
        .await
        .expect("wrong ct");
    assert_eq!(r.status(), 415);

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn feed_announces_projection_changes() {
    let (server, node) = start().await;
    let base = format!("http://{}", server.addr());
    let client = reqwest::Client::new();

    let r = client
        .get(format!("{base}/feed"))
        .bearer_auth(server.token())
        .send()
        .await
        .expect("feed");
    assert_eq!(r.status(), 200);
    assert!(
        r.headers()
            .get("Content-Type")
            .expect("ct")
            .to_str()
            .expect("str")
            .starts_with("text/event-stream")
    );
    let mut stream = r.bytes_stream();

    // The first bytes carry the retry hint; then a projection change
    // must arrive as an `envelope` event.
    let mut text = String::new();
    let first = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("first chunk in time")
        .expect("stream open")
        .expect("chunk");
    text.push_str(&String::from_utf8_lossy(&first));
    assert!(text.contains("retry: 2000"), "got {text:?}");

    node.db()
        .call(|conn| {
            conn.execute(
                "INSERT INTO _projection (point_iri, kind, label) \
                 VALUES ('urn:test:point', 'radiant', 'test')",
                [],
            )
            .expect("projection row");
        })
        .await
        .expect("db");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !text.contains("event: envelope") {
        let chunk = tokio::time::timeout_at(deadline, stream.next())
            .await
            .expect("envelope event in time")
            .expect("stream open")
            .expect("chunk");
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(
        text.contains("rsntr:Vibration") && text.contains("urn:rsntr:projection-changed"),
        "got {text:?}"
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn entrain_streams_vibrations() {
    let (server, node) = start().await;
    let base = format!("http://{}", server.addr());
    let client = reqwest::Client::new();

    // The server's feed republisher holds one permanent subscription;
    // the abort check below compares against this baseline.
    let baseline = node.entrainments().active();
    let entrain = EnvelopeObject::Entrain(resonator_protocol::Entrain {
        id: ulid::Ulid::new().to_string(),
        point: "urn:rsntr:projection-changed".to_string(),
    });
    let r = client
        .post(format!("{base}/entrain"))
        .bearer_auth(server.token())
        .header("Content-Type", "application/rsntr-frames")
        .body(frame_bytes(&entrain))
        .send()
        .await
        .expect("entrain");
    assert_eq!(r.status(), 200);
    let mut stream = r.bytes_stream();

    let mut buf = BytesMut::new();
    let mut frames: Vec<EnvelopeObject> = Vec::new();
    let mut poked = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while frames.len() < 2 {
        // After the entrained ack, poke a change so a vibration fires.
        if !poked && !frames.is_empty() {
            poked = true;
            node.db()
                .call(|conn| {
                    conn.execute(
                        "INSERT INTO _projection (point_iri, kind) \
                         VALUES ('urn:test:tick', 'sympathetic')",
                        [],
                    )
                    .expect("tick row");
                })
                .await
                .expect("db");
        }
        let chunk = tokio::time::timeout_at(deadline, stream.next())
            .await
            .expect("frames in time")
            .expect("stream open")
            .expect("chunk");
        buf.extend_from_slice(&chunk);
        while let Some(doc) = resonator_protocol::decode_frame(&mut buf).expect("frame") {
            frames.push(EnvelopeObject::from_turtle(&doc).expect("envelope"));
        }
    }
    assert!(
        matches!(frames[0], EnvelopeObject::Done(_)),
        "expected the entrained ack, got {:?}",
        frames[0]
    );
    match &frames[1] {
        EnvelopeObject::Vibration(v) => {
            assert_eq!(v.point, "urn:rsntr:projection-changed");
            assert_eq!(v.seq, 0);
        }
        other => panic!("expected a Vibration, got {other:?}"),
    }

    // Dropping the response is the damp; the registry empties.
    drop(stream);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        // The server notices on its next (failed) send.
        node.db()
            .call(|conn| {
                conn.execute(
                    "UPDATE _projection SET label = 'poke' WHERE point_iri = 'urn:test:tick'",
                    [],
                )
                .expect("poke");
            })
            .await
            .expect("db");
        if node.entrainments().active() == baseline {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "entrainment did not end after the client aborted"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn api_table_read_and_edit() {
    let (server, _node) = start().await;
    let base = format!("http://{}", server.addr());
    let client = reqwest::Client::new();

    let r = client
        .get(format!("{base}/api/table/notes?limit=10"))
        .bearer_auth(server.token())
        .send()
        .await
        .expect("table");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["ok"], true);
    assert_eq!(body["total"], 2);
    assert_eq!(body["columns"][1]["name"], "title");
    assert_eq!(body["columns"][0]["pk"], true);
    assert_eq!(body["rows"][0][1], "groceries");
    // The NULL body cell is restored in column order.
    assert_eq!(body["rows"][1][2], serde_json::Value::Null);
    assert_eq!(body["rowids"][0], 1);

    // Insert through the editor verb.
    let r = client
        .post(format!("{base}/api/table/notes/rows"))
        .bearer_auth(server.token())
        .json(&serde_json::json!({ "values": { "title": "third", "body": null } }))
        .send()
        .await
        .expect("insert");
    assert_eq!(r.status(), 201);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["last_insert_rowid"], 3);

    // Update it, then delete it, by rowid.
    let r = client
        .patch(format!("{base}/api/table/notes/rows/3"))
        .bearer_auth(server.token())
        .json(&serde_json::json!({ "values": { "body": "filled in" } }))
        .send()
        .await
        .expect("update");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["affected_rows"], 1);
    let r = client
        .delete(format!("{base}/api/table/notes/rows/3"))
        .bearer_auth(server.token())
        .send()
        .await
        .expect("delete");
    assert_eq!(r.status(), 200);
    let r = client
        .delete(format!("{base}/api/table/notes/rows/99"))
        .bearer_auth(server.token())
        .send()
        .await
        .expect("delete missing");
    assert_eq!(r.status(), 404);
    let r = client
        .get(format!("{base}/api/table/nope"))
        .bearer_auth(server.token())
        .send()
        .await
        .expect("missing table");
    assert_eq!(r.status(), 404);

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn api_sql_and_sparql() {
    let (server, _node) = start().await;
    let base = format!("http://{}", server.addr());
    let client = reqwest::Client::new();

    let r = client
        .post(format!("{base}/api/sql"))
        .bearer_auth(server.token())
        .json(&serde_json::json!({
            "sql": "SELECT title FROM notes WHERE title = ?",
            "params": ["groceries"],
        }))
        .send()
        .await
        .expect("sql");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["ok"], true);
    assert_eq!(body["columns"], serde_json::json!(["title"]));
    assert_eq!(body["rows"], serde_json::json!([["groceries"]]));
    assert_eq!(body["row_count"], 1);

    // Update into the store, then read it back: cells are N-Triples
    // lexical text end to end.
    let r = client
        .post(format!("{base}/api/sparql"))
        .bearer_auth(server.token())
        .json(&serde_json::json!({
            "query": "INSERT DATA { <http://ex.org/n1> <http://ex.org/title> \"hi\" }",
        }))
        .send()
        .await
        .expect("sparql update");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["ok"], true);
    assert_eq!(body["affected_rows"], 1);

    let r = client
        .post(format!("{base}/api/sparql"))
        .bearer_auth(server.token())
        .json(&serde_json::json!({ "query": "SELECT ?s ?o WHERE { ?s ?p ?o }" }))
        .send()
        .await
        .expect("sparql select");
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["ok"], true);
    assert_eq!(body["rows"][0][0], "<http://ex.org/n1>");
    assert_eq!(body["rows"][0][1], "\"hi\"");

    // Turtle load, gated and counted.
    let r = client
        .post(format!("{base}/api/turtle"))
        .bearer_auth(server.token())
        .header("Content-Type", "text/turtle")
        .body("<http://ex.org/n2> <http://ex.org/title> \"two\", \"zwei\" .")
        .send()
        .await
        .expect("turtle");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["ok"], true);
    assert_eq!(body["triple_count"], 2);

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn csv_round_trip() {
    let (server, _node) = start().await;
    let base = format!("http://{}", server.addr());
    let client = reqwest::Client::new();

    // Create-or-append: the table does not exist yet.
    let doc = "name,note,data\r\nrex,\"a,b\",x'0a0b'\r\nfido,,\"\"\r\n";
    let r = client
        .post(format!("{base}/api/csv/pets"))
        .bearer_auth(server.token())
        .header("Content-Type", "text/csv")
        .body(doc)
        .send()
        .await
        .expect("import");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(
        body,
        serde_json::json!({ "ok": true, "table": "pets", "created": true, "rows_inserted": 2 })
    );

    // Export is byte-compatible with the import.
    let r = client
        .get(format!("{base}/api/csv/pets"))
        .bearer_auth(server.token())
        .send()
        .await
        .expect("export");
    assert_eq!(r.status(), 200);
    assert!(
        r.headers()
            .get("Content-Disposition")
            .expect("cd")
            .to_str()
            .expect("str")
            .contains("pets.csv")
    );
    let text = r.text().await.expect("csv");
    assert_eq!(text, doc);

    // Appending again doubles the rows; a bad header is refused.
    let r = client
        .post(format!("{base}/api/csv/pets"))
        .bearer_auth(server.token())
        .header("Content-Type", "text/csv")
        .body("data,name,note\r\n,third,\r\n")
        .send()
        .await
        .expect("append");
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["created"], false);
    assert_eq!(body["rows_inserted"], 1);
    let r = client
        .post(format!("{base}/api/csv/pets"))
        .bearer_auth(server.token())
        .header("Content-Type", "text/csv")
        .body("name,wrong\r\na,b\r\n")
        .send()
        .await
        .expect("bad header");
    assert_eq!(r.status(), 400);

    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// The audio-duplex upstream: POST /duplex/{id}
// ---------------------------------------------------------------------------

/// Full local round trip through a `cat` echo source: open the exchange,
/// read the AudioDuplex header off the streaming body, POST upstream
/// bytes, read them back downstream, Fin, and confirm the exchange is
/// reaped (a late POST answers 404).
#[tokio::test(flavor = "multi_thread")]
async fn duplex_local_round_trip() {
    let (server, node) = start().await;
    let base = format!("http://{}", server.addr());
    let client = reqwest::Client::new();

    node.db()
        .call(|conn| {
            conn.execute_batch(&format!(
                "INSERT INTO _media (name, command, content_type, accepts) VALUES
                   ('echo', 'cat', 'application/octet-stream', 'application/octet-stream');
                 INSERT INTO _policy (peer_or_group, table_name, action, effect) VALUES
                   ('{OWNER}', 'echo', 'audio-duplex', 'allow');"
            ))
            .expect("seed duplex source");
        })
        .await
        .expect("db call");

    let request = Request::new(RequestKind::Query, "audio-duplex", "echo");
    let id = request.id_string();

    let r = client
        .post(format!("{base}/request"))
        .bearer_auth(server.token())
        .header("Content-Type", "application/rsntr-frames")
        .body(frame_bytes(&request.to_envelope()))
        .send()
        .await
        .expect("request");
    assert_eq!(r.status(), 200);
    let mut body = r.bytes_stream();

    // The header frame arrives first; anything after it is raw bytes.
    let mut buf = BytesMut::new();
    let header = loop {
        if let Some(doc) =
            resonator_protocol::decode_envelope(&mut buf).expect("well-formed stream")
        {
            break doc;
        }
        let chunk = body
            .next()
            .await
            .expect("body ended before the header")
            .expect("body read");
        buf.extend_from_slice(&chunk);
    };
    match &header {
        EnvelopeObject::AudioDuplex(d) => {
            assert_eq!(d.id, id);
            assert_eq!(d.accepts, "application/octet-stream");
        }
        other => panic!("expected AudioDuplex, got {other:?}"),
    }

    // Upstream bytes in two serialized POSTs.
    for span in ["ring ", "ring"] {
        let r = client
            .post(format!("{base}/duplex/{id}"))
            .bearer_auth(server.token())
            .header("Content-Type", "application/octet-stream")
            .body(span.as_bytes().to_vec())
            .send()
            .await
            .expect("duplex post");
        assert_eq!(r.status(), 204, "{span:?}");
    }

    // The echo comes back downstream.
    let mut got: Vec<u8> = buf.to_vec();
    while got.len() < b"ring ring".len() {
        let chunk = tokio::time::timeout(Duration::from_secs(10), body.next())
            .await
            .expect("echo timed out")
            .expect("body ended early")
            .expect("body read");
        got.extend_from_slice(&chunk);
    }
    assert_eq!(got, b"ring ring");

    // Fin: cat sees stdin EOF, exits, the body ends.
    let r = client
        .post(format!("{base}/duplex/{id}"))
        .bearer_auth(server.token())
        .header("Rsntr-Fin", "1")
        .body(Vec::new())
        .send()
        .await
        .expect("fin post");
    assert_eq!(r.status(), 204);
    let tail = tokio::time::timeout(Duration::from_secs(10), async {
        let mut extra = Vec::new();
        while let Some(chunk) = body.next().await {
            extra.extend_from_slice(&chunk.expect("body read"));
        }
        extra
    })
    .await
    .expect("body end timed out");
    assert!(tail.is_empty(), "unexpected trailing bytes: {tail:?}");

    // The exchange is reaped: a late POST finds nothing.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let r = client
        .post(format!("{base}/duplex/{id}"))
        .bearer_auth(server.token())
        .body(vec![1u8])
        .send()
        .await
        .expect("late post");
    assert_eq!(r.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn duplex_unknown_id_404() {
    let (server, _node) = start().await;
    let base = format!("http://{}", server.addr());
    let client = reqwest::Client::new();
    let r = client
        .post(format!("{base}/duplex/01K1N0SUCHEXCHANGE00000001"))
        .bearer_auth(server.token())
        .body(vec![0u8; 4])
        .send()
        .await
        .expect("post");
    assert_eq!(r.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn duplex_unauthorized_401() {
    let (server, _node) = start().await;
    let base = format!("http://{}", server.addr());
    let client = reqwest::Client::new();
    let r = client
        .post(format!("{base}/duplex/whatever"))
        .body(vec![0u8; 4])
        .send()
        .await
        .expect("post");
    assert_eq!(r.status(), 401);
}

/// A talk sink (a source that emits nothing downstream) must still end
/// when the caller drops the response: without watching the body, the
/// relay would wait forever for bytes that never come and the source
/// process would outlive the call.
#[tokio::test(flavor = "multi_thread")]
async fn duplex_silent_source_ends_when_the_caller_leaves() {
    let (server, node) = start().await;
    let base = format!("http://{}", server.addr());
    let client = reqwest::Client::new();

    // `tee` keeps the marker path in its argv, so the source's liveness
    // is observable with pgrep: a marker FILE would survive the SIGTERM
    // that reaps the process and prove nothing.
    let marker = std::env::temp_dir().join(format!("rsntr-duplex-sink-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let command = format!("tee {m} > /dev/null", m = marker.display());
    node.db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO _media (name, command, content_type, accepts) \
                 VALUES ('sink', ?1, '', 'audio/L16;rate=8000;channels=1')",
                [command],
            )
            .expect("seed sink");
            conn.execute(
                &format!(
                    "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
                     VALUES ('{OWNER}', 'sink', 'audio-duplex', 'allow')"
                ),
                [],
            )
            .expect("policy");
        })
        .await
        .expect("db call");

    let request = Request::new(RequestKind::Query, "audio-duplex", "sink");
    let r = client
        .post(format!("{base}/request"))
        .bearer_auth(server.token())
        .header("Content-Type", "application/rsntr-frames")
        .body(frame_bytes(&request.to_envelope()))
        .send()
        .await
        .expect("request");
    assert_eq!(r.status(), 200);

    // Read the header, then walk away: dropping the response is the hangup.
    let mut body = r.bytes_stream();
    let chunk = tokio::time::timeout(Duration::from_secs(10), body.next())
        .await
        .expect("header timed out")
        .expect("body ended")
        .expect("read");
    let mut buf = BytesMut::from(&chunk[..]);
    let header = resonator_protocol::decode_envelope(&mut buf)
        .expect("frame")
        .expect("header");
    assert!(
        matches!(header, EnvelopeObject::AudioDuplex(_)),
        "{header:?}"
    );
    let alive = || {
        std::process::Command::new("pgrep")
            .args(["-f", &marker.display().to_string()])
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false)
    };
    for _ in 0..40 {
        if alive() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(alive(), "the source never started");
    drop(body);

    // The source is reaped without anyone sending Fin.
    let mut gone = false;
    for _ in 0..60 {
        if !alive() {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _ = std::fs::remove_file(&marker);
    assert!(gone, "the silent source outlived the caller");
}

/// GET /api/peer/{id}: the local `_peers` row round-trips; with no
/// transport the live probe is null; unknown peers 404; malformed ids
/// 400; the auth middleware covers the route.
#[tokio::test(flavor = "multi_thread")]
async fn api_peer_row_probe_and_errors() {
    let (server, node) = start().await;
    let base = format!("http://{}", server.addr());
    let client = reqwest::Client::new();
    let hex = "12".repeat(32);
    {
        let hex = hex.clone();
        node.db()
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO _peers (endpoint_id, name, addrs, added_at, last_seen, notes) \
                     VALUES (?1, 'alice', '[\"10.0.0.7:41641\"]', '2026-08-01 10:00:00', \
                             NULL, 'front desk')",
                    [hex],
                )
                .expect("seed peer");
            })
            .await
            .expect("db call");
    }

    // Unauthenticated: 401.
    let r = client
        .get(format!("{base}/api/peer/{hex}"))
        .send()
        .await
        .expect("get");
    assert_eq!(r.status(), 401);

    // The row round-trips; live is null (this server has no transport).
    let r = client
        .get(format!("{base}/api/peer/{hex}"))
        .bearer_auth(server.token())
        .send()
        .await
        .expect("get");
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["ok"], true);
    assert_eq!(body["peer"]["name"], "alice");
    assert_eq!(body["peer"]["addrs"][0], "10.0.0.7:41641");
    assert_eq!(body["peer"]["notes"], "front desk");
    assert!(body["peer"]["last_seen"].is_null());
    assert!(body["live"].is_null());

    // Unknown peer: 404 not-found.
    let other = "cd".repeat(32);
    let r = client
        .get(format!("{base}/api/peer/{other}"))
        .bearer_auth(server.token())
        .send()
        .await
        .expect("get");
    assert_eq!(r.status(), 404);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["error"]["code"], "not-found");

    // Malformed id: 400.
    let r = client
        .get(format!("{base}/api/peer/nothex"))
        .bearer_auth(server.token())
        .send()
        .await
        .expect("get");
    assert_eq!(r.status(), 400);

    server.shutdown().await;
}

/// Deep links: any GET under /o/ serves the data-free shell without a
/// token (the client router reads the path); the exemption is
/// method-gated, and the prefix does not leak other routes.
#[tokio::test(flavor = "multi_thread")]
async fn deep_link_paths_serve_the_shell() {
    let (server, _node) = start().await;
    let base = format!("http://{}", server.addr());
    let client = reqwest::Client::new();

    for path in ["/o/local/proj", "/o/abcd/holo/cameras", "/o/x/proj/urn%3Ax%3Aadmin"] {
        let r = client.get(format!("{base}{path}")).send().await.expect(path);
        assert_eq!(r.status(), 200, "{path}");
        assert!(r.text().await.expect("body").contains("<html"), "{path}");
    }

    // Method-gated: POSTing a deep-link path is not public.
    let r = client
        .post(format!("{base}/o/local/proj"))
        .send()
        .await
        .expect("post");
    assert_eq!(r.status(), 401);

    // The prefix exempts only /o/...; sibling routes still authenticate.
    let r = client
        .get(format!("{base}/api/meta"))
        .send()
        .await
        .expect("meta");
    assert_eq!(r.status(), 401);

    server.shutdown().await;
}
