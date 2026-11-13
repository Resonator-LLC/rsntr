//! A test harness that runs the real server on an ephemeral port and
//! talks to it with a real HTTP client and real WebSocket clients.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use resonator_model::id::{IdSerialiser, ModernIdSerialiser, ParticipantId, WaveletName};
use resonator_model::operation::{WaveletDelta, WaveletOperation};
use resonator_model::proto::federation::ProtocolWaveletDelta;
use resonator_model::proto::json::{self, JsonMessage};
use resonator_model::proto::rpc::{CancelRpc, RpcFinished};
use resonator_model::proto::waveclient_rpc::{
    ProtocolAuthenticate, ProtocolOpenRequest, ProtocolSubmitRequest, ProtocolSubmitResponse,
    ProtocolWaveletUpdate, WaveletVersion,
};
use resonator_model::proto::{deserialize_hashed_version, serialize_delta};
use resonator_model::version::{HashedVersion, HashedVersionFactory};
use resonator_server::app::App;
use resonator_server::config::Config;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::common::DOMAIN;

/// How long a test waits for something the server should do promptly.
pub const TIMEOUT: Duration = Duration::from_secs(10);

pub struct TestServer {
    pub app: Option<App>,
    pub addr: SocketAddr,
    pub http: reqwest::Client,
    pub dir: TempDir,
}

impl TestServer {
    pub async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    pub async fn start_with(configure: impl FnOnce(&mut Config)) -> Self {
        Self::start_in(tempfile::tempdir().expect("temp dir"), configure).await
    }

    /// Starts the server on the database in `dir` (creating it if it is
    /// not there), for tests that restart the server.
    pub async fn start_in(dir: TempDir, configure: impl FnOnce(&mut Config)) -> Self {
        let mut config = Config {
            domain: DOMAIN.to_owned(),
            listen: vec!["127.0.0.1:0".to_owned()],
            database_path: dir.path().join("resonator.db"),
            // Tests register and sign in a lot from one address.
            register_max_per_hour: 0,
            ..Config::default()
        };
        configure(&mut config);
        let app = App::start(&config).await.expect("start server");
        let addr = app.local_addrs()[0];
        TestServer {
            app: Some(app),
            addr,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("http client"),
            dir,
        }
    }

    /// Shuts the server down and hands back its directory.
    pub async fn stop(mut self) -> TempDir {
        if let Some(app) = self.app.take() {
            app.shutdown().await;
        }
        self.dir
    }

    /// The configuration [`TestServer::start_in`] uses for `dir`, for
    /// opening its database without the server.
    pub fn config_in(dir: &TempDir) -> Config {
        Config {
            domain: DOMAIN.to_owned(),
            listen: vec!["127.0.0.1:0".to_owned()],
            database_path: dir.path().join("resonator.db"),
            register_max_per_hour: 0,
            ..Config::default()
        }
    }

    pub fn app(&self) -> &App {
        self.app.as_ref().expect("server running")
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// The origin a browser on the server's own pages would send.
    pub fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub async fn shutdown(mut self) {
        if let Some(app) = self.app.take() {
            app.shutdown().await;
        }
    }

    pub async fn register(&self, name: &str, password: &str) -> reqwest::Response {
        self.http
            .post(self.url("/auth/register"))
            .json(&json!({"address": name, "password": password}))
            .send()
            .await
            .expect("register request")
    }

    pub async fn sign_in_response(&self, name: &str, password: &str) -> reqwest::Response {
        self.http
            .post(self.url("/auth/signin"))
            .json(&json!({"address": name, "password": password}))
            .send()
            .await
            .expect("sign-in request")
    }

    /// Registers (if needed) and signs in; returns the `Cookie` header
    /// value of the session.
    pub async fn user(&self, name: &str) -> String {
        let password = format!("{name}-password");
        let registered = self.register(name, &password).await;
        assert!(
            registered.status() == 200 || registered.status() == 403,
            "register: {}",
            registered.status()
        );
        let response = self.sign_in_response(name, &password).await;
        assert_eq!(response.status(), 200, "sign in {name}");
        session_cookie(&response).expect("session cookie")
    }

    pub async fn get(&self, path: &str, cookie: Option<&str>) -> reqwest::Response {
        let mut request = self.http.get(self.url(path));
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        request.send().await.expect("get request")
    }

    pub async fn get_bearer(&self, path: &str, token: &str) -> reqwest::Response {
        self.http
            .get(self.url(path))
            .bearer_auth(token)
            .send()
            .await
            .expect("get request")
    }

    /// The value of a metric from `/metrics`.
    pub async fn metric(&self, name: &str) -> i64 {
        let text = self.get("/metrics", None).await.text().await.unwrap();
        text.lines()
            .find_map(|line| line.strip_prefix(&format!("resonator_{name} ")))
            .unwrap_or_else(|| panic!("no metric {name} in:\n{text}"))
            .parse()
            .unwrap()
    }

    /// Waits until `/metrics` reports `value` for a metric.
    pub async fn await_metric(&self, name: &str, value: i64) {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            let current = self.metric(name).await;
            if current == value {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "metric {name} is {current}, expected {value}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Creates an access token for the session; returns the token.
    pub async fn create_token(&self, cookie: &str, name: &str, scopes: &[&str]) -> (String, i64) {
        let response = self
            .http
            .post(self.url("/auth/tokens"))
            .header("Cookie", cookie)
            .json(&json!({"name": name, "scopes": scopes}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201);
        let body: Value = response.json().await.unwrap();
        (
            body["token"].as_str().unwrap().to_owned(),
            body["id"].as_i64().unwrap(),
        )
    }
}

/// The `name=value` of the session cookie a response sets, if it sets a
/// non-empty one.
pub fn session_cookie(response: &reqwest::Response) -> Option<String> {
    let header = response.headers().get("set-cookie")?.to_str().ok()?;
    let pair = header.split(';').next()?;
    let (name, value) = pair.split_once('=')?;
    (name == "WSESSIONID" && !value.is_empty()).then(|| pair.to_owned())
}

/// A message received on a WebSocket.
#[derive(Debug, Clone, PartialEq)]
pub struct Received {
    pub kind: String,
    pub seq: i32,
    pub body: Value,
    /// The frame as it came.
    pub text: String,
}

impl Received {
    pub fn decode<M: JsonMessage>(&self) -> M {
        assert_eq!(self.kind, M::NAME, "unexpected message {}", self.text);
        json::from_value(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.text))
    }

    /// The error text if this is a failed `RpcFinished`.
    pub fn failure(&self) -> Option<String> {
        if self.kind != "RpcFinished" {
            return None;
        }
        let finished: RpcFinished = self.decode();
        finished
            .failed
            .unwrap_or(false)
            .then(|| finished.error_text.unwrap_or_default())
    }
}

/// What a WebSocket yields next.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Message(Received),
    /// A close frame with its code and reason, or the bare end of the
    /// stream.
    Closed(Option<(u16, String)>),
}

pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A client of the wave protocol.
pub struct WaveClient {
    pub socket: Socket,
    next_seq: i32,
    /// Messages read while waiting for another one.
    inbox: VecDeque<Received>,
    closed: Option<Option<(u16, String)>>,
}

/// Tries to open the WebSocket with the given request headers.
pub async fn try_connect(
    server: &TestServer,
    headers: &[(&'static str, &str)],
) -> Result<WaveClient, tokio_tungstenite::tungstenite::Error> {
    let mut request = format!("ws://{}/socket", server.addr)
        .into_client_request()
        .expect("request");
    for (name, value) in headers {
        request
            .headers_mut()
            .insert(*name, value.parse().expect("header value"));
    }
    let (socket, _) = tokio_tungstenite::connect_async(request).await?;
    Ok(WaveClient {
        socket,
        next_seq: 1,
        inbox: VecDeque::new(),
        closed: None,
    })
}

impl WaveClient {
    pub async fn connect(server: &TestServer, headers: &[(&'static str, &str)]) -> Self {
        try_connect(server, headers).await.expect("websocket")
    }

    /// Connects as the user of a session cookie.
    pub async fn with_cookie(server: &TestServer, cookie: &str) -> Self {
        Self::connect(server, &[("Cookie", cookie)]).await
    }

    pub async fn send_text(&mut self, text: &str) {
        self.socket
            .send(Message::Text(text.into()))
            .await
            .expect("send");
    }

    /// Sends a request under a fresh sequence number, which is returned.
    pub async fn send<M: JsonMessage>(&mut self, message: &M) -> i32 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.send_as(seq, message).await;
        seq
    }

    pub async fn send_as<M: JsonMessage>(&mut self, seq: i32, message: &M) {
        let text = json::envelope(seq, message).expect("envelope");
        self.send_text(&text).await;
    }

    /// Reads the next frame from the socket, answering pings on the way.
    async fn read(&mut self) -> Frame {
        if let Some(closed) = &self.closed {
            return Frame::Closed(closed.clone());
        }
        loop {
            let next = tokio::time::timeout(TIMEOUT, self.socket.next())
                .await
                .expect("timed out waiting for a frame");
            let closed = match next {
                Some(Ok(Message::Text(text))) => {
                    let envelope = json::Envelope::parse(text.as_str())
                        .unwrap_or_else(|e| panic!("{e}: {text}"));
                    return Frame::Message(Received {
                        kind: envelope.message_type,
                        seq: envelope.sequence_number,
                        body: envelope.message,
                        text: text.to_string(),
                    });
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                Some(Ok(Message::Close(frame))) => {
                    frame.map(|frame| (u16::from(frame.code), frame.reason.to_string()))
                }
                Some(Ok(other)) => panic!("unexpected frame {other:?}"),
                Some(Err(_)) | None => None,
            };
            self.closed = Some(closed.clone());
            return Frame::Closed(closed);
        }
    }

    /// The next frame, whatever it is.
    pub async fn next(&mut self) -> Frame {
        match self.inbox.pop_front() {
            Some(message) => Frame::Message(message),
            None => self.read().await,
        }
    }

    /// The next message with this sequence number; others are kept for
    /// later.
    pub async fn next_for(&mut self, seq: i32) -> Received {
        if let Some(index) = self.inbox.iter().position(|message| message.seq == seq) {
            return self.inbox.remove(index).expect("index in range");
        }
        loop {
            match self.read().await {
                Frame::Message(message) if message.seq == seq => return message,
                Frame::Message(message) => self.inbox.push_back(message),
                Frame::Closed(reason) => panic!("socket closed ({reason:?}) waiting for {seq}"),
            }
        }
    }

    /// Waits for the connection to close; returns the close code and
    /// reason if the server sent them.
    pub async fn closed(&mut self) -> Option<(u16, String)> {
        loop {
            if let Frame::Closed(reason) = self.next().await {
                return reason;
            }
        }
    }

    /// Asserts that nothing but commit notices arrives for a little while.
    pub async fn expect_silence(&mut self) {
        let quiet_until = tokio::time::Instant::now() + Duration::from_millis(150);
        loop {
            let message = match self.inbox.pop_front() {
                Some(message) => message,
                None => match tokio::time::timeout_at(quiet_until, self.socket.next()).await {
                    Err(_) => return,
                    Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
                    Ok(Some(Ok(Message::Text(text)))) => {
                        let envelope = json::Envelope::parse(text.as_str()).expect("envelope");
                        Received {
                            kind: envelope.message_type,
                            seq: envelope.sequence_number,
                            body: envelope.message,
                            text: text.to_string(),
                        }
                    }
                    Ok(other) => panic!("expected silence, got {other:?}"),
                },
            };
            assert!(
                message.kind == "ProtocolWaveletUpdate"
                    && is_bare_commit(&message.decode::<ProtocolWaveletUpdate>()),
                "expected silence, got {}",
                message.text
            );
        }
    }

    pub async fn authenticate(&mut self, token: &str) -> Received {
        let seq = self
            .send(&ProtocolAuthenticate {
                token: Some(token.to_owned()),
            })
            .await;
        self.next_for(seq).await
    }

    /// Sends an open request; returns its sequence number.
    pub async fn send_open(&mut self, wave_id: &str, known: Vec<WaveletVersion>) -> i32 {
        self.send(&ProtocolOpenRequest {
            participant_id: Some(String::new()),
            wave_id: Some(wave_id.to_owned()),
            wavelet_id_prefix: vec![String::new()],
            known_wavelet: known,
        })
        .await
    }

    /// Opens a wave view and reads up to and including the marker.
    pub async fn open(&mut self, wave_id: &str, known: Vec<WaveletVersion>) -> View {
        let seq = self.send_open(wave_id, known).await;
        let mut initial = Vec::new();
        loop {
            let update: ProtocolWaveletUpdate = self.next_for(seq).await.decode();
            let marker = update.marker == Some(true);
            initial.push(update);
            if marker {
                break;
            }
        }
        let channel_id = initial[0].channel_id.clone().expect("channel id");
        View {
            seq,
            channel_id,
            initial,
        }
    }

    /// The next update of a view that is not a bare commit notice.
    pub async fn next_update(&mut self, view: &View) -> ProtocolWaveletUpdate {
        loop {
            let update: ProtocolWaveletUpdate = self.next_for(view.seq).await.decode();
            if !is_bare_commit(&update) {
                return update;
            }
        }
    }

    pub async fn submit_delta(
        &mut self,
        wavelet: &WaveletName,
        delta: ProtocolWaveletDelta,
        channel_id: Option<&str>,
    ) -> Received {
        let seq = self
            .send(&ProtocolSubmitRequest {
                wavelet_name: Some(wire_name(wavelet)),
                delta: Some(delta),
                channel_id: channel_id.map(str::to_owned),
            })
            .await;
        self.next_for(seq).await
    }

    /// Submits operations by `author` at `target`; returns the response.
    pub async fn submit(
        &mut self,
        author: &ParticipantId,
        wavelet: &WaveletName,
        target: &HashedVersion,
        ops: Vec<WaveletOperation>,
        channel_id: Option<&str>,
    ) -> ProtocolSubmitResponse {
        let delta = serialize_delta(&WaveletDelta::new(author.clone(), target.clone(), ops))
            .expect("delta");
        self.submit_delta(wavelet, delta, channel_id).await.decode()
    }

    /// [`WaveClient::submit`] that must succeed; returns the version after
    /// the delta.
    pub async fn submit_ok(
        &mut self,
        author: &ParticipantId,
        wavelet: &WaveletName,
        target: &HashedVersion,
        ops: Vec<WaveletOperation>,
        channel_id: Option<&str>,
    ) -> HashedVersion {
        let response = self.submit(author, wavelet, target, ops, channel_id).await;
        assert_eq!(response.error_message, None);
        deserialize_hashed_version(
            response
                .hashed_version_after_application
                .as_ref()
                .expect("version after application"),
        )
        .expect("hashed version")
    }

    pub async fn cancel(&mut self, seq: i32) {
        self.send_as(seq, &CancelRpc {}).await;
    }
}

/// An open wave view: the request's sequence number, the channel id, and
/// the messages up to the marker.
#[derive(Debug, Clone)]
pub struct View {
    pub seq: i32,
    pub channel_id: String,
    pub initial: Vec<ProtocolWaveletUpdate>,
}

impl View {
    /// The initial messages without the marker.
    pub fn wavelets(&self) -> &[ProtocolWaveletUpdate] {
        &self.initial[..self.initial.len() - 1]
    }
}

/// Whether an update is nothing but a commit notice.
pub fn is_bare_commit(update: &ProtocolWaveletUpdate) -> bool {
    update.commit_notice.is_some()
        && update.applied_delta.is_empty()
        && update.snapshot.is_none()
        && update.resulting_version.is_none()
}

/// A wavelet name as the protocol writes it.
pub fn wire_name(wavelet: &WaveletName) -> String {
    ModernIdSerialiser
        .serialise_wavelet_name(wavelet)
        .expect("wavelet name")
}

pub fn version_zero(wavelet: &WaveletName) -> HashedVersion {
    HashedVersionFactory::create_version_zero(wavelet)
}

/// The version an update leaves the wavelet at.
pub fn resulting_version(update: &ProtocolWaveletUpdate) -> HashedVersion {
    deserialize_hashed_version(
        update
            .resulting_version
            .as_ref()
            .expect("resulting version"),
    )
    .expect("hashed version")
}
