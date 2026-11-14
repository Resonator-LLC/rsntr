//! Two (or three) federating servers in one process, each on its own
//! temporary database, talking over iroh on 127.0.0.1 with static peers,
//! no relay and no discovery: replication of hosted wavelets to remote
//! participants' servers, submissions forwarded to the host, catch-up
//! after downtime, gap filling, and the refusals.

#[path = "../../resonator-agents/tests/common/fake_provider.rs"]
mod fake_provider;

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use resonator_agents::Integrations;
use resonator_federation::dns::StaticTxtResolver;
use resonator_federation::endpoint::{RequestError, new_request_id};
use resonator_federation::signing::{Ed25519DeltaSigner, signer_info};
use resonator_federation::{Federation, FederationSetup, ServerKey, Tuning};
use resonator_model::id::{DualIdSerialiser, IdSerialiser, IdUriCodec, ParticipantId, WaveletName};
use resonator_model::operation::{WaveletDelta, WaveletOperation, WaveletOperationContext};
use resonator_model::proto::federation::{ProtocolAppliedWaveletDelta, ProtocolSignedDelta};
use resonator_model::proto::federation_error::federation_error::Code;
use resonator_model::proto::federation_rpc::{
    FetchAttachment, History, PostSignerInfo, Submit, Update, federation_request,
    federation_response,
};
use resonator_model::proto::json::{Envelope, envelope};
use resonator_model::proto::waveclient_rpc::ProtocolOpenRequest;
use resonator_model::proto::{encode, serialize_delta, serialize_hashed_version};
use resonator_server::app::App;
use resonator_server::config::{
    AgentTools, AiAgentConfig, AiProviderConfig, Config, FederationMode, FederationPeer,
};
use resonator_server::storage::OutboxStatus;
use resonator_server::waveserver::WaveServerOptions;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;

use fake_provider::{FakeProvider, Reply};

const A: &str = "a.example";
const TIMEOUT: Duration = Duration::from_secs(20);

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn tuning() -> Tuning {
    Tuning {
        retry_min: Duration::from_millis(50),
        retry_max: Duration::from_millis(400),
        commit_wait: Duration::from_secs(5),
        ..Tuning::default()
    }
}

/// What a server needs before it starts: its directory, key, domain and
/// federation port.
struct Site {
    dir: TempDir,
    key: ServerKey,
    domain: String,
    port: u16,
}

impl Site {
    /// `domain` None: the server's key domain.
    fn new(domain: Option<&str>) -> Site {
        let dir = tempfile::tempdir().unwrap();
        let key = ServerKey::load_or_create(&dir.path().join("federation.key")).unwrap();
        let domain = domain.map_or_else(|| key.key_domain(), str::to_owned);
        Site {
            dir,
            key,
            domain,
            port: free_udp_port(),
        }
    }

    fn addr(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// A static peer entry naming this site.
    fn peer(&self) -> FederationPeer {
        FederationPeer {
            domain: self.domain.clone(),
            key: self.key.key_domain(),
            addr: self.addr(),
        }
    }

    /// The config of this site, federating with `peers`.
    fn config(&self, peers: &[&Site]) -> Config {
        let mut config = Config {
            domain: self.domain.clone(),
            listen: vec!["127.0.0.1:0".to_owned()],
            database_path: self.dir.path().join("resonator.db"),
            register_max_per_hour: 0,
            ..Config::default()
        };
        config.federation.enabled = true;
        config.federation.mode = FederationMode::Allowlist;
        config.federation.allow = peers.iter().map(|p| p.domain.clone()).collect();
        config.federation.relay = "off".to_owned();
        config.federation.discovery = "off".to_owned();
        config.federation.listen = self.addr();
        config.federation.request_timeout_secs = 2;
        config.federation.peers = peers.iter().map(|p| p.peer()).collect();
        assert_eq!(
            config.federation_key_path(),
            self.dir.path().join("federation.key"),
            "the key file the server will load"
        );
        config
    }
}

struct Node {
    domain: String,
    app: Option<App>,
    integrations: Option<Integrations>,
    federation: Option<Federation>,
    addr: SocketAddr,
    http: reqwest::Client,
}

impl Node {
    async fn start(config: Config) -> Node {
        let (mut node, setup) = Node::serve(config).await;
        node.federate(setup).await;
        node
    }

    /// Serves, subscribed to the wave bus as `resonator serve` is, but
    /// without the federation endpoint yet.
    async fn serve(config: Config) -> (Node, FederationSetup) {
        let mut setup = FederationSetup::load(&config)
            .expect("federation setup")
            .expect("federation enabled")
            .with_tuning(tuning())
            .with_resolver(Arc::new(StaticTxtResolver::new()));
        let mut options = WaveServerOptions::from_config(&config);
        options.delta_signer = Some(setup.delta_signer());
        let mut prepared = App::prepare_with(&config, options).await.expect("prepare");
        setup.subscribe(prepared.server());
        setup.serve_app(prepared.state());
        let (integrations, routes) = Integrations::mount(&mut prepared).await.expect("mount");
        let app = prepared.serve(routes).await.expect("serve");
        let node = Node {
            domain: config.domain.clone(),
            addr: app.local_addrs()[0],
            app: Some(app),
            integrations: Some(integrations),
            federation: None,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
        };
        (node, setup)
    }

    /// Starts the federation endpoint, the fan-out and the rest.
    async fn federate(&mut self, setup: FederationSetup) {
        let federation = setup.start(self.server()).await.expect("federation");
        self.federation = Some(federation);
    }

    async fn stop(mut self) {
        if let Some(federation) = self.federation.take() {
            federation.shutdown().await;
        }
        if let Some(integrations) = self.integrations.take() {
            integrations.shutdown().await;
        }
        if let Some(app) = self.app.take() {
            app.shutdown().await;
        }
    }

    fn federation(&self) -> &Federation {
        self.federation.as_ref().unwrap()
    }

    fn server(&self) -> &resonator_server::waveserver::WaveServer {
        self.app.as_ref().unwrap().server()
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn address(&self, name: &str) -> String {
        format!("{name}@{}", self.domain)
    }

    /// Registers and signs in; returns the session cookie.
    async fn cookie(&self, name: &str) -> String {
        let password = format!("{name}-password-long");
        let registered = self
            .http
            .post(self.url("/auth/register"))
            .json(&json!({"address": name, "password": password}))
            .send()
            .await
            .unwrap();
        assert!(registered.status() == 200 || registered.status() == 403);
        let response = self
            .http
            .post(self.url("/auth/signin"))
            .json(&json!({"address": name, "password": password}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "sign in {name}");
        let header = response.headers()["set-cookie"].to_str().unwrap();
        header.split(';').next().unwrap().to_owned()
    }

    /// A read-write token of a new user.
    async fn token(&self, name: &str) -> String {
        let cookie = self.cookie(name).await;
        self.token_for(&cookie).await
    }

    async fn token_for(&self, cookie: &str) -> String {
        let response = self
            .http
            .post(self.url("/auth/tokens"))
            .header("Cookie", cookie)
            .header("Origin", format!("http://{}", self.addr))
            .json(&json!({"name": "t", "scopes": ["read", "write"]}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201);
        let body: Value = response.json().await.unwrap();
        body["token"].as_str().unwrap().to_owned()
    }

    async fn data_api(&self, token: &str, operations: Value) -> Value {
        let response = self
            .http
            .post(self.url("/robot/dataapi"))
            .bearer_auth(token)
            .json(&operations)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body: Value = response.json().await.unwrap();
        assert_eq!(status, 200, "{body}");
        body
    }

    /// Runs Data API operations that must all succeed.
    async fn run(&self, token: &str, operations: Value) -> Value {
        let body = self.data_api(token, operations).await;
        for response in body.as_array().unwrap() {
            assert!(response.get("error").is_none(), "{body}");
        }
        body
    }

    /// Creates a wave hosted here; returns (wave id, wavelet id, root blip).
    async fn create_wave(&self, token: &str, participants: &[&str]) -> (String, String, String) {
        let body = self
            .run(
                token,
                json!([{
                    "id": "create",
                    "method": "robot.createWavelet",
                    "params": {"waveletData": {
                        "waveId": format!("{}/TBD_wave", self.domain),
                        "waveletId": format!("{}/conv+root", self.domain),
                        "rootBlipId": "TBD_blip",
                        "participants": participants,
                    }}
                }]),
            )
            .await;
        let data = &body[0]["data"];
        (
            data["waveId"].as_str().unwrap().to_owned(),
            data["waveletId"].as_str().unwrap().to_owned(),
            data["blipId"].as_str().unwrap().to_owned(),
        )
    }

    /// Appends text to a blip.
    async fn append(&self, token: &str, wave: &str, wavelet: &str, blip: &str, text: &str) {
        self.run(token, json!([modify_append(wave, wavelet, blip, text)]))
            .await;
    }

    /// The text of the wavelet's blips, by blip id, as its user sees it.
    async fn texts(&self, token: &str, wave: &str, wavelet: &str) -> Option<Value> {
        let body = self
            .data_api(
                token,
                json!([{"id": "f", "method": "robot.fetchWave",
                        "params": {"waveId": wave, "waveletId": wavelet}}]),
            )
            .await;
        let blips = body[0]["data"]["blips"].as_object()?;
        Some(
            blips
                .iter()
                .map(|(id, blip)| (id.clone(), blip["content"].clone()))
                .collect::<serde_json::Map<_, _>>()
                .into(),
        )
    }

    async fn version(&self, wave: &str, wavelet: &str) -> u64 {
        self.server()
            .current_version(&name(wave, wavelet))
            .await
            .unwrap()
            .version()
    }

    /// Whether `cookie`'s user finds the wave with the search.
    async fn search_finds(&self, cookie: &str, wave: &str) -> bool {
        let response = self
            .http
            .get(self.url("/search/"))
            .query(&[("query", "in:inbox"), ("index", "0"), ("numResults", "50")])
            .header("Cookie", cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let wave_local = wave.split('/').nth(1).unwrap();
        response.text().await.unwrap().contains(wave_local)
    }
}

fn modify_append(wave: &str, wavelet: &str, blip: &str, text: &str) -> Value {
    json!({"id": "m", "method": "document.modify", "params": {
        "waveId": wave, "waveletId": wavelet, "blipId": blip,
        "modifyAction": {"modifyHow": "INSERT_AFTER", "values": [text]}
    }})
}

fn add_participant(wave: &str, wavelet: &str, who: &str) -> Value {
    json!({"id": "a", "method": "wavelet.addParticipant", "params": {
        "waveId": wave, "waveletId": wavelet, "participantId": who
    }})
}

fn name(wave: &str, wavelet: &str) -> WaveletName {
    WaveletName::of(
        DualIdSerialiser::MODERN.deserialise_wave_id(wave).unwrap(),
        DualIdSerialiser::MODERN
            .deserialise_wavelet_id(wavelet)
            .unwrap(),
    )
}

async fn wait_until<F, Fut>(what: &str, mut condition: F)
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
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Waits until both servers show the same wavelet at the same version.
async fn converged(
    (a, a_token): (&Node, &str),
    (b, b_token): (&Node, &str),
    wave: &str,
    wavelet: &str,
) -> Value {
    wait_until("convergence", || async {
        a.version(wave, wavelet).await == b.version(wave, wavelet).await
            && a.texts(a_token, wave, wavelet).await == b.texts(b_token, wave, wavelet).await
    })
    .await;
    a.texts(a_token, wave, wavelet).await.unwrap()
}

/// Two servers: A at a DNS domain, B at its key domain, each allowing the
/// other.
async fn pair() -> (Site, Site, Node, Node) {
    let site_a = Site::new(Some(A));
    let site_b = Site::new(None);
    let a = Node::start(site_a.config(&[&site_b])).await;
    let b = Node::start(site_b.config(&[&site_a])).await;
    (site_a, site_b, a, b)
}

fn code(result: Result<federation_response::Result, RequestError>) -> Code {
    match result {
        Ok(federation_response::Result::Error(error)) => error.code(),
        Err(RequestError::Remote { error, .. }) => error.code(),
        Ok(other) => panic!("expected an error, got {other:?}"),
        Err(other) => panic!("expected a remote error, got {other}"),
    }
}

/// A wave socket of a signed-in user.
struct Socket {
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

impl Socket {
    async fn connect(node: &Node, cookie: &str) -> Socket {
        let mut request = format!("ws://{}/socket", node.addr)
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("Cookie", cookie.parse().unwrap());
        let (socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        Socket { socket }
    }

    async fn open(&mut self, wave: &str) {
        let open = ProtocolOpenRequest {
            participant_id: Some(String::new()),
            wave_id: Some(wave.to_owned()),
            wavelet_id_prefix: vec![String::new()],
            known_wavelet: Vec::new(),
        };
        let text = envelope(1, &open).unwrap();
        self.socket.send(Message::Text(text.into())).await.unwrap();
    }

    /// Reads frames until one contains `needle`.
    async fn wait_for(&mut self, needle: &str) -> String {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            let frame = tokio::time::timeout_at(deadline, self.socket.next())
                .await
                .unwrap_or_else(|_| panic!("no frame with {needle}"))
                .expect("socket open")
                .unwrap();
            if let Message::Text(text) = frame {
                let parsed = Envelope::parse(text.as_str()).unwrap();
                assert_ne!(parsed.message_type, "RpcFinished", "{text}");
                if text.contains(needle) {
                    return text.to_string();
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_participants_get_a_replica_over_search_and_the_socket() {
    let (_site_a, _site_b, a, b) = pair().await;
    let alice = a.token("alice").await;
    let bob_cookie = b.cookie("bob").await;
    let bob = b.token_for(&bob_cookie).await;
    let bob_address = b.address("bob");

    let (wave, wavelet, root) = a.create_wave(&alice, &[&a.address("alice")]).await;
    a.append(&alice, &wave, &wavelet, &root, "Hello from A")
        .await;
    a.run(
        &alice,
        json!([add_participant(&wave, &wavelet, &bob_address)]),
    )
    .await;

    wait_until("replica on B", || async {
        b.version(&wave, &wavelet).await == a.version(&wave, &wavelet).await
    })
    .await;
    let texts = converged((&a, &alice), (&b, &bob), &wave, &wavelet).await;
    assert!(texts.to_string().contains("Hello from A"), "{texts}");
    assert!(
        !b.server().is_local_wavelet(&name(&wave, &wavelet)),
        "a replica"
    );
    wait_until("bob finds the wave", || b.search_finds(&bob_cookie, &wave)).await;

    let mut socket = Socket::connect(&b, &bob_cookie).await;
    socket.open(&wave).await;
    socket.wait_for("Hello from A").await;
    a.append(&alice, &wave, &wavelet, &root, " and live").await;
    socket.wait_for(" and live").await;

    // Delivered and acknowledged.
    wait_until("outbox delivered", || async {
        let outbox = a.federation().outbox().await.unwrap();
        !outbox.is_empty() && outbox.iter().all(|e| e.status == OutboxStatus::Delivered)
    })
    .await;
    b.stop().await;
    a.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_submissions_apply_at_the_host_and_both_converge() {
    let (_site_a, _site_b, a, b) = pair().await;
    let alice = a.token("alice").await;
    let bob = b.token("bob").await;
    let (wave, wavelet, root) = a.create_wave(&alice, &[&a.address("alice")]).await;
    a.run(
        &alice,
        json!([add_participant(&wave, &wavelet, &b.address("bob"))]),
    )
    .await;
    wait_until("replica on B", || async {
        b.texts(&bob, &wave, &wavelet).await.is_some()
    })
    .await;

    b.append(&bob, &wave, &wavelet, &root, "bob was here").await;
    wait_until("A applied bob's edit", || async {
        a.texts(&alice, &wave, &wavelet)
            .await
            .is_some_and(|t| t.to_string().contains("bob was here"))
    })
    .await;

    // Concurrent edits from both sides.
    let (left, right) = tokio::join!(
        async {
            for i in 0..5 {
                a.append(&alice, &wave, &wavelet, &root, &format!("[a{i}]"))
                    .await;
            }
        },
        async {
            for i in 0..5 {
                b.append(&bob, &wave, &wavelet, &root, &format!("[b{i}]"))
                    .await;
            }
        }
    );
    let ((), ()) = (left, right);
    let texts = converged((&a, &alice), (&b, &bob), &wave, &wavelet).await;
    let text = texts.to_string();
    for i in 0..5 {
        assert!(text.contains(&format!("[a{i}]")), "{text}");
        assert!(text.contains(&format!("[b{i}]")), "{text}");
    }
    b.stop().await;
    a.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_offline_server_catches_up_across_restarts() {
    let (site_a, site_b, a, b) = pair().await;
    let alice = a.token("alice").await;
    let bob = b.token("bob").await;
    let (wave, wavelet, root) = a.create_wave(&alice, &[&a.address("alice")]).await;
    a.run(
        &alice,
        json!([add_participant(&wave, &wavelet, &b.address("bob"))]),
    )
    .await;
    converged((&a, &alice), (&b, &bob), &wave, &wavelet).await;

    // B goes away; A keeps editing and keeps the notification.
    b.stop().await;
    for i in 0..3 {
        a.append(&alice, &wave, &wavelet, &root, &format!("<{i}>"))
            .await;
    }
    wait_until("a failed attempt", || async {
        a.federation()
            .outbox()
            .await
            .unwrap()
            .iter()
            .any(|e| e.status == OutboxStatus::Pending && e.attempts > 0)
    })
    .await;

    // A restarts too; the outbox survives.
    a.stop().await;
    let a = Node::start(site_a.config(&[&site_b])).await;
    let pending = a.federation().outbox().await.unwrap();
    assert!(
        pending.iter().any(|e| e.status == OutboxStatus::Pending),
        "{pending:?}"
    );
    a.append(&alice, &wave, &wavelet, &root, "<after restart>")
        .await;

    let b = Node::start(site_b.config(&[&site_a])).await;
    let texts = converged((&a, &alice), (&b, &bob), &wave, &wavelet).await;
    assert!(texts.to_string().contains("<after restart>"), "{texts}");
    assert!(texts.to_string().contains("<0>"), "{texts}");
    b.stop().await;
    a.stop().await;
}

/// The applied deltas of a hosted wavelet from `from` on.
async fn applied_since(
    node: &Node,
    name: &WaveletName,
    from: u64,
) -> Vec<resonator_server::storage::AppliedDelta> {
    let server = node.server();
    let start = server.hashed_version_at(name, from).await.unwrap().unwrap();
    let end = server.current_version(name).await.unwrap();
    server
        .get_applied_history(name, &start, &end, None)
        .await
        .unwrap()
}

fn update(name: &WaveletName, deltas: Vec<Vec<u8>>) -> federation_request::Op {
    federation_request::Op::Update(Update {
        wavelet_name: Some(IdUriCodec::wavelet_name_to_uri(name)),
        applied_delta: deltas,
        epoch_start: Vec::new(),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gaps_are_filled_with_history_and_tampered_deltas_are_rejected() {
    let (_site_a, _site_b, a, b) = pair().await;
    let alice = a.token("alice").await;
    let bob = b.token("bob").await;
    let (wave, wavelet, root) = a.create_wave(&alice, &[&a.address("alice")]).await;
    a.run(
        &alice,
        json!([add_participant(&wave, &wavelet, &b.address("bob"))]),
    )
    .await;
    converged((&a, &alice), (&b, &bob), &wave, &wavelet).await;
    let wavelet_name = name(&wave, &wavelet);

    // Held notifications: B hears only of the last of three deltas.
    a.federation().hold_outbox(true);
    let before = b.version(&wave, &wavelet).await;
    for i in 0..3 {
        a.append(&alice, &wave, &wavelet, &root, &format!("(gap{i})"))
            .await;
    }
    let deltas = applied_since(&a, &wavelet_name, before).await;
    assert!(deltas.len() >= 3);

    // A tampered applied delta (wrong hash at B's version) and a bad
    // signature are refused, and B stays where it was.
    let mut tampered: ProtocolAppliedWaveletDelta = deltas[0].message().clone();
    let mut wrong = b.server().current_version(&wavelet_name).await.unwrap();
    wrong = resonator_model::version::HashedVersion::of(wrong.version(), vec![7; 20]);
    tampered.hashed_version_applied_at = Some(serialize_hashed_version(&wrong).unwrap());
    let answer = a
        .federation()
        .request(
            &b.domain,
            update(&wavelet_name, vec![encode(&tampered).unwrap()]),
        )
        .await;
    assert_eq!(code(answer), Code::NotAcceptable);
    let mut forged: ProtocolAppliedWaveletDelta = deltas[0].message().clone();
    let signed = forged.signed_original_delta.as_mut().unwrap();
    signed.signature[0].signature_bytes.as_mut().unwrap()[0] ^= 1;
    let answer = a
        .federation()
        .request(
            &b.domain,
            update(&wavelet_name, vec![encode(&forged).unwrap()]),
        )
        .await;
    assert_eq!(code(answer), Code::NotAcceptable);
    assert_eq!(b.version(&wave, &wavelet).await, before);

    // The last delta alone: B fetches the ones before it.
    let last = deltas.last().unwrap().bytes().to_vec();
    let answer = a
        .federation()
        .request(&b.domain, update(&wavelet_name, vec![last]))
        .await
        .expect("update");
    assert!(
        matches!(answer, federation_response::Result::Ack(_)),
        "{answer:?}"
    );
    let texts = converged((&a, &alice), (&b, &bob), &wave, &wavelet).await;
    assert!(texts.to_string().contains("(gap0)"), "{texts}");

    // Released, the held notification finds B already there.
    a.federation().hold_outbox(false);
    wait_until("outbox delivered", || async {
        a.federation()
            .outbox()
            .await
            .unwrap()
            .iter()
            .all(|e| e.status == OutboxStatus::Delivered)
    })
    .await;
    converged((&a, &alice), (&b, &bob), &wave, &wavelet).await;
    b.stop().await;
    a.stop().await;
}

/// A delta of one no-op by `author` at the host's current version, signed
/// with `key`.
async fn signed_no_op(
    host: &Node,
    name: &WaveletName,
    author: &str,
    key: &ServerKey,
    version: Option<u64>,
) -> ProtocolSignedDelta {
    let author = ParticipantId::of(author).unwrap();
    let target = match version {
        Some(0) => host.server().version_zero(name),
        _ => host.server().current_version(name).await.unwrap(),
    };
    let delta = WaveletDelta::new(
        author.clone(),
        target,
        vec![WaveletOperation::no_op(WaveletOperationContext::new(
            author, 0, 1,
        ))],
    );
    let bytes = encode(&serialize_delta(&delta).unwrap()).unwrap();
    let signer = Ed25519DeltaSigner::new(key.secret().clone());
    ProtocolSignedDelta {
        signature: vec![signer.signature(&bytes)],
        delta: Some(bytes),
    }
}

fn submit(name: &WaveletName, signed: ProtocolSignedDelta) -> federation_request::Op {
    federation_request::Op::Submit(Submit {
        wavelet_name: Some(IdUriCodec::wavelet_name_to_uri(name)),
        signed_delta: Some(signed),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hosts_refuse_what_peers_may_not_do_and_apply_requests_once() {
    let site_a = Site::new(Some(A));
    let site_b = Site::new(None);
    let site_c = Site::new(None);
    // A allows B only; B allows A and C.
    let a = Node::start(site_a.config(&[&site_b])).await;
    let b = Node::start(site_b.config(&[&site_a, &site_c])).await;
    let c = Node::start(site_c.config(&[&site_a, &site_b])).await;
    let alice = a.token("alice").await;
    let bob = b.token("bob").await;
    let (wave, wavelet, _) = a.create_wave(&alice, &[&a.address("alice")]).await;
    let wavelet_name = name(&wave, &wavelet);
    let posted = b
        .federation()
        .request(
            A,
            federation_request::Op::PostSignerInfo(PostSignerInfo {
                signer_info: Some(signer_info(&site_b.key.public(), &b.domain, Vec::new())),
            }),
        )
        .await
        .expect("post signer info");
    assert!(matches!(posted, federation_response::Result::Ack(_)));

    // No participant of B's: no history, no submissions.
    let history = federation_request::Op::History(History {
        wavelet_name: Some(IdUriCodec::wavelet_name_to_uri(&wavelet_name)),
        start_version: Some(
            serialize_hashed_version(&a.server().version_zero(&wavelet_name)).unwrap(),
        ),
        end_version: Some(
            serialize_hashed_version(&a.server().current_version(&wavelet_name).await.unwrap())
                .unwrap(),
        ),
        length_limit: None,
    });
    assert_eq!(
        code(b.federation().request(A, history.clone()).await),
        Code::ItemNotFound
    );
    let bobs = signed_no_op(&a, &wavelet_name, &b.address("bob"), &site_b.key, None).await;
    assert_eq!(
        code(b.federation().request(A, submit(&wavelet_name, bobs)).await),
        Code::NotAuthorized
    );

    // A user of A's own, forged by B.
    let forged = signed_no_op(&a, &wavelet_name, &a.address("alice"), &site_b.key, None).await;
    assert_eq!(
        code(
            b.federation()
                .request(A, submit(&wavelet_name, forged))
                .await
        ),
        Code::NotAuthorized
    );

    // Bob on the wave: history now, but no deltas at version 0.
    a.run(
        &alice,
        json!([add_participant(&wave, &wavelet, &b.address("bob"))]),
    )
    .await;
    let answer = b.federation().request(A, history).await.expect("history");
    let federation_response::Result::History(history) = answer else {
        panic!("{answer:?}");
    };
    assert!(!history.applied_delta.is_empty());
    let at_zero = signed_no_op(&a, &wavelet_name, &b.address("bob"), &site_b.key, Some(0)).await;
    assert_eq!(
        code(
            b.federation()
                .request(A, submit(&wavelet_name, at_zero))
                .await
        ),
        Code::NotAuthorized
    );

    // The same request id twice: applied once, answered alike.
    let before = a.version(&wave, &wavelet).await;
    let bobs = signed_no_op(&a, &wavelet_name, &b.address("bob"), &site_b.key, None).await;
    let id = new_request_id();
    let first = b
        .federation()
        .request_with_id(A, id.clone(), submit(&wavelet_name, bobs.clone()))
        .await
        .expect("submit");
    let again = b
        .federation()
        .request_with_id(A, id, submit(&wavelet_name, bobs))
        .await
        .expect("submit again");
    assert!(
        matches!(first, federation_response::Result::Submit(_)),
        "{first:?}"
    );
    assert_eq!(first, again);
    assert_eq!(a.version(&wave, &wavelet).await, before + 1);
    converged((&a, &alice), (&b, &bob), &wave, &wavelet).await;

    // Copies of one request at the same time: applied once, every copy
    // answered with the first answer, and so is a retry afterwards.
    let before = a.version(&wave, &wavelet).await;
    let bobs = signed_no_op(&a, &wavelet_name, &b.address("bob"), &site_b.key, None).await;
    let id = new_request_id();
    let answers = futures_util::future::join_all((0..8).map(|_| {
        b.federation()
            .request_with_id(A, id.clone(), submit(&wavelet_name, bobs.clone()))
    }))
    .await;
    let first = answers[0].as_ref().expect("submit").clone();
    assert!(
        matches!(first, federation_response::Result::Submit(_)),
        "{first:?}"
    );
    for answer in &answers {
        assert_eq!(answer.as_ref().expect("submit"), &first);
    }
    let retried = b
        .federation()
        .request_with_id(A, id, submit(&wavelet_name, bobs))
        .await
        .expect("retry");
    assert_eq!(retried, first);
    assert_eq!(a.version(&wave, &wavelet).await, before + 1);
    converged((&a, &alice), (&b, &bob), &wave, &wavelet).await;

    // C is not on A's allowlist.
    let probe = c
        .federation()
        .request(A, federation_request::Op::History(History::default()))
        .await;
    assert!(probe.is_err(), "{probe:?}");

    // Updates of wavelets B hosts, or from a server that is not the host.
    let (b_wave, b_wavelet, _) = b.create_wave(&bob, &[&b.address("bob")]).await;
    let b_name = name(&b_wave, &b_wavelet);
    let b_deltas = applied_since(&b, &b_name, 0).await;
    let b_bytes: Vec<Vec<u8>> = b_deltas.iter().map(|d| d.bytes().to_vec()).collect();
    assert_eq!(
        code(
            a.federation()
                .request(&b.domain, update(&b_name, b_bytes.clone()))
                .await
        ),
        Code::BadRequest
    );
    let a_deltas = applied_since(&a, &wavelet_name, 0).await;
    let a_bytes: Vec<Vec<u8>> = a_deltas.iter().map(|d| d.bytes().to_vec()).collect();
    assert_eq!(
        code(
            c.federation()
                .request(&b.domain, update(&wavelet_name, a_bytes))
                .await
        ),
        Code::NotAuthorized
    );
    c.stop().await;
    b.stop().await;
    a.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_the_remote_server_answers_on_a_hosted_wave() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::text(&["Hello from the helper."]));
    fake.push(Reply::text(&["Hello from the helper."]));
    let site_a = Site::new(Some(A));
    let site_b = Site::new(None);
    let a = Node::start(site_a.config(&[&site_b])).await;
    let mut config_b = site_b.config(&[&site_a]);
    config_b.ai.providers.insert(
        "fake".to_owned(),
        AiProviderConfig {
            base_url: fake.base_url(),
            max_retries: 0,
            first_byte_timeout_secs: 5,
            idle_timeout_secs: 5,
            ..AiProviderConfig::default()
        },
    );
    config_b.ai.agents = vec![AiAgentConfig {
        name: "helper".to_owned(),
        provider: "fake".to_owned(),
        model: "fake-model".to_owned(),
        tools: AgentTools::Wave,
        system_prompt: "Be brief.".to_owned(),
        ..AiAgentConfig::default()
    }];
    config_b.ai.reply_debounce_ms = 300;
    config_b.ai.stream_batch_ms = 40;
    let b = Node::start(config_b).await;

    let alice = a.token("alice").await;
    let (wave, wavelet, root) = a.create_wave(&alice, &[&a.address("alice")]).await;
    a.append(&alice, &wave, &wavelet, &root, "What is four?")
        .await;
    a.run(
        &alice,
        json!([add_participant(&wave, &wavelet, &b.address("helper"))]),
    )
    .await;
    wait_until("the helper's answer at A", || async {
        a.texts(&alice, &wave, &wavelet)
            .await
            .is_some_and(|t| t.to_string().contains("Hello from the helper."))
    })
    .await;
    b.stop().await;
    a.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deltas_applied_before_federation_starts_are_fanned_out() {
    let site_a = Site::new(Some(A));
    let site_b = Site::new(None);
    let b = Node::start(site_b.config(&[&site_a])).await;
    // A serves (as between `serve` and `start` in the binary) and a wave
    // with bob on it is made before its federation starts.
    let (mut a, setup) = Node::serve(site_a.config(&[&site_b])).await;
    let alice = a.token("alice").await;
    let bob = b.token("bob").await;
    let (wave, wavelet, root) = a
        .create_wave(&alice, &[&a.address("alice"), &b.address("bob")])
        .await;
    a.append(&alice, &wave, &wavelet, &root, "written early")
        .await;
    a.federate(setup).await;
    let texts = converged((&a, &alice), (&b, &bob), &wave, &wavelet).await;
    assert!(texts.to_string().contains("written early"), "{texts}");
    b.stop().await;
    a.stop().await;
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([(x % 256) as u8, (y % 256) as u8, 99, 255])
    });
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}

impl Node {
    /// Uploads a file as attachment `id` of the wave.
    async fn upload(&self, cookie: &str, id: &str, wave: &str, bytes: &[u8]) -> u16 {
        let form = reqwest::multipart::Form::new()
            .text("attachmentId", id.to_owned())
            .text("waveRef", wave.to_owned())
            .part(
                "uploadFormElement",
                reqwest::multipart::Part::bytes(bytes.to_vec()).file_name("picture.png"),
            );
        self.http
            .post(self.url("/attachment/upload"))
            .header("Cookie", cookie)
            .multipart(form)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    /// `GET /<kind>/<id>?waveRef=<wave>`: the status and the body.
    async fn download(&self, cookie: &str, kind: &str, id: &str, wave: &str) -> (u16, Vec<u8>) {
        let response = self
            .http
            .get(self.url(&format!("/{kind}/{id}")))
            .query(&[("waveRef", wave)])
            .header("Cookie", cookie)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.bytes().await.unwrap().to_vec())
    }
}

/// Overwrites the stored copy of `bytes` under `directory` with as many
/// other bytes.
fn tamper(directory: &std::path::Path, bytes: &[u8]) {
    let mut stack = vec![directory.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if std::fs::read(&path).unwrap() == bytes {
                let mut other = bytes.to_vec();
                other[0] ^= 0xff;
                std::fs::write(&path, other).unwrap();
                return;
            }
        }
    }
    panic!("no stored copy found");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attachments_travel_from_their_origin_to_participants_servers() {
    let site_a = Site::new(Some(A));
    let site_b = Site::new(None);
    let site_c = Site::new(None);
    let a = Node::start(site_a.config(&[&site_b, &site_c])).await;
    let b = Node::start(site_b.config(&[&site_a, &site_c])).await;
    let c = Node::start(site_c.config(&[&site_a, &site_b])).await;
    let alice_cookie = a.cookie("alice").await;
    let alice = a.token_for(&alice_cookie).await;
    let bob_cookie = b.cookie("bob").await;
    let bob = b.token_for(&bob_cookie).await;
    let dave_cookie = a.cookie("dave").await;

    let (wave, wavelet, _root) = a.create_wave(&alice, &[&a.address("alice")]).await;
    a.run(
        &alice,
        json!([add_participant(&wave, &wavelet, &b.address("bob"))]),
    )
    .await;
    converged((&a, &alice), (&b, &bob), &wave, &wavelet).await;

    // Bob uploads into the wave hosted on A: stored on B, for A's wavelet.
    let picture = png(320, 240);
    let picture_id = format!("{}/picture", b.domain);
    assert_eq!(
        b.upload(&bob_cookie, &picture_id, &wave, &picture).await,
        201
    );
    // Not in another server's name.
    let foreign = format!("{A}/forged");
    assert_eq!(b.upload(&bob_cookie, &foreign, &wave, b"x").await, 400);

    // Alice, on A, gets it and its thumbnail from B.
    let (status, bytes) = a
        .download(&alice_cookie, "attachment", &picture_id, &wave)
        .await;
    assert_eq!(status, 200);
    assert_eq!(bytes, picture);
    let (status, thumbnail) = a
        .download(&alice_cookie, "thumbnail", &picture_id, &wave)
        .await;
    assert_eq!(status, 200);
    assert!(!thumbnail.is_empty() && thumbnail != picture);
    // Dave, on A but not on the wave, does not.
    let (status, _) = a
        .download(&dave_cookie, "attachment", &picture_id, &wave)
        .await;
    assert_eq!(status, 403);

    // An id that names no server (a client that did not know its server
    // federates): A, the host, finds it on the participant's server B.
    let bare = png(64, 48);
    assert_eq!(b.upload(&bob_cookie, "bare", &wave, &bare).await, 201);
    let (status, bytes) = a.download(&alice_cookie, "attachment", "bare", &wave).await;
    assert_eq!((status, bytes), (200, bare));
    // One that is nowhere is still not found.
    let (status, _) = a
        .download(&alice_cookie, "attachment", "nowhere", &wave)
        .await;
    assert_eq!(status, 404);

    // C has nobody on the wave: B refuses it.
    let refused = c
        .federation()
        .endpoint()
        .request(
            site_b.key.public(),
            Some(&b.domain),
            federation_request::Op::FetchAttachment(FetchAttachment {
                attachment_id: Some(picture_id.clone()),
                wavelet_name: Some(IdUriCodec::wavelet_name_to_uri(&name(&wave, &wavelet))),
            }),
        )
        .await;
    assert_eq!(code(refused), Code::ItemNotFound);

    // Bytes that do not match what the origin says they are: rejected.
    let document = vec![5u8; 50_000];
    let document_id = format!("{}/document", b.domain);
    assert_eq!(
        b.upload(&bob_cookie, &document_id, &wave, &document).await,
        201
    );
    tamper(&site_b.config(&[]).attachments_directory(), &document);
    let (status, _) = a
        .download(&alice_cookie, "attachment", &document_id, &wave)
        .await;
    assert_eq!(status, 502);

    // B goes away: what A has cached is still served, the rest is 503.
    let late_id = format!("{}/late", b.domain);
    assert_eq!(b.upload(&bob_cookie, &late_id, &wave, b"late").await, 201);
    // Servers (not devices of one person) copy nothing ahead.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let late = resonator_server::attachment::AttachmentId::deserialise(&late_id).unwrap();
    let a_attachments = &a.app.as_ref().unwrap().state().attachments;
    assert_eq!(a_attachments.metadata(&late).await.unwrap(), None);
    b.stop().await;
    let (status, bytes) = a
        .download(&alice_cookie, "attachment", &picture_id, &wave)
        .await;
    assert_eq!((status, bytes), (200, picture));
    let (status, body) = a
        .download(&alice_cookie, "attachment", &late_id, &wave)
        .await;
    assert_eq!(status, 503);
    assert!(String::from_utf8_lossy(&body).contains("unavailable from"));
    c.stop().await;
    a.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_in_users_probe_remote_addresses() {
    let (_site_a, _site_b, a, b) = pair().await;
    let alice = a.cookie("alice").await;
    b.cookie("bob").await;
    let probe = |cookie: Option<String>, address: String| {
        let a = &a;
        async move {
            let mut request = a
                .http
                .get(a.url("/federation/probe"))
                .query(&[("address", address)]);
            if let Some(cookie) = cookie {
                request = request.header("Cookie", cookie);
            }
            let response = request.send().await.unwrap();
            let status = response.status().as_u16();
            (
                status,
                response.json::<Value>().await.unwrap_or(Value::Null),
            )
        }
    };
    let session: Value = a
        .http
        .get(a.url("/session"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(session["federation"], json!(true));

    let (status, _) = probe(None, b.address("bob")).await;
    assert_eq!(status, 403);
    let (status, _) = probe(Some(alice.clone()), a.address("carol")).await;
    assert_eq!(status, 400);
    let (status, body) = probe(Some(alice.clone()), b.address("bob")).await;
    assert_eq!(status, 200);
    assert_eq!(
        (&body["reachable"], &body["allowed"], &body["exists"]),
        (&json!(true), &json!(true), &json!(true)),
        "{body}"
    );
    let (_, body) = probe(Some(alice.clone()), b.address("nobody")).await;
    assert_eq!(body["exists"], json!(false), "{body}");
    let (_, body) = probe(Some(alice.clone()), "eve@elsewhere.example".to_owned()).await;
    assert_eq!(
        (&body["reachable"], &body["allowed"]),
        (&json!(false), &json!(false)),
        "{body}"
    );
    b.stop().await;
    let (_, body) = probe(Some(alice.clone()), "bob@".to_owned() + &_site_b.domain).await;
    assert_eq!(
        (&body["reachable"], &body["allowed"]),
        (&json!(false), &json!(true)),
        "{body}"
    );
    // Rate-limited per user.
    let mut limited = false;
    for _ in 0..40 {
        let (status, _) = probe(Some(alice.clone()), "eve@elsewhere.example".to_owned()).await;
        if status == 429 {
            limited = true;
            break;
        }
    }
    assert!(limited);
    a.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn names_and_avatars_are_fetched_from_their_domain() {
    let (_site_a, _site_b, a, b) = pair().await;
    let alice = a.cookie("alice").await;
    let bob = b.cookie("bob").await;
    // Bob sets a name and an avatar on B.
    let named = b
        .http
        .post(b.url("/profile/me"))
        .header("Cookie", &bob)
        .json(&json!({"name": "Bob the Builder"}))
        .send()
        .await
        .unwrap();
    assert_eq!(named.status(), 200);
    let set = b
        .http
        .put(b.url("/profile/me/avatar"))
        .header("Cookie", &bob)
        .body(png(300, 200))
        .send()
        .await
        .unwrap();
    assert_eq!(set.status(), 200);

    // Alice on A sees them.
    let profiles = |cookie: String| {
        let a = &a;
        let bob = b.address("bob");
        async move {
            a.http
                .get(a.url("/profile"))
                .query(&[("addresses", format!("{bob},nobody@{}", A))])
                .header("Cookie", cookie)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let seen = profiles(alice.clone()).await;
    assert_eq!(seen["1"][0]["2"], "Bob the Builder", "{seen}");
    let url = seen["1"][0]["3"].as_str().unwrap().to_owned();
    assert!(url.starts_with("/avatar/"), "{url}");
    let avatar = a
        .http
        .get(a.url(&url))
        .header("Cookie", &alice)
        .send()
        .await
        .unwrap();
    assert_eq!(avatar.status(), 200);
    let image = image::load_from_memory(&avatar.bytes().await.unwrap()).unwrap();
    assert_eq!((image.width(), image.height()), (200, 200));
    assert_eq!(seen["1"][1]["2"], "Nobody");
    b.stop().await;
    a.stop().await;
}
