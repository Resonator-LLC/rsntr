//! Fakes for tests (here and in other crates): a push service that
//! records what it is sent (h2c, so the relay's HTTP/2 client is used as
//! against APNs), and keys to configure the relay with.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use serde_json::{Value, json};

use crate::config::{Config, Rate};

/// A request the fake received.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub version: String,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

#[derive(Default)]
struct Shared {
    requests: Vec<Recorded>,
    /// The answer to pushes: status and JSON body.
    answer: Option<(u16, Value)>,
}

/// A fake APNs, FCM and OAuth endpoint on loopback. Pushes are answered
/// 200 unless told otherwise; `POST /token` answers an access token.
pub struct FakePushService {
    addr: SocketAddr,
    shared: Arc<Mutex<Shared>>,
    task: tokio::task::JoinHandle<()>,
}

impl FakePushService {
    pub async fn start() -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .fallback(record)
            .with_state(Arc::clone(&shared));
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        FakePushService { addr, shared, task }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Answers later pushes with `status` and `body`.
    pub fn answer(&self, status: u16, body: Value) {
        self.shared.lock().unwrap().answer = Some((status, body));
    }

    /// The requests so far.
    pub fn requests(&self) -> Vec<Recorded> {
        self.shared.lock().unwrap().requests.clone()
    }

    /// The pushes so far (not the OAuth requests).
    pub fn pushes(&self) -> Vec<Recorded> {
        self.requests()
            .into_iter()
            .filter(|request| request.path != "/token")
            .collect()
    }
}

impl Drop for FakePushService {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn record(State(shared): State<Arc<Mutex<Shared>>>, request: Request) -> impl IntoResponse {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, 1 << 20).await.unwrap_or_default();
    let recorded = Recorded {
        method: parts.method.to_string(),
        path: parts.uri.path().to_owned(),
        version: format!("{:?}", parts.version),
        headers: parts
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect(),
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    let token = recorded.path == "/token";
    let mut shared = shared.lock().unwrap();
    shared.requests.push(recorded);
    if token {
        return (
            StatusCode::OK,
            axum::Json(json!({"access_token": "fake-access", "expires_in": 3600})),
        );
    }
    let (status, body) = shared.answer.clone().unwrap_or((200, json!({})));
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        axum::Json(body),
    )
}

/// A fresh APNs-style `.p8` (PKCS#8 PEM, P-256) and its public key
/// (uncompressed point), to check provider tokens with.
pub fn p8_key() -> (String, Vec<u8>) {
    let rng = SystemRandom::new();
    let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
    let pair =
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, doc.as_ref(), &rng).unwrap();
    (
        format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            STANDARD.encode(doc.as_ref())
        ),
        pair.public_key().as_ref().to_vec(),
    )
}

/// A relay config in `dir` sending APNs (production and sandbox) to
/// `apns` (a fresh `.p8` written there; its public key returned), with no
/// limits to speak of.
pub fn relay_config(dir: &Path, apns: &str) -> (Config, Vec<u8>) {
    let (pem, public) = p8_key();
    let key_path = dir.join("AuthKey_TEST.p8");
    std::fs::write(&key_path, pem).unwrap();
    let mut config = Config {
        listen: "127.0.0.1:0".to_owned(),
        sealing_key: dir.join("sealing.key"),
        ..Config::default()
    };
    config.apns.key_path = key_path;
    config.apns.key_id = "KEYID12345".to_owned();
    config.apns.team_id = "TEAMID1234".to_owned();
    config.apns.endpoint = apns.to_owned();
    config.apns.sandbox_endpoint = format!("{apns}/sandbox");
    let plenty = Rate::new(1000, 1);
    config.limits.seal_per_ip = plenty;
    config.limits.wake_per_ip = plenty;
    config.limits.wake_per_sender = plenty;
    (config, public)
}

/// A relay serving on a loopback port; stopped when dropped.
pub struct RunningRelay {
    pub relay: Arc<crate::Relay>,
    addr: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl RunningRelay {
    pub async fn start(config: Config) -> Self {
        let relay = Arc::new(crate::Relay::new(config).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let serving = Arc::clone(&relay);
        tokio::spawn(async move {
            let _ = serving
                .serve(listener, async move {
                    let _ = stopped.await;
                })
                .await;
        });
        RunningRelay {
            relay,
            addr,
            stop: Some(stop),
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for RunningRelay {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
