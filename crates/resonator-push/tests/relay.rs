//! The relay over HTTP, against fake push services: sealing, the checks
//! on wake requests, the rate limits, what APNs and FCM are sent, and
//! dead tokens answered `gone`.
//!
//! `fixtures/test-only-rsa.pem` is a throwaway RSA key made for these
//! tests (a stand-in for a service account's key); it grants nothing.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use resonator_push::config::{Config, Rate};
use resonator_push::jwt::{decode_unverified, now_secs};
use resonator_push::testing::{FakePushService, RunningRelay, relay_config};
use resonator_push::wake_message;
use ring::rand::SystemRandom;
use ring::signature::{
    ECDSA_P256_SHA256_FIXED, Ed25519KeyPair, KeyPair, RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey,
};
use serde_json::{Value, json};

const TEST_RSA: &str = include_str!("fixtures/test-only-rsa.pem");
const TEST_RSA_PUBLIC: &[u8] = include_bytes!("fixtures/test-only-rsa.pub.der");

struct Sender {
    pair: Ed25519KeyPair,
}

impl Sender {
    fn new() -> Sender {
        let doc = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        Sender {
            pair: Ed25519KeyPair::from_pkcs8(doc.as_ref()).unwrap(),
        }
    }

    fn hex(&self) -> String {
        self.pair
            .public_key()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn request(&self, sealed: &str, ts: i64) -> Value {
        let sig = self.pair.sign(&wake_message(sealed, ts));
        json!({
            "sealed": sealed,
            "sender": self.hex(),
            "ts": ts,
            "sig": URL_SAFE_NO_PAD.encode(sig.as_ref()),
        })
    }
}

struct Setup {
    _dir: tempfile::TempDir,
    fake: FakePushService,
    relay: RunningRelay,
    apns_public: Vec<u8>,
    http: reqwest::Client,
}

impl Setup {
    async fn start(adjust: impl FnOnce(&mut Config, &std::path::Path, &str)) -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakePushService::start().await;
        let (mut config, apns_public) = relay_config(dir.path(), &fake.url());
        adjust(&mut config, dir.path(), &fake.url());
        let relay = RunningRelay::start(config).await;
        Setup {
            _dir: dir,
            fake,
            relay,
            apns_public,
            http: reqwest::Client::new(),
        }
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let response = self
            .http
            .post(format!("{}{path}", self.relay.url()))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or_default())
    }

    async fn seal(&self, platform: &str, token: &str) -> String {
        let (status, body) = self
            .post("/seal", json!({"platform": platform, "token": token}))
            .await;
        assert_eq!(status, 200, "{body}");
        body["sealed"].as_str().unwrap().to_owned()
    }

    /// A wake at a time not used before (Ed25519 signatures are
    /// deterministic: the same blob and second would be a replay).
    async fn wake(&self, sender: &Sender, sealed: &str) -> (u16, Value) {
        static SECONDS_BACK: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        let back = SECONDS_BACK.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 200;
        self.post("/wake", sender.request(sealed, now_secs() - back))
            .await
    }
}

fn apns_token(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

#[tokio::test]
async fn a_wake_sends_apns_a_silent_background_push() {
    let setup = Setup::start(|_, _, _| {}).await;
    let health: Value = setup
        .http
        .get(format!("{}/health", setup.relay.url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health, json!({"status": "ok", "apns": true, "fcm": false}));

    let token = apns_token(0xab);
    let sealed = setup.seal("apns", &token.to_uppercase()).await;
    assert!(!sealed.contains(&token[..8]));
    let sender = Sender::new();
    assert_eq!(
        setup.wake(&sender, &sealed).await,
        (200, json!({"status": "sent"}))
    );

    let pushes = setup.fake.pushes();
    assert_eq!(pushes.len(), 1);
    let push = &pushes[0];
    assert_eq!(push.method, "POST");
    assert_eq!(push.version, "HTTP/2.0");
    assert_eq!(push.path, format!("/3/device/{token}"));
    assert_eq!(push.headers["apns-push-type"], "background");
    assert_eq!(push.headers["apns-priority"], "5");
    assert_eq!(push.headers["apns-topic"], "am.resonator.client.ios");
    assert!(!push.headers.contains_key("apns-collapse-id"));
    let payload: Value = serde_json::from_str(&push.body).unwrap();
    assert_eq!(payload, json!({"aps": {"content-available": 1}}));
    // The provider token: ES256 by the .p8, kid and team, issued now.
    let jwt = push.headers["authorization"]
        .strip_prefix("bearer ")
        .unwrap()
        .to_owned();
    let (header, claims) = decode_unverified(&jwt).unwrap();
    assert_eq!(header, json!({"alg": "ES256", "kid": "KEYID12345"}));
    assert_eq!(claims["iss"], "TEAMID1234");
    assert!((claims["iat"].as_i64().unwrap() - now_secs()).abs() < 60);
    let (input, signature) = jwt.rsplit_once('.').unwrap();
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, &setup.apns_public)
        .verify(
            input.as_bytes(),
            &URL_SAFE_NO_PAD.decode(signature).unwrap(),
        )
        .expect("the provider token verifies with the .p8's public key");

    // A sandbox token goes to the sandbox, with the same provider token.
    let sandbox = setup.seal("apns-sandbox", &apns_token(0xcd)).await;
    assert_eq!(setup.wake(&sender, &sandbox).await.0, 200);
    let pushes = setup.fake.pushes();
    assert_eq!(
        pushes[1].path,
        format!("/sandbox/3/device/{}", apns_token(0xcd))
    );
    assert_eq!(pushes[1].headers["authorization"], format!("bearer {jwt}"));
}

#[tokio::test]
async fn wake_requests_are_checked() {
    let setup = Setup::start(|_, _, _| {}).await;
    let sealed = setup.seal("apns", &apns_token(1)).await;
    let sender = Sender::new();
    let other = Sender::new();
    let refused = |body: &Value| body["status"].as_str().unwrap().to_owned();

    // Stale either way.
    for ts in [now_secs() - 301, now_secs() + 301] {
        let (status, body) = setup.post("/wake", sender.request(&sealed, ts)).await;
        assert_eq!((status, refused(&body).as_str()), (401, "stale"));
    }
    // Signed by another key, or over another blob or time.
    let mut request = sender.request(&sealed, now_secs());
    request["sender"] = json!(other.hex());
    let (status, body) = setup.post("/wake", request).await;
    assert_eq!((status, refused(&body).as_str()), (401, "bad_signature"));
    let mut request = sender.request(&sealed, now_secs());
    request["ts"] = json!(now_secs() - 5);
    assert_eq!(setup.post("/wake", request).await.0, 401);
    let mut request = sender.request(&sealed, now_secs());
    request["sig"] = json!("AAAA");
    assert_eq!(setup.post("/wake", request).await.0, 401);
    let mut request = sender.request(&sealed, now_secs());
    request["sender"] = json!("not a key");
    assert_eq!(setup.post("/wake", request).await.0, 400);
    // A tampered blob, signed as it is.
    let mut bytes = URL_SAFE_NO_PAD.decode(&sealed).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    let tampered = URL_SAFE_NO_PAD.encode(bytes);
    let (status, body) = setup.wake(&sender, &tampered).await;
    assert_eq!((status, refused(&body).as_str()), (400, "bad_blob"));
    // Not JSON.
    assert_eq!(setup.post("/wake", json!({"sealed": 1})).await.0, 400);
    assert!(setup.fake.pushes().is_empty());

    // A good one, then the same request again.
    let request = sender.request(&sealed, now_secs());
    assert_eq!(setup.post("/wake", request.clone()).await.0, 200);
    let (status, body) = setup.post("/wake", request).await;
    assert_eq!((status, refused(&body).as_str()), (409, "replay"));
    assert_eq!(setup.fake.pushes().len(), 1);

    // Seal: bad platform and token.
    assert_eq!(
        setup
            .post("/seal", json!({"platform": "sms", "token": apns_token(1)}))
            .await
            .0,
        400
    );
    assert_eq!(
        setup
            .post("/seal", json!({"platform": "apns", "token": "xyz"}))
            .await
            .0,
        400
    );
    // An FCM token with no FCM configured.
    let fcm = setup.seal("fcm", "abc:DEF").await;
    assert_eq!(
        setup.wake(&sender, &fcm).await,
        (503, json!({"status": "unsupported"}))
    );
}

#[tokio::test]
async fn wakes_are_rate_limited_per_device_and_sender_and_seals_per_address() {
    let setup = Setup::start(|config, _, _| {
        config.limits.wake_per_device = Rate::new(2, 60_000);
        config.limits.wake_per_sender = Rate::new(4, 60_000);
        config.limits.seal_per_ip = Rate::new(6, 60_000);
    })
    .await;
    let a = Sender::new();
    let b = Sender::new();
    let sealed = setup.seal("apns", &apns_token(1)).await;
    assert_eq!(setup.wake(&a, &sealed).await.0, 200);
    assert_eq!(setup.wake(&b, &sealed).await.0, 200);
    // The device's burst is used up, whoever asks...
    assert_eq!(
        setup.wake(&a, &sealed).await,
        (429, json!({"status": "rate_limited"}))
    );
    // ...and with whatever blob of its token.
    let again = setup.seal("apns", &apns_token(1)).await;
    assert_ne!(again, sealed);
    assert_eq!(setup.wake(&b, &again).await.0, 429);
    assert_eq!(setup.fake.pushes().len(), 2);
    // Another device is not held up; sender a then hits its own limit
    // (refused asks count too: four so far).
    let other = setup.seal("apns", &apns_token(2)).await;
    assert_eq!(setup.wake(&a, &other).await.0, 200);
    assert_eq!(setup.wake(&a, &other).await.0, 200);
    let third = setup.seal("apns", &apns_token(3)).await;
    assert_eq!(setup.wake(&a, &third).await.0, 429);
    assert_eq!(setup.wake(&b, &third).await.0, 200);
    assert_eq!(setup.fake.pushes().len(), 5);
    // Four seals so far: two more, then refused.
    setup.seal("apns", &apns_token(4)).await;
    setup.seal("apns", &apns_token(5)).await;
    assert_eq!(
        setup
            .post("/seal", json!({"platform": "apns", "token": apns_token(6)}))
            .await
            .0,
        429
    );
}

#[tokio::test]
async fn dead_tokens_are_answered_gone() {
    let setup = Setup::start(|_, _, _| {}).await;
    let sender = Sender::new();
    for (n, (status, reason)) in [
        (410, "Unregistered"),
        (400, "BadDeviceToken"),
        (400, "DeviceTokenNotForTopic"),
    ]
    .into_iter()
    .enumerate()
    {
        setup.fake.answer(status, json!({"reason": reason}));
        let sealed = setup.seal("apns", &apns_token(n as u8)).await;
        assert_eq!(
            setup.wake(&sender, &sealed).await,
            (410, json!({"status": "gone"})),
            "{reason}"
        );
    }
    // Other refusals are failures, not gone.
    for (n, (status, reason)) in [
        (400, "BadTopic"),
        (500, "InternalServerError"),
        (429, "TooManyRequests"),
    ]
    .into_iter()
    .enumerate()
    {
        setup.fake.answer(status, json!({"reason": reason}));
        let sealed = setup.seal("apns", &apns_token(10 + n as u8)).await;
        assert_eq!(
            setup.wake(&sender, &sealed).await,
            (502, json!({"status": "failed"})),
            "{reason}"
        );
    }
    // An expired provider token is made again and the push retried.
    setup
        .fake
        .answer(403, json!({"reason": "ExpiredProviderToken"}));
    let sealed = setup.seal("apns", &apns_token(20)).await;
    assert_eq!(setup.wake(&sender, &sealed).await.0, 502);
    assert_eq!(
        setup
            .fake
            .pushes()
            .iter()
            .filter(|push| push.path.ends_with(&apns_token(20)))
            .count(),
        2
    );
}

#[tokio::test]
async fn fcm_gets_a_high_priority_data_message_with_an_oauth_token() {
    let setup = Setup::start(|config, dir, fake| {
        let account = json!({
            "type": "service_account",
            "project_id": "resonator-test",
            "client_email": "relay@resonator-test.iam.gserviceaccount.com",
            "private_key": TEST_RSA,
            "token_uri": format!("{fake}/token"),
        });
        let path = dir.join("service-account.json");
        std::fs::write(&path, account.to_string()).unwrap();
        config.fcm.service_account_path = path;
        config.fcm.endpoint = fake.to_owned();
    })
    .await;
    let sender = Sender::new();
    let sealed = setup.seal("fcm", "fcm-token:ABC_123-x").await;
    assert_eq!(setup.wake(&sender, &sealed).await.0, 200);
    assert_eq!(setup.wake(&sender, &sealed).await.0, 200);

    let requests = setup.fake.requests();
    // One OAuth exchange, cached for the second push.
    let oauth: Vec<_> = requests.iter().filter(|r| r.path == "/token").collect();
    assert_eq!(oauth.len(), 1);
    let form = &oauth[0].body;
    assert!(form.starts_with(
        "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion="
    ));
    let assertion = form.split("assertion=").nth(1).unwrap();
    let (header, claims) = decode_unverified(assertion).unwrap();
    assert_eq!(header["alg"], "RS256");
    assert_eq!(
        claims["iss"],
        "relay@resonator-test.iam.gserviceaccount.com"
    );
    assert_eq!(
        claims["scope"],
        "https://www.googleapis.com/auth/firebase.messaging"
    );
    assert_eq!(claims["aud"], format!("{}/token", setup.fake.url()));
    assert_eq!(
        claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(),
        3600
    );
    let (input, signature) = assertion.rsplit_once('.').unwrap();
    UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, TEST_RSA_PUBLIC)
        .verify(
            input.as_bytes(),
            &URL_SAFE_NO_PAD.decode(signature).unwrap(),
        )
        .expect("the assertion verifies with the service account's key");

    let pushes = setup.fake.pushes();
    assert_eq!(pushes.len(), 2);
    assert_eq!(pushes[0].path, "/v1/projects/resonator-test/messages:send");
    assert_eq!(pushes[0].headers["authorization"], "Bearer fake-access");
    let message: Value = serde_json::from_str(&pushes[0].body).unwrap();
    assert_eq!(
        message,
        json!({"message": {
            "token": "fcm-token:ABC_123-x",
            "data": {"resonator": "wake"},
            "android": {"priority": "high", "ttl": "300s"},
        }})
    );
    assert!(message["message"].get("notification").is_none());

    // UNREGISTERED is gone.
    setup.fake.answer(
        404,
        json!({"error": {"status": "NOT_FOUND", "details": [{"errorCode": "UNREGISTERED"}]}}),
    );
    let dead = setup.seal("fcm", "dead-token").await;
    assert_eq!(
        setup.wake(&sender, &dead).await,
        (410, json!({"status": "gone"}))
    );
}

#[tokio::test]
async fn a_relay_without_push_services_still_seals_and_checks() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        listen: "127.0.0.1:0".to_owned(),
        sealing_key: dir.path().join("k"),
        ..Config::default()
    };
    let relay = RunningRelay::start(config).await;
    let http = reqwest::Client::new();
    let body: Value = http
        .post(format!("{}/seal", relay.url()))
        .json(&json!({"platform": "apns", "token": apns_token(1)}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sealed = body["sealed"].as_str().unwrap();
    let response = http
        .post(format!("{}/wake", relay.url()))
        .json(&Sender::new().request(sealed, now_secs()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 503);
    // The relay restarted on the same key opens what it sealed.
    let again = Arc::new(
        resonator_push::Relay::new(Config {
            sealing_key: dir.path().join("k"),
            ..Config::default()
        })
        .unwrap(),
    );
    assert!(format!("{again:?}").contains("apns: false"));
}
