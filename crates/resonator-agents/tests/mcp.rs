//! The MCP endpoint against a real server.

mod common;

use common::*;
use serde_json::{Value, json};

const MODERN: &str = "2026-07-28";

struct Mcp<'a> {
    server: &'a Server,
    token: String,
}

impl Mcp<'_> {
    /// A legacy-style request (no `_meta` version).
    async fn legacy(&self, method: &str, params: Value) -> (u16, Value) {
        self.raw(
            &json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}),
            &[],
        )
        .await
    }

    /// A modern request with the right headers.
    async fn call(&self, method: &str, mut params: Value) -> (u16, Value) {
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": MODERN,
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "1"}
        });
        let name = params["name"].as_str().unwrap_or("").to_owned();
        let mut headers = vec![
            ("MCP-Protocol-Version", MODERN.to_owned()),
            ("Mcp-Method", method.to_owned()),
        ];
        if method == "tools/call" {
            headers.push(("Mcp-Name", name));
        }
        self.raw(
            &json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}),
            &headers,
        )
        .await
    }

    async fn raw(&self, body: &Value, headers: &[(&str, String)]) -> (u16, Value) {
        let mut request = self
            .server
            .http
            .post(self.server.url("/mcp"))
            .header("Accept", "application/json, text/event-stream")
            .json(body);
        if !self.token.is_empty() {
            request = request.bearer_auth(&self.token);
        }
        for (name, value) in headers {
            request = request.header(*name, value);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let text = response.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    /// Calls a tool; returns (is_error, text).
    async fn tool(&self, name: &str, arguments: Value) -> (bool, String) {
        let (status, body) = self
            .call("tools/call", json!({"name": name, "arguments": arguments}))
            .await;
        assert_eq!(status, 200, "{body}");
        let result = &body["result"];
        assert_eq!(result["resultType"], "complete", "{body}");
        (
            result["isError"].as_bool().unwrap(),
            result["content"][0]["text"].as_str().unwrap().to_owned(),
        )
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn protocol_conformance() {
    let server = Server::start().await;
    let token = server.token("alice", &["read", "write"]).await;
    let anonymous = Mcp {
        server: &server,
        token: String::new(),
    };
    let (status, _) = anonymous.legacy("ping", json!({})).await;
    assert_eq!(status, 401);
    let mcp = Mcp {
        server: &server,
        token,
    };

    // Legacy: initialize negotiates, ping answers, notifications get 202.
    let (status, body) = mcp
        .legacy(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                                     "clientInfo": {"name": "t", "version": "1"}}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(
        body["result"]["capabilities"]["tools"]["listChanged"],
        false
    );
    assert_eq!(body["result"]["serverInfo"]["name"], "resonator");
    let (_, body) = mcp
        .legacy("initialize", json!({"protocolVersion": "1999-01-01"}))
        .await;
    assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
    let (status, body) = mcp.legacy("ping", json!({})).await;
    assert_eq!((status, &body["result"]), (200, &json!({})));
    let (status, _) = mcp
        .raw(
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            &[],
        )
        .await;
    assert_eq!(status, 202);
    let (status, body) = mcp.legacy("tools/list", json!({})).await;
    assert_eq!(status, 200);
    assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 13);
    assert!(body["result"].get("resultType").is_none());

    // Modern: discover, list, header checks, versions, unknown methods.
    let (status, body) = mcp.call("server/discover", json!({})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["supportedVersions"][0], MODERN);
    assert_eq!(body["result"]["resultType"], "complete");
    assert_eq!(
        body["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "resonator"
    );
    let (status, body) = mcp.call("tools/list", json!({})).await;
    assert_eq!(status, 200);
    assert_eq!(body["result"]["cacheScope"], "private");
    let request = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {
        "_meta": {"io.modelcontextprotocol/protocolVersion": MODERN}}});
    let (status, body) = mcp.raw(&request, &[]).await;
    assert_eq!(
        (status, body["error"]["code"].as_i64()),
        (400, Some(-32020)),
        "{body}"
    );
    let (status, body) = mcp
        .raw(
            &request,
            &[
                ("MCP-Protocol-Version", MODERN.into()),
                ("Mcp-Method", "tools/call".into()),
            ],
        )
        .await;
    assert_eq!(
        (status, body["error"]["code"].as_i64()),
        (400, Some(-32020)),
        "{body}"
    );
    let old = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {
        "_meta": {"io.modelcontextprotocol/protocolVersion": "2030-01-01"}}});
    let (status, body) = mcp
        .raw(
            &old,
            &[
                ("MCP-Protocol-Version", "2030-01-01".into()),
                ("Mcp-Method", "tools/list".into()),
            ],
        )
        .await;
    assert_eq!(
        (status, body["error"]["code"].as_i64()),
        (400, Some(-32022)),
        "{body}"
    );
    assert_eq!(body["error"]["data"]["requested"], "2030-01-01");
    let (status, body) = mcp.call("resources/list", json!({})).await;
    assert_eq!(
        (status, body["error"]["code"].as_i64()),
        (404, Some(-32601)),
        "{body}"
    );
    let (status, _) = mcp.call("ping", json!({})).await;
    assert_eq!(status, 404);
    let (status, body) = mcp.call("tools/call", json!({"name": "nope"})).await;
    assert_eq!(body["error"]["code"], -32602, "{status} {body}");

    // GET and DELETE, foreign origins, batches.
    let get = server.http.get(server.url("/mcp")).send().await.unwrap();
    assert_eq!(get.status(), 405);
    let delete = server.http.delete(server.url("/mcp")).send().await.unwrap();
    assert_eq!(delete.status(), 405);
    let (status, _) = mcp
        .raw(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
            &[("Origin", "https://evil.example".into())],
        )
        .await;
    assert_eq!(status, 403);
    let (status, _) = mcp
        .raw(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
            &[("Origin", format!("http://{}", server.addr))],
        )
        .await;
    assert_eq!(status, 200);
    let (status, _) = mcp
        .raw(&json!([{"jsonrpc": "2.0", "id": 1, "method": "ping"}]), &[])
        .await;
    assert_eq!(status, 400);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_read_reply_and_edit_with_a_bearer_token() {
    let server = Server::start().await;
    let token = server.token("alice", &["read", "write"]).await;
    let mcp = Mcp {
        server: &server,
        token: token.clone(),
    };
    let (is_error, text) = mcp
        .tool(
            "create_wave",
            json!({"title": "Garden plans", "text": "Plant tomatoes in May.", "participants": ["bob@example.com"]}),
        )
        .await;
    assert!(!is_error, "{text}");
    let created: Value = serde_json::from_str(&text).unwrap();
    let wave = created["wave_id"].as_str().unwrap().to_owned();
    let root = created["root_blip_id"].as_str().unwrap().to_owned();

    // Search finds it (the index may lag a moment).
    server
        .wait_until("search finds the wave", || async {
            let (_, text) = mcp.tool("search_waves", json!({"query": "tomatoes"})).await;
            text.contains(&wave)
        })
        .await;
    let (_, text) = mcp.tool("read_wave", json!({"wave_id": wave})).await;
    assert!(text.contains("Title: Garden plans"), "{text}");
    assert!(
        text.contains(&format!("[{root}] alice@example.com"))
            && text.contains("Plant tomatoes in May."),
        "{text}"
    );

    let (is_error, text) = mcp
        .tool(
            "reply_to_blip",
            json!({"wave_id": wave, "blip_id": root, "text": "And basil."}),
        )
        .await;
    assert!(!is_error, "{text}");
    let reply: Value = serde_json::from_str(&text).unwrap();
    let reply_id = reply["blip_id"].as_str().unwrap().to_owned();
    let (is_error, text) = mcp
        .tool(
            "edit_blip",
            json!({"wave_id": wave, "blip_id": reply_id, "find": "basil", "replace_with": "mint"}),
        )
        .await;
    assert!(!is_error, "{text}");
    let (is_error, text) = mcp
        .tool(
            "edit_blip",
            json!({"wave_id": wave, "blip_id": reply_id, "find": "parsley", "replace_with": "x"}),
        )
        .await;
    assert!(is_error && text.contains("does not occur"), "{text}");
    let (is_error, text) = mcp
        .tool("inline_reply", json!({"wave_id": wave, "blip_id": root, "after_text": "tomatoes", "text": "Which kind?"}))
        .await;
    assert!(!is_error, "{text}");
    let (is_error, _) = mcp
        .tool("set_title", json!({"wave_id": wave, "title": "Garden"}))
        .await;
    assert!(!is_error);
    let (is_error, text) = mcp.tool("mark_read", json!({"wave_id": wave})).await;
    assert!(!is_error, "{text}");
    let (is_error, text) = mcp.tool("list_unread", json!({})).await;
    assert!(!is_error, "{text}");

    let (_, text) = mcp
        .tool("read_wave", json!({"wave_id": wave, "blip_id": reply_id}))
        .await;
    assert!(
        text.contains("And mint.") && !text.contains("Plant tomatoes"),
        "{text}"
    );
    let (_, text) = mcp.tool("read_wave", json!({"wave_id": wave})).await;
    assert!(
        text.contains("Which kind?") && text.contains("Title: Garden"),
        "{text}"
    );

    // Someone else's wave is out of reach.
    let bob_alone = server.token("carol", &["read", "write"]).await;
    let (other, _, _) = server
        .create_wave(&bob_alone, &["carol@example.com"], "private")
        .await;
    let (is_error, text) = mcp.tool("read_wave", json!({"wave_id": other})).await;
    assert!(is_error && text.contains("cannot read wave"), "{text}");
    let (is_error, text) = mcp.tool("read_wave", json!({"wave_id": "garbage"})).await;
    assert!(is_error && text.contains("not a wave id"), "{text}");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_only_token_cannot_write() {
    let server = Server::start().await;
    let cookie = server.user("dave").await;
    let writer = server.token_for(&cookie, &["read", "write"]).await;
    let reader = server.token_for(&cookie, &["read"]).await;
    let (wave, _, root) = server
        .create_wave(&writer, &["dave@example.com"], "Read me")
        .await;
    let mcp = Mcp {
        server: &server,
        token: reader,
    };
    let (_, body) = mcp.call("tools/list", json!({})).await;
    let names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["search_waves", "read_wave", "list_unread"]);
    let (is_error, text) = mcp.tool("read_wave", json!({"wave_id": wave})).await;
    assert!(!is_error && text.contains("Read me"), "{text}");
    let (is_error, text) = mcp
        .tool(
            "reply_to_blip",
            json!({"wave_id": wave, "blip_id": root, "text": "sneaky"}),
        )
        .await;
    assert!(is_error && text.contains("read scope"), "{text}");
    let blips = server
        .blips(&writer, &wave, &format!("{DOMAIN}/conv+root"))
        .await;
    assert_eq!(blips.len(), 1);
    server.stop().await;
}
