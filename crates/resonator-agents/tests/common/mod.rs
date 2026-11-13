//! A real server (wave server, HTTP, robots, agents) on a temporary
//! database, and helpers to talk to it.
#![allow(dead_code)]

pub mod fake_provider;

use std::net::SocketAddr;
use std::time::Duration;

use resonator_agents::Integrations;
use resonator_server::app::App;
use resonator_server::config::Config;
use serde_json::{Value, json};
use tempfile::TempDir;

pub const DOMAIN: &str = "example.com";
pub const TIMEOUT: Duration = Duration::from_secs(10);

pub struct Server {
    pub app: Option<App>,
    pub integrations: Option<Integrations>,
    pub addr: SocketAddr,
    pub http: reqwest::Client,
    pub dir: TempDir,
}

pub fn base_config(dir: &TempDir) -> Config {
    Config {
        domain: DOMAIN.to_owned(),
        listen: vec!["127.0.0.1:0".to_owned()],
        database_path: dir.path().join("resonator.db"),
        register_max_per_hour: 0,
        ..Config::default()
    }
}

impl Server {
    pub async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    pub async fn start_with(configure: impl FnOnce(&mut Config)) -> Self {
        Self::start_in(tempfile::tempdir().expect("temp dir"), configure).await
    }

    pub async fn start_in(dir: TempDir, configure: impl FnOnce(&mut Config)) -> Self {
        let mut config = base_config(&dir);
        configure(&mut config);
        let mut prepared = App::prepare(&config).await.expect("prepare");
        let (integrations, routes) = Integrations::mount(&mut prepared).await.expect("mount");
        let app = prepared.serve(routes).await.expect("serve");
        let addr = app.local_addrs()[0];
        Server {
            app: Some(app),
            integrations: Some(integrations),
            addr,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("client"),
            dir,
        }
    }

    pub async fn stop(mut self) -> TempDir {
        if let Some(integrations) = self.integrations.take() {
            integrations.shutdown().await;
        }
        if let Some(app) = self.app.take() {
            app.shutdown().await;
        }
        self.dir
    }

    pub fn integrations(&self) -> &Integrations {
        self.integrations.as_ref().expect("running")
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// Registers and signs in; returns the session cookie.
    pub async fn user(&self, name: &str) -> String {
        let password = format!("{name}-password-long");
        let registered = self
            .http
            .post(self.url("/auth/register"))
            .json(&json!({"address": name, "password": password}))
            .send()
            .await
            .unwrap();
        assert!(
            registered.status() == 200 || registered.status() == 403,
            "register {name}: {}",
            registered.status()
        );
        let response = self
            .http
            .post(self.url("/auth/signin"))
            .json(&json!({"address": name, "password": password}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "sign in {name}");
        let header = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .to_owned();
        header.split(';').next().unwrap().to_owned()
    }

    /// A personal access token of a new user.
    pub async fn token(&self, name: &str, scopes: &[&str]) -> String {
        let cookie = self.user(name).await;
        self.token_for(&cookie, scopes).await
    }

    pub async fn token_for(&self, cookie: &str, scopes: &[&str]) -> String {
        let response = self
            .http
            .post(self.url("/auth/tokens"))
            .header("Cookie", cookie)
            .header("Origin", format!("http://{}", self.addr))
            .json(&json!({"name": format!("t{}", scopes.join("-")), "scopes": scopes}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201);
        let body: Value = response.json().await.unwrap();
        body["token"].as_str().unwrap().to_owned()
    }

    pub async fn post_json(&self, path: &str, token: &str, body: &Value) -> (u16, Value) {
        let response = self
            .http
            .post(self.url(path))
            .bearer_auth(token)
            .json(body)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        let text = response.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    /// Runs Data API operations as the token's owner.
    pub async fn data_api(&self, token: &str, operations: Value) -> (u16, Value) {
        self.post_json("/robot/dataapi", token, &operations).await
    }

    /// Creates a wave; returns (wave id, wavelet id, root blip id).
    pub async fn create_wave(
        &self,
        token: &str,
        participants: &[&str],
        text: &str,
    ) -> (String, String, String) {
        let (status, body) = self
            .data_api(
                token,
                json!([{
                    "id": "create",
                    "method": "robot.createWavelet",
                    "params": {"waveletData": {
                        "waveId": "example.com/TBD_wave",
                        "waveletId": "example.com/conv+root",
                        "rootBlipId": "TBD_blip",
                        "participants": participants,
                    }}
                }]),
            )
            .await;
        assert_eq!(status, 200, "{body}");
        let data = &body[0]["data"];
        let wave = data["waveId"].as_str().expect("wave id").to_owned();
        let wavelet = data["waveletId"].as_str().unwrap().to_owned();
        let blip = data["blipId"].as_str().unwrap().to_owned();
        if !text.is_empty() {
            let (status, body) = self
                .data_api(
                    token,
                    json!([modify_append("w", &wave, &wavelet, &blip, text)]),
                )
                .await;
            assert_eq!(status, 200);
            assert!(body[0].get("error").is_none(), "{body}");
        }
        (wave, wavelet, blip)
    }

    /// The wave as `robot.fetchWave` shows it to the token's owner.
    pub async fn fetch(&self, token: &str, wave: &str, wavelet: &str) -> Value {
        let (status, body) = self
            .data_api(
                token,
                json!([{"id": "f", "method": "robot.fetchWave",
                        "params": {"waveId": wave, "waveletId": wavelet}}]),
            )
            .await;
        assert_eq!(status, 200, "{body}");
        body[0].clone()
    }

    /// The blips of a wave, (id, creator, content), in no particular order.
    pub async fn blips(
        &self,
        token: &str,
        wave: &str,
        wavelet: &str,
    ) -> Vec<(String, String, String)> {
        let fetched = self.fetch(token, wave, wavelet).await;
        let Some(blips) = fetched["data"]["blips"].as_object() else {
            return Vec::new();
        };
        blips
            .iter()
            .map(|(id, blip)| {
                (
                    id.clone(),
                    blip["creator"].as_str().unwrap_or("").to_owned(),
                    blip["content"].as_str().unwrap_or("").to_owned(),
                )
            })
            .collect()
    }

    /// Submits a delta of one no-op wavelet operation by `who`, as a
    /// client does when a person ends an edit session ("finished
    /// editing"). Returns the wavelet's version after it.
    pub async fn no_op(&self, who: &str, wave: &str, wavelet: &str) -> u64 {
        use resonator_model::id::{DualIdSerialiser, IdSerialiser, ParticipantId, WaveletName};
        use resonator_model::operation::{WaveletDelta, WaveletOperation, WaveletOperationContext};
        use resonator_server::waveserver::WaveletProvider;
        let name = WaveletName::of(
            DualIdSerialiser::MODERN.deserialise_wave_id(wave).unwrap(),
            DualIdSerialiser::MODERN
                .deserialise_wavelet_id(wavelet)
                .unwrap(),
        );
        let server = self.app.as_ref().unwrap().server();
        let target = server
            .get_snapshot(&name)
            .await
            .unwrap()
            .unwrap()
            .snapshot
            .hashed_version()
            .clone();
        let author = ParticipantId::of(who).unwrap();
        let delta = WaveletDelta::new(
            author.clone(),
            target,
            vec![WaveletOperation::no_op(WaveletOperationContext::new(
                author.clone(),
                0,
                1,
            ))],
        );
        let delta = resonator_model::proto::serialize_delta(&delta).unwrap();
        let result = server
            .submit_request_as(&author, &name, delta)
            .await
            .unwrap();
        result.hashed_version_after_application.version()
    }

    pub async fn wait_until<F, Fut>(&self, what: &str, mut condition: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            if condition().await {
                return;
            }
            assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }
}

/// `document.modify` appending text to a blip.
pub fn modify_append(id: &str, wave: &str, wavelet: &str, blip: &str, text: &str) -> Value {
    json!({"id": id, "method": "document.modify", "params": {
        "waveId": wave, "waveletId": wavelet, "blipId": blip,
        "modifyAction": {"modifyHow": "INSERT_AFTER", "values": [text]}
    }})
}

/// `wavelet.addParticipant`.
pub fn add_participant(id: &str, wave: &str, wavelet: &str, who: &str) -> Value {
    json!({"id": id, "method": "wavelet.addParticipant", "params": {
        "waveId": wave, "waveletId": wavelet, "participantId": who
    }})
}

/// `blip.continueThread`: a new blip after `blip` in its thread.
pub fn continue_thread(id: &str, wave: &str, wavelet: &str, blip: &str, text: &str) -> Value {
    json!({"id": id, "method": "blip.continueThread", "params": {
        "waveId": wave, "waveletId": wavelet, "blipId": blip,
        "blipData": {"waveId": wave, "waveletId": wavelet, "blipId": format!("TBD_{id}"),
                     "content": text}
    }})
}
