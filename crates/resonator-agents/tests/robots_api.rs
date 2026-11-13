//! The Data API, `/robot/rpc`, robot registration, webhook delivery and
//! the robot event stream, against a real server.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use common::*;
use futures_util::StreamExt;
use resonator_agents::robots::sign;
use serde_json::{Value, json};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn data_api_with_a_personal_access_token() {
    let server = Server::start().await;
    let token = server.token("alice", &["read", "write"]).await;
    let (wave, wavelet, root) = server
        .create_wave(&token, &["alice@example.com"], "hello world")
        .await;
    let (status, body) = server
        .data_api(
            &token,
            json!([{"id": "a", "method": "wavelet.appendBlip", "params": {
                "waveId": wave, "waveletId": wavelet,
                "blipData": {"waveId": wave, "waveletId": wavelet, "blipId": "TBD_1", "content": "\nsecond"}
            }}]),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert!(body[0]["data"]["newBlipId"].is_string(), "{body}");
    let blips = server.blips(&token, &wave, &wavelet).await;
    assert_eq!(blips.len(), 2, "{blips:?}");
    let root_blip = blips.iter().find(|b| b.0 == root).unwrap();
    assert_eq!(root_blip.1, "alice@example.com");
    assert!(root_blip.2.contains("hello world"), "{blips:?}");
    assert!(blips.iter().any(|b| b.2.contains("second")));

    // The /rpc alias works the same.
    let (status, _) = server
        .post_json("/robot/dataapi/rpc", &token, &json!([]))
        .await;
    assert_eq!(status, 200);

    // Someone else cannot read alice's wave.
    let bob = server.token("bob", &["read", "write"]).await;
    let fetched = server.fetch(&bob, &wave, &wavelet).await;
    assert!(fetched.get("error").is_some(), "{fetched}");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scopes_authentication_and_bad_requests() {
    let server = Server::start().await;
    let read_only = server.token("carol", &["read"]).await;
    let (status, body) = server.data_api(&read_only, json!([])).await;
    assert_eq!(status, 403, "{body}");
    let response = server
        .http
        .post(server.url("/robot/dataapi"))
        .json(&json!([]))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let (status, _) = server.data_api("wn_pat_nonsense", json!([])).await;
    assert_eq!(status, 401);
    let token = server.token("dave", &["read", "write"]).await;
    let (status, body) = server
        .post_json("/robot/dataapi", &token, &json!({"no": "operations"}))
        .await;
    assert_eq!(status, 400, "{body}");
    // People may not act as robot proxies.
    let (status, body) = server
        .data_api(
            &token,
            json!([{"id": "p", "method": "robot.fetchWave",
                    "params": {"waveId": "example.com/w+x", "waveletId": "example.com/conv+root",
                               "proxyingFor": "someone"}}]),
        )
        .await;
    assert_eq!(status, 403, "{body}");
    // /robot/rpc is for robots only.
    let (status, _) = server.post_json("/robot/rpc", &token, &json!([])).await;
    assert_eq!(status, 403);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn robot_registration_lifecycle_and_ownership() {
    let server = Server::start_with(|c| c.robot_max_per_user = 2).await;
    let alice = server.token("alice", &["read", "write"]).await;
    let bob = server.token("bob", &["read", "write"]).await;
    for bad in ["Bad", "a+b", "a#b", "a b", ""] {
        let (status, _) = server
            .post_json("/robot/register", &alice, &json!({"name": bad}))
            .await;
        assert_eq!(status, 400, "{bad}");
    }
    let (status, created) = server
        .post_json("/robot/register", &alice, &json!({"name": "streamer"}))
        .await;
    assert_eq!(status, 201, "{created}");
    assert_eq!(created["address"], "streamer@example.com");
    let robot_token = created["token"].as_str().unwrap().to_owned();
    assert!(robot_token.starts_with("wn_bot_"));
    assert!(
        created["webhook_secret"]
            .as_str()
            .unwrap()
            .starts_with("wn_whsec_")
    );
    let (status, _) = server
        .post_json("/robot/register", &bob, &json!({"name": "streamer"}))
        .await;
    assert_eq!(status, 409);
    // The address of a person is taken too.
    let (status, _) = server
        .post_json("/robot/register", &bob, &json!({"name": "alice"}))
        .await;
    assert_eq!(status, 409);

    // Listing shows only one's own, and never secrets.
    let list: Value = server
        .http
        .get(server.url("/robot/register"))
        .bearer_auth(&alice)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["robots"].as_array().unwrap().len(), 1);
    assert!(!list.to_string().contains("wn_"), "{list}");
    let list: Value = server
        .http
        .get(server.url("/robot/register"))
        .bearer_auth(&bob)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["robots"].as_array().unwrap().len(), 0);

    // Bob cannot touch alice's robot.
    let response = server
        .http
        .delete(server.url("/robot/register/streamer@example.com"))
        .bearer_auth(&bob)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    let (status, _) = server
        .post_json(
            "/robot/register/streamer@example.com/token",
            &bob,
            &json!({}),
        )
        .await;
    assert_eq!(status, 404);

    // The robot token works on /robot/rpc; rotating it ends the old one.
    let (status, _) = server
        .post_json("/robot/rpc", &robot_token, &json!([]))
        .await;
    assert_eq!(status, 200);
    let (status, rotated) = server
        .post_json(
            "/robot/register/streamer@example.com/token",
            &alice,
            &json!({}),
        )
        .await;
    assert_eq!(status, 200);
    let (status, _) = server
        .post_json("/robot/rpc", &robot_token, &json!([]))
        .await;
    assert_eq!(status, 401);
    let new_token = rotated["token"].as_str().unwrap();
    let (status, _) = server.post_json("/robot/rpc", new_token, &json!([])).await;
    assert_eq!(status, 200);
    // Robots cannot register robots.
    let (status, _) = server
        .post_json("/robot/register", new_token, &json!({"name": "child"}))
        .await;
    assert_eq!(status, 403);

    // A URL in the server's own network is refused by default.
    let (status, body) = server
        .post_json(
            "/robot/register",
            &alice,
            &json!({"name": "local", "url": "http://127.0.0.1:9/bot"}),
        )
        .await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("refused"),
        "{body}"
    );
    let response = server
        .http
        .put(server.url("/robot/register/streamer@example.com"))
        .bearer_auth(&alice)
        .json(&json!({"url": "http://169.254.169.254/latest"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);

    // The per-user limit.
    let (status, _) = server
        .post_json("/robot/register", &alice, &json!({"name": "second"}))
        .await;
    assert_eq!(status, 201);
    let (status, _) = server
        .post_json("/robot/register", &alice, &json!({"name": "third"}))
        .await;
    assert_eq!(status, 403);

    // Deleting.
    let response = server
        .http
        .delete(server.url("/robot/register/streamer@example.com"))
        .bearer_auth(&alice)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    let (status, _) = server.post_json("/robot/rpc", new_token, &json!([])).await;
    assert_eq!(status, 401);
    server.stop().await;
}

/// A robot at a URL: serves capabilities, records signed deliveries and
/// answers each with a blip.
struct FakeRobot {
    deliveries: Mutex<Vec<(HeaderMap, Bytes)>>,
}

async fn start_fake_robot() -> (String, Arc<FakeRobot>, tokio::task::JoinHandle<()>) {
    let robot = Arc::new(FakeRobot {
        deliveries: Mutex::new(Vec::new()),
    });
    let capabilities = "<w:robot xmlns:w=\"http://wave.google.com/extensions/robots/1.0\">\
        <w:version>1</w:version><w:protocolversion>0.22</w:protocolversion><w:capabilities>\
        <w:capability name=\"WAVELET_SELF_ADDED\"/></w:capabilities></w:robot>";
    let state = Arc::clone(&robot);
    let app = Router::new()
        .route(
            "/bot/_wave/capabilities.xml",
            get(move || async move { capabilities }),
        )
        .route(
            "/bot/_wave/robot/jsonrpc",
            post(move |headers: HeaderMap, body: Bytes| {
                let state = Arc::clone(&state);
                async move {
                    let bundle: Value = serde_json::from_slice(&body).unwrap_or_default();
                    state.deliveries.lock().unwrap().push((headers, body));
                    let wave = bundle["wavelet"]["waveId"].clone();
                    let wavelet = bundle["wavelet"]["waveletId"].clone();
                    json!([{"id": "op1", "method": "wavelet.appendBlip", "params": {
                        "waveId": wave, "waveletId": wavelet,
                        "blipData": {"waveId": wave, "waveletId": wavelet, "blipId": "TBD_r",
                                     "content": "\nhello from the webhook robot"}}}])
                    .to_string()
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/bot", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (url, robot, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn webhook_delivery_is_signed_and_answers_are_applied() {
    let (url, robot, task) = start_fake_robot().await;
    let server =
        Server::start_with(|c| c.robot_allowed_addresses = vec!["127.0.0.1".to_owned()]).await;
    let alice = server.token("alice", &["read", "write"]).await;
    let (status, created) = server
        .post_json(
            "/robot/register",
            &alice,
            &json!({"name": "hook", "url": url}),
        )
        .await;
    assert_eq!(status, 201, "{created}");
    assert!(created.get("capabilities_error").is_none(), "{created}");
    assert_eq!(created["events"], json!(["WAVELET_SELF_ADDED"]));
    let secret = created["webhook_secret"].as_str().unwrap().to_owned();
    let (wave, wavelet, _) = server
        .create_wave(
            &alice,
            &["alice@example.com", "hook@example.com"],
            "hi robot",
        )
        .await;
    server
        .wait_until("the robot's answer", || async {
            server
                .blips(&alice, &wave, &wavelet)
                .await
                .iter()
                .any(|b| b.1 == "hook@example.com" && b.2.contains("webhook robot"))
        })
        .await;
    let deliveries = robot.deliveries.lock().unwrap().clone();
    assert_eq!(deliveries.len(), 1);
    let (headers, body) = &deliveries[0];
    assert_eq!(headers[sign::ROBOT_HEADER], "hook@example.com");
    let timestamp: i64 = headers[sign::TIMESTAMP_HEADER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    let signature = headers[sign::SIGNATURE_HEADER].to_str().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!(sign::verify(&secret, timestamp, body, signature, now, 300));
    assert!(!sign::verify(
        "wn_whsec_wrong",
        timestamp,
        body,
        signature,
        now,
        300
    ));
    server.stop().await;
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn webhooks_to_private_addresses_are_refused_by_default() {
    let (url, robot, task) = start_fake_robot().await;
    let server = Server::start().await;
    let alice = server.token("alice", &["read", "write"]).await;
    let (status, _) = server
        .post_json(
            "/robot/register",
            &alice,
            &json!({"name": "hook", "url": url}),
        )
        .await;
    assert_eq!(status, 400);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(robot.deliveries.lock().unwrap().is_empty());
    server.stop().await;
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn robots_without_url_listen_on_the_event_stream() {
    let server = Server::start().await;
    let alice = server.token("alice", &["read", "write"]).await;
    let (status, created) = server
        .post_json(
            "/robot/register",
            &alice,
            &json!({"name": "listener", "events": ["WAVELET_SELF_ADDED"]}),
        )
        .await;
    assert_eq!(status, 201, "{created}");
    let robot_token = created["token"].as_str().unwrap().to_owned();
    // People cannot listen.
    let response = server
        .http
        .get(server.url("/robot/events"))
        .bearer_auth(&alice)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    let response = server
        .http
        .get(server.url("/robot/events"))
        .bearer_auth(&robot_token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let mut stream = response.bytes_stream();
    // Give the listener time to be registered (it is, before the response).
    let (wave, wavelet, _) = server
        .create_wave(
            &alice,
            &["alice@example.com", "listener@example.com"],
            "hello",
        )
        .await;
    let mut text = String::new();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !text.contains("\n\n") || !text.contains("event: bundle") {
        let chunk = tokio::time::timeout_at(deadline, stream.next())
            .await
            .expect("an event in time")
            .expect("stream open")
            .unwrap();
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
    let data = text
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap();
    let bundle: Value = serde_json::from_str(data).unwrap();
    assert_eq!(
        bundle["events"][0]["type"], "WAVELET_SELF_ADDED",
        "{bundle}"
    );
    // The robot answers through /robot/rpc.
    let (status, body) = server
        .post_json(
            "/robot/rpc",
            &robot_token,
            &json!([{"id": "a", "method": "wavelet.appendBlip", "params": {
                "waveId": wave, "waveletId": wavelet,
                "blipData": {"waveId": wave, "waveletId": wavelet, "blipId": "TBD_x",
                             "content": "\nstreamed robot here"}}}]),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert!(body[0].get("error").is_none(), "{body}");
    let blips = server.blips(&alice, &wave, &wavelet).await;
    assert!(
        blips.iter().any(|b| b.1 == "listener@example.com"),
        "{blips:?}"
    );
    drop(stream);
    server.stop().await;
}
