//! A node in device mode, in-process: the loopback token signs its one
//! account in, requests from other machines are refused, contacts stand
//! in for the directory, and it stops cleanly and starts again on the
//! same data.

use std::net::SocketAddr;
use std::path::Path;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use resonator_node::{Handle, Node, device_config, launch};
use serde_json::{Value, json};
use tower::ServiceExt;

/// A device config in `dir` without federation (no endpoint to bind):
/// person mode needs federation, so the account is `me@example.com`.
fn config(dir: &Path) -> resonator_server::config::Config {
    device_config(
        dir,
        "domain = \"example.com\"\n[federation]\nenabled = false\n[person]\nenabled = false\n",
    )
    .unwrap()
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

/// Signs in with the token; returns the session cookie.
async fn sign_in(node: &Node) -> String {
    let response = http()
        .get(node.signed_in_url().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::LOCATION], "/");
    response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

async fn session(node: &Node, cookie: &str) -> Value {
    http()
        .get(format!("{}/session", node.local_url()))
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_token_signs_the_device_account_in() {
    let dir = tempfile::tempdir().unwrap();
    let node = launch(config(dir.path())).await.unwrap();
    assert!(node.local_url().starts_with("http://127.0.0.1:"));
    assert!(node.local_addrs()[0].ip().is_loopback());
    let token = node.token().unwrap().to_owned();
    assert!(token.len() >= 40);
    // The URL file, readable by the owner only.
    let path = node.app().device_url_path();
    let written = std::fs::read_to_string(&path).unwrap();
    assert_eq!(written.trim(), node.signed_in_url().unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // No session without the token; a wrong token is refused.
    let anonymous: Value = http()
        .get(format!("{}/session", node.local_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(anonymous.get("address").is_none_or(Value::is_null));
    assert_eq!(anonymous["mode"], "device");
    assert_eq!(anonymous["registration_enabled"], false);
    let wrong = http()
        .get(format!("{}/?token=nope", node.local_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::FORBIDDEN);

    let cookie = sign_in(&node).await;
    let info = session(&node, &cookie).await;
    assert_eq!(info["address"], "me@example.com");
    assert_eq!(info["mode"], "device");
    assert_eq!(info["device"]["account"], "me@example.com");

    // Registration and the directory are off.
    let register = http()
        .post(format!("{}/auth/register", node.local_url()))
        .json(&json!({"address": "eve", "password": "eve-password-long"}))
        .send()
        .await
        .unwrap();
    assert_eq!(register.status(), StatusCode::FORBIDDEN);
    let directory = http()
        .get(format!("{}/directory?query=", node.local_url()))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(directory.status(), StatusCode::NOT_FOUND);

    // Another start, another token.
    node.shutdown().await;
    assert!(!path.exists());
    let node = launch(config(dir.path())).await.unwrap();
    assert_ne!(node.token().unwrap(), token);
    let cookie = sign_in(&node).await;
    assert_eq!(session(&node, &cookie).await["address"], "me@example.com");
    node.shutdown().await;
}

/// A Data API token of the signed-in account.
async fn api_token(node: &Node, cookie: &str) -> String {
    let response = http()
        .post(format!("{}/auth/tokens", node.local_url()))
        .header(header::COOKIE, cookie)
        .header(header::ORIGIN, node.local_url())
        .json(&json!({"name": "t", "scopes": ["read", "write"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body: Value = response.json().await.unwrap();
    body["token"].as_str().unwrap().to_owned()
}

/// The defaults of a device: person mode with a person key made on the
/// first start, the account on the person domain, federation on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_device_makes_its_person() {
    let dir = tempfile::tempdir().unwrap();
    let overrides =
        "[federation]\nrelay = \"off\"\ndiscovery = \"off\"\nlisten = \"127.0.0.1:0\"\n";
    let node = launch(device_config(dir.path(), overrides).unwrap())
        .await
        .unwrap();
    let domain = node.config().domain.clone();
    assert!(dir.path().join("person.key").exists());
    let cookie = sign_in(&node).await;
    let info = session(&node, &cookie).await;
    assert_eq!(info["address"], format!("me@{domain}"));
    let key = node.federation().unwrap().endpoint().id().to_z32();
    assert!(node.lease_state().is_some());
    assert_eq!(info["device"]["key"], key);
    node.shutdown().await;

    // Again: the same person, no new key.
    let key_file = std::fs::read(dir.path().join("person.key")).unwrap();
    let node = launch(device_config(dir.path(), overrides).unwrap())
        .await
        .unwrap();
    assert_eq!(node.config().domain, domain);
    assert_eq!(
        std::fs::read(dir.path().join("person.key")).unwrap(),
        key_file
    );
    node.shutdown().await;
}

/// A request through the node's router as if from `peer`.
async fn from_peer(node: &Node, peer: &str, uri: &str) -> StatusCode {
    let router = resonator_server::http::router(node.state().clone());
    let mut request = Request::get(uri).body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    router.oneshot(request).await.unwrap().status()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_from_other_machines_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let node = launch(config(dir.path())).await.unwrap();
    let token = node.token().unwrap().to_owned();
    let with_token = format!("/?token={token}");
    assert_eq!(
        from_peer(&node, "192.168.1.20:5000", &with_token).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        from_peer(&node, "100.64.0.7:5000", "/session").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        from_peer(&node, "[2001:db8::1]:5000", "/session").await,
        StatusCode::FORBIDDEN
    );
    // Loopback, also IPv6 and IPv4-mapped.
    assert_eq!(
        from_peer(&node, "127.0.0.1:5000", "/session").await,
        StatusCode::OK
    );
    assert_eq!(
        from_peer(&node, "[::1]:5000", "/session").await,
        StatusCode::OK
    );
    assert_eq!(
        from_peer(&node, "[::ffff:127.0.0.1]:5000", &with_token).await,
        StatusCode::SEE_OTHER
    );
    node.shutdown().await;

    // allow_remote: other machines get the normal sign-in, never the
    // token.
    let mut config = config(dir.path());
    config.device.allow_remote = true;
    config.listen = vec!["127.0.0.1:0".to_owned()];
    let node = launch(config).await.unwrap();
    let with_token = format!("/?token={}", node.token().unwrap());
    assert_eq!(
        from_peer(&node, "192.168.1.20:5000", "/session").await,
        StatusCode::OK
    );
    assert_eq!(
        from_peer(&node, "192.168.1.20:5000", &with_token).await,
        StatusCode::FORBIDDEN
    );
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn contacts_stand_in_for_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let node = launch(config(dir.path())).await.unwrap();
    let cookie = sign_in(&node).await;
    let origin = node.local_url();
    let contacts = |query: &str| {
        let request = http()
            .get(format!("{origin}/contacts"))
            .query(&[("query", query)])
            .header(header::COOKIE, &cookie);
        async move {
            let response = request.send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            response.json::<Value>().await.unwrap()["accounts"].clone()
        }
    };
    assert_eq!(contacts("").await, json!([]));

    // Added by address and by link.
    let add = |body: Value| {
        http()
            .post(format!("{origin}/contacts"))
            .header(header::COOKIE, &cookie)
            .header(header::ORIGIN, &origin)
            .json(&body)
            .send()
    };
    let added = add(json!({"address": "Bob@example.com"})).await.unwrap();
    assert_eq!(added.status(), StatusCode::CREATED);
    let again = add(json!({"address": "bob@example.com"})).await.unwrap();
    assert_eq!(again.status(), StatusCode::OK);
    let linked = add(json!({"link": "resonator:contact/carol@example.com"}))
        .await
        .unwrap();
    assert_eq!(linked.status(), StatusCode::CREATED);
    let bad = add(json!({"address": "not an address"})).await.unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    let me = add(json!({"address": "me@example.com"})).await.unwrap();
    assert_eq!(me.status(), StatusCode::BAD_REQUEST);

    let all = contacts("").await;
    let addresses: Vec<&str> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["address"].as_str().unwrap())
        .collect();
    assert_eq!(addresses, ["bob@example.com", "carol@example.com"]);
    assert_eq!(all[0]["kind"], "human");
    assert_eq!(all[0]["added"], true);
    let found = contacts("CAR").await;
    assert_eq!(found.as_array().unwrap().len(), 1);
    assert_eq!(found[0]["address"], "carol@example.com");

    // Everyone the account shares a conversation with.
    let token = api_token(&node, &cookie).await;
    let created = http()
        .post(format!("{origin}/robot/dataapi"))
        .bearer_auth(&token)
        .json(&json!([{
            "id": "create",
            "method": "robot.createWavelet",
            "params": {"waveletData": {
                "waveId": "example.com/TBD_wave",
                "waveletId": "example.com/conv+root",
                "rootBlipId": "TBD_blip",
                "participants": ["me@example.com", "dana@example.com"],
            }}
        }]))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let found = contacts("dana").await;
        if found.as_array().unwrap().len() == 1 {
            assert_eq!(found[0]["address"], "dana@example.com");
            assert_eq!(found[0]["added"], false);
            break;
        }
        assert!(std::time::Instant::now() < deadline, "dana never listed");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(contacts("").await.as_array().unwrap().len(), 3);

    // Removed again.
    let removed = http()
        .delete(format!("{origin}/contacts/bob@example.com"))
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, &origin)
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    assert_eq!(contacts("bob").await, json!([]));

    // Without a session: refused.
    let anonymous = http()
        .get(format!("{origin}/contacts"))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::FORBIDDEN);
    node.shutdown().await;
}

#[test]
fn a_handle_starts_stops_and_starts_again_on_the_same_data() {
    let dir = tempfile::tempdir().unwrap();
    let handle: Handle = resonator_node::start(config(dir.path())).unwrap();
    assert!(handle.is_running());
    assert!(handle.local_url().starts_with("http://127.0.0.1:"));
    let url = handle.signed_in_url().unwrap();
    assert!(url.contains(handle.token().unwrap()));
    assert!(!format!("{handle:?}").contains(handle.token().unwrap()));
    assert!(handle.lease_state().is_none());
    let background = handle
        .enter_background(std::time::Duration::from_secs(1))
        .unwrap();
    assert_eq!(background.handed_over_to, None);
    let foreground = handle
        .enter_foreground(std::time::Duration::from_secs(1))
        .unwrap();
    assert_eq!(foreground.devices_synced, 0);
    // A plain client on another thread: the handle's runtime serves it.
    let status = std::thread::spawn(move || raw_get(&url)).join().unwrap();
    assert!(status.starts_with("HTTP/1.1 303"), "{status}");
    handle.stop();
    assert!(!handle.is_running());
    handle.stop();
    assert!(matches!(
        handle.enter_background(std::time::Duration::from_secs(1)),
        Err(resonator_node::NodeError::Stopped)
    ));
    drop(handle);
    // The port is free and the database closed: it starts again.
    let handle = resonator_node::start(config(dir.path())).unwrap();
    assert!(handle.is_running());
    drop(handle);
}

/// `GET url` over a plain TCP connection; returns the status line.
fn raw_get(url: &str) -> String {
    use std::io::{BufRead, BufReader, Write};
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_at(rest.find('/').unwrap());
    let mut stream = std::net::TcpStream::connect(host).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    line
}

/// The person settings over HTTP on a first (admin) device: this person
/// and device, a pairing invitation with its link and QR code, the
/// recovery phrase only when confirmed and never stored by the browser,
/// and none of it without the device's sign-in or outside person mode.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_person_settings_over_http() {
    let dir = tempfile::tempdir().unwrap();
    let overrides = "[federation]\nrelay = \"off\"\ndiscovery = \"off\"\nlisten = \
                     \"127.0.0.1:0\"\n[person]\ndevice_name = \"phone\"\n";
    let node = launch(device_config(dir.path(), overrides).unwrap())
        .await
        .unwrap();
    let origin = node.local_url();
    let cookie = sign_in(&node).await;
    let get = |path: &str, cookie: Option<&str>| {
        let mut request = http().get(format!("{origin}{path}"));
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        request.send()
    };
    let post = |path: &str, body: Value| {
        http()
            .post(format!("{origin}{path}"))
            .header(header::COOKIE, &cookie)
            .header(header::ORIGIN, &origin)
            .json(&body)
            .send()
    };

    // Not signed in: refused.
    let anonymous = get("/person", None).await.unwrap();
    assert_eq!(anonymous.status(), StatusCode::FORBIDDEN);

    let overview: Value = get("/person", Some(&cookie))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let domain = node.config().domain.clone();
    let key = node.federation().unwrap().endpoint().id().to_z32();
    assert_eq!(overview["domain"], domain);
    assert_eq!(overview["device"], key);
    assert_eq!(overview["device_name"], "phone");
    assert_eq!(overview["admin"], true);
    let devices = overview["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["this"], true);

    // An invitation: a link and its QR code; listed until used.
    let invite = post("/person/pair", json!({"admin": false})).await.unwrap();
    assert_eq!(invite.status(), StatusCode::OK);
    let invitation: Value = invite.json().await.unwrap();
    assert!(
        invitation["link"]
            .as_str()
            .unwrap()
            .starts_with("resonator:pair/")
    );
    assert!(invitation["qr"].as_str().unwrap().contains("<svg"));
    // Without discovery, pairing by code is off unless a rendezvous is
    // configured: no code.
    assert!(invitation["code"].is_null(), "{invitation}");
    let id = invitation["id"].as_str().unwrap().to_owned();
    let listed: Value = get("/person/pair", Some(&cookie))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["invitations"][0]["id"], id);
    assert_eq!(listed["invitations"][0]["state"], "open");
    let cancelled = http()
        .delete(format!("{origin}/person/pair/{id}"))
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, &origin)
        .send()
        .await
        .unwrap();
    assert_eq!(cancelled.status(), StatusCode::OK);

    // The phrase: only confirmed, and not to be cached.
    let unconfirmed = post("/person/phrase", json!({})).await.unwrap();
    assert_eq!(unconfirmed.status(), StatusCode::BAD_REQUEST);
    let shown = post("/person/phrase", json!({"confirm": true}))
        .await
        .unwrap();
    assert_eq!(shown.status(), StatusCode::OK);
    assert_eq!(shown.headers()[header::CACHE_CONTROL], "no-store");
    let words: Value = shown.json().await.unwrap();
    assert_eq!(words["phrase"].as_str().unwrap().split(' ').count(), 24);

    // A bad link and a bad phrase are refused without a restart.
    let joined = post(
        "/person/pair/join",
        json!({"link": "resonator:pair/nonsense", "name": "x"}),
    )
    .await
    .unwrap();
    assert_eq!(joined.status(), StatusCode::BAD_REQUEST);
    for (code, status) in [
        ("nonsense", StatusCode::BAD_REQUEST),
        ("482913-ocean-tigr", StatusCode::BAD_REQUEST),
        ("482913-ocean-tiger", StatusCode::CONFLICT),
    ] {
        let joined = post("/person/pair/join", json!({"code": code}))
            .await
            .unwrap();
        assert_eq!(joined.status(), status, "{code}");
    }
    let nothing = post("/person/pair/join", json!({})).await.unwrap();
    assert_eq!(nothing.status(), StatusCode::BAD_REQUEST);
    let progress: Value = get("/person/pair/join", Some(&cookie))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(progress["stage"].is_null(), "{progress}");
    let recovered = post(
        "/person/recover",
        json!({"phrase": "not a phrase", "revoke_old": true}),
    )
    .await
    .unwrap();
    assert_eq!(recovered.status(), StatusCode::BAD_REQUEST);

    // The anchor setting sticks.
    let anchored = post("/person/anchor", json!({"anchor": true}))
        .await
        .unwrap();
    assert_eq!(anchored.status(), StatusCode::OK);
    let overview: Value = get("/person", Some(&cookie))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(overview["anchor"], true);
    node.shutdown().await;

    // Outside person mode there is nothing to see.
    let dir = tempfile::tempdir().unwrap();
    let node = launch(config(dir.path())).await.unwrap();
    let cookie = sign_in(&node).await;
    let response = http()
        .get(format!("{}/person", node.local_url()))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    node.shutdown().await;
}

#[test]
fn a_device_takes_large_attachments_unless_told_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        config(dir.path()).attachment_max_size,
        4 * 1024 * 1024 * 1024
    );
    let small = device_config(
        dir.path(),
        "attachment_max_size = 1024\n[federation]\nenabled = false\n[person]\nenabled = false\n",
    )
    .unwrap();
    assert_eq!(small.attachment_max_size, 1024);
}
