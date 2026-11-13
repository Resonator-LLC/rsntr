//! Host epochs over federation, with three servers in one process (as in
//! `federation.rs`): a wavelet's host role moves to another server, the
//! old host's edits made meanwhile are rewound everywhere, resubmitted and
//! rebased onto the new host's history exactly once, and an old host is
//! fenced off.

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use resonator_agents::Integrations;
use resonator_federation::dns::StaticTxtResolver;
use resonator_federation::{CoHosts, Federation, FederationSetup, ServerKey, Tuning};
use resonator_model::id::{DualIdSerialiser, IdSerialiser, IdUriCodec, WaveletName};
use resonator_model::proto::federation_rpc::{
    Commit, Resubmit, Update, federation_request, federation_response,
};
use resonator_model::proto::serialize_hashed_version;
use resonator_server::app::App;
use resonator_server::config::{Config, FederationMode, FederationPeer};
use resonator_server::storage::{AppliedDelta, OutboxStatus};
use resonator_server::waveserver::WaveServerOptions;
use serde_json::{Value, json};
use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(30);
const DOMAINS: [&str; 3] = ["h1.example", "h2.example", "h3.example"];

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

struct Site {
    dir: TempDir,
    key: ServerKey,
    domain: String,
    port: u16,
}

impl Site {
    fn new(domain: &str) -> Site {
        let dir = tempfile::tempdir().unwrap();
        let key = ServerKey::load_or_create(&dir.path().join("federation.key")).unwrap();
        Site {
            dir,
            key,
            domain: domain.to_owned(),
            port: free_udp_port(),
        }
    }

    fn peer(&self) -> FederationPeer {
        FederationPeer {
            domain: self.domain.clone(),
            key: self.key.key_domain(),
            addr: format!("127.0.0.1:{}", self.port),
        }
    }

    fn config(&self, peers: &[&Site]) -> Config {
        let mut config = Config {
            domain: self.domain.clone(),
            listen: vec!["127.0.0.1:0".to_owned()],
            database_path: self.dir.path().join("resonator.db"),
            register_max_per_hour: 0,
            // Snapshots on both sides of every fork.
            snapshot_interval: 2,
            ..Config::default()
        };
        config.federation.enabled = true;
        config.federation.mode = FederationMode::Allowlist;
        config.federation.allow = peers.iter().map(|p| p.domain.clone()).collect();
        config.federation.relay = "off".to_owned();
        config.federation.discovery = "off".to_owned();
        config.federation.listen = format!("127.0.0.1:{}", self.port);
        config.federation.request_timeout_secs = 2;
        config.federation.peers = peers.iter().map(|p| p.peer()).collect();
        config
    }
}

/// Three sites, each peering with the other two.
struct Sites([Site; 3]);

impl Sites {
    fn new() -> Sites {
        Sites(DOMAINS.map(Site::new))
    }

    fn config(&self, i: usize) -> Config {
        let peers: Vec<&Site> = (0..3).filter(|j| *j != i).map(|j| &self.0[j]).collect();
        self.0[i].config(&peers)
    }

    async fn start(&self, i: usize) -> Node {
        Node::start(self.config(i)).await
    }

    async fn serve(&self, i: usize) -> (Node, FederationSetup) {
        Node::serve(self.config(i)).await
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

    /// Serves without the federation endpoint: cut off from the others.
    async fn serve(config: Config) -> (Node, FederationSetup) {
        let mut setup = FederationSetup::load(&config)
            .expect("federation setup")
            .expect("federation enabled")
            .with_tuning(tuning())
            .with_resolver(Arc::new(StaticTxtResolver::new()))
            .with_epoch_authority(Arc::new(CoHosts::new(DOMAINS)));
        let mut options = WaveServerOptions::from_config(&config);
        options.delta_signer = Some(setup.delta_signer());
        let mut prepared = App::prepare_with(&config, options).await.expect("prepare");
        setup.subscribe(prepared.server());
        setup.serve_app(prepared.state());
        // The Data API comes with the integrations.
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

    async fn token(&self, cookie: &str) -> String {
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

    async fn run(&self, token: &str, operations: Value) -> Value {
        let body = self.data_api(token, operations).await;
        for response in body.as_array().unwrap() {
            assert!(response.get("error").is_none(), "{body}");
        }
        body
    }

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

    async fn append(&self, token: &str, wave: &Wave, text: &str) {
        self.run(
            token,
            json!([{"id": "m", "method": "document.modify", "params": {
                "waveId": wave.wave, "waveletId": wave.wavelet, "blipId": wave.root,
                "modifyAction": {"modifyHow": "INSERT_AFTER", "values": [text]}
            }}]),
        )
        .await;
    }

    /// The root blip's text as `token`'s user sees it.
    async fn text(&self, token: &str, wave: &Wave) -> Option<String> {
        let body = self
            .data_api(
                token,
                json!([{"id": "f", "method": "robot.fetchWave",
                        "params": {"waveId": wave.wave, "waveletId": wave.wavelet}}]),
            )
            .await;
        Some(
            body[0]["data"]["blips"][&wave.root]["content"]
                .as_str()?
                .to_owned(),
        )
    }

    async fn version(&self, wave: &Wave) -> resonator_model::version::HashedVersion {
        self.server().current_version(&wave.name).await.unwrap()
    }

    /// The whole history, as hashed.
    async fn history(&self, wave: &Wave) -> Vec<Vec<u8>> {
        let server = self.server();
        let end = server.current_version(&wave.name).await.unwrap();
        server
            .get_applied_history(&wave.name, &server.version_zero(&wave.name), &end, None)
            .await
            .unwrap()
            .iter()
            .map(|delta| delta.bytes().to_vec())
            .collect()
    }

    async fn applied_since(&self, wave: &Wave, from: u64) -> Vec<AppliedDelta> {
        let server = self.server();
        let start = server
            .hashed_version_at(&wave.name, from)
            .await
            .unwrap()
            .unwrap();
        let end = server.current_version(&wave.name).await.unwrap();
        server
            .get_applied_history(&wave.name, &start, &end, None)
            .await
            .unwrap()
    }

    /// Whether `cookie`'s user finds the wave searching for `term`.
    async fn search_finds(&self, cookie: &str, term: &str, wave: &Wave) -> bool {
        let response = self
            .http
            .get(self.url("/search/"))
            .query(&[("query", term), ("index", "0"), ("numResults", "50")])
            .header("Cookie", cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let wave_local = wave.wave.split('/').nth(1).unwrap();
        response.text().await.unwrap().contains(wave_local)
    }
}

struct Wave {
    wave: String,
    wavelet: String,
    root: String,
    name: WaveletName,
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

/// A user on each of some nodes: (node, token).
type Seat<'a> = (&'a Node, &'a str);

/// Waits until every node has the same history and its user sees the same
/// text; returns that text.
async fn converged(seats: &[Seat<'_>], wave: &Wave) -> String {
    wait_until("convergence", || async {
        let (first, token) = seats[0];
        let version = first.version(wave).await;
        let text = first.text(token, wave).await;
        for (node, token) in &seats[1..] {
            if node.version(wave).await != version || node.text(token, wave).await != text {
                return false;
            }
        }
        true
    })
    .await;
    let history = seats[0].0.history(wave).await;
    for (node, _) in &seats[1..] {
        assert!(
            node.history(wave).await == history,
            "same hash chain on {}",
            node.domain
        );
    }
    seats[0].0.text(seats[0].1, wave).await.unwrap()
}

/// Every marker occurs exactly once.
fn each_once(text: &str, markers: &[&str]) {
    for marker in markers {
        assert_eq!(text.matches(marker).count(), 1, "{marker} once in {text:?}");
    }
}

/// No notification of the wavelet is left undelivered on `node`, and none
/// is past its history.
async fn outbox_settled(node: &Node, wave: &Wave) {
    let uri = IdUriCodec::wavelet_name_to_uri(&wave.name);
    wait_until("the outbox settles", || async {
        let current = node.version(wave).await.version();
        node.federation()
            .outbox()
            .await
            .unwrap()
            .iter()
            .filter(|entry| entry.wavelet == uri)
            .all(|entry| entry.status == OutboxStatus::Delivered && entry.from_version <= current)
    })
    .await;
}

struct Users {
    alice: (String, String),
    bob: (String, String),
    carol: (String, String),
}

async fn users(h1: &Node, h2: &Node, h3: &Node) -> Users {
    let mut out = Vec::new();
    for (node, name) in [(h1, "alice"), (h2, "bob"), (h3, "carol")] {
        let cookie = node.cookie(name).await;
        let token = node.token(&cookie).await;
        out.push((cookie, token));
    }
    let carol = out.pop().unwrap();
    let bob = out.pop().unwrap();
    let alice = out.pop().unwrap();
    Users { alice, bob, carol }
}

/// A wave hosted on h1 with alice, bob and carol, seen by all three.
async fn shared_wave(h1: &Node, h2: &Node, h3: &Node, users: &Users) -> Wave {
    let (wave, wavelet, root) = h1
        .create_wave(
            &users.alice.1,
            &[
                &h1.address("alice"),
                &h2.address("bob"),
                &h3.address("carol"),
            ],
        )
        .await;
    let wave = Wave {
        name: WaveletName::of(
            DualIdSerialiser::MODERN.deserialise_wave_id(&wave).unwrap(),
            DualIdSerialiser::MODERN
                .deserialise_wavelet_id(&wavelet)
                .unwrap(),
        ),
        wave,
        wavelet,
        root,
    };
    h1.append(&users.alice.1, &wave, "[start]").await;
    converged(
        &[
            (h1, &users.alice.1),
            (h2, &users.bob.1),
            (h3, &users.carol.1),
        ],
        &wave,
    )
    .await;
    wave
}

async fn wait_host(node: &Node, wave: &Wave, host: &str) {
    wait_until(&format!("{} sees {host} as host", node.domain), || async {
        node.server().host_domain(&wave.name) == host
    })
    .await;
}

fn resubmit(
    wave: &Wave,
    fork: &resonator_model::version::HashedVersion,
    chain: &[AppliedDelta],
) -> federation_request::Op {
    federation_request::Op::Resubmit(Resubmit {
        wavelet_name: Some(IdUriCodec::wavelet_name_to_uri(&wave.name)),
        fork_version: Some(serialize_hashed_version(fork).unwrap()),
        applied_delta: chain.iter().map(|d| d.bytes().to_vec()).collect(),
        signer_info: Vec::new(),
    })
}

/// The main scenario: h1 hosts, is cut off, h2 takes over, both sides
/// edit, h1 comes back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_returning_old_host_rewinds_and_its_edits_are_rebased_once() {
    let sites = Sites::new();
    let h1 = sites.start(0).await;
    let h2 = sites.start(1).await;
    let h3 = sites.start(2).await;
    let users = users(&h1, &h2, &h3).await;
    let wave = shared_wave(&h1, &h2, &h3, &users).await;

    // h1 is cut off, and keeps serving its users as host.
    h1.stop().await;
    let (h1, setup) = sites.serve(0).await;
    let fork = h2.version(&wave).await;
    let start = h2.federation().take_over(&wave.name, 2).await.unwrap();
    assert_eq!(start, fork);
    assert_eq!(h2.server().host_domain(&wave.name), "h2.example");
    wait_host(&h3, &wave, "h2.example").await;
    for i in 0..3 {
        h1.append(&users.alice.1, &wave, &format!("[orphan{i}]"))
            .await;
        h2.append(&users.bob.1, &wave, &format!("[bob{i}]")).await;
        h3.append(&users.carol.1, &wave, &format!("[carol{i}]"))
            .await;
    }
    let orphans = h1.applied_since(&wave, fork.version()).await;
    assert_eq!(orphans.len(), 3);
    assert_eq!(h1.server().host_domain(&wave.name), "h1.example");

    // h1 comes back: it learns of epoch 2, steps down and rewinds; its
    // edits are resubmitted and rebased on h2.
    let mut h1 = h1;
    h1.federate(setup).await;
    let seats = [
        (&h1, users.alice.1.as_str()),
        (&h2, users.bob.1.as_str()),
        (&h3, users.carol.1.as_str()),
    ];
    let markers = [
        "[start]",
        "[orphan0]",
        "[orphan1]",
        "[orphan2]",
        "[bob0]",
        "[bob1]",
        "[bob2]",
        "[carol0]",
        "[carol1]",
        "[carol2]",
    ];
    wait_until("the orphans are rebased", || async {
        h2.text(&users.bob.1, &wave)
            .await
            .is_some_and(|text| text.contains("[orphan2]"))
    })
    .await;
    let text = converged(&seats, &wave).await;
    each_once(&text, &markers);
    for (node, _) in &seats {
        assert_eq!(node.server().host_domain(&wave.name), "h2.example");
        assert_eq!(node.server().epoch_at(&wave.name, fork.version()), 2);
    }
    // Deltas of epoch 2 on: 3 by bob, 3 by carol, 3 rebased.
    assert_eq!(h2.version(&wave).await.version(), fork.version() + 9);

    // Search follows the rewound history everywhere.
    let cookies = [&users.alice.0, &users.bob.0, &users.carol.0];
    for ((node, _), cookie) in seats.iter().zip(cookies) {
        wait_until(&format!("search on {}", node.domain), || async {
            node.search_finds(cookie, "orphan1", &wave).await
                && node.search_finds(cookie, "carol2", &wave).await
        })
        .await;
    }
    for (node, _) in &seats {
        outbox_settled(node, &wave).await;
    }

    // Resubmitting the same orphans again applies nothing.
    let answer = h3
        .federation()
        .request("h2.example", resubmit(&wave, &fork, &orphans))
        .await
        .unwrap();
    let federation_response::Result::Resubmit(result) = answer else {
        panic!("{answer:?}");
    };
    assert_eq!((result.applied, result.skipped), (Some(0), Some(3)));

    // An old-epoch update or commit is fenced off.
    let update = federation_request::Op::Update(Update {
        wavelet_name: Some(IdUriCodec::wavelet_name_to_uri(&wave.name)),
        applied_delta: orphans.iter().map(|d| d.bytes().to_vec()).collect(),
        epoch_start: Vec::new(),
    });
    let answer = h1.federation().request("h3.example", update).await.unwrap();
    let federation_response::Result::Fenced(fenced) = answer else {
        panic!("{answer:?}");
    };
    assert_eq!(fenced.epoch_start.len(), 1);
    let commit = federation_request::Op::Commit(Commit {
        wavelet_name: Some(IdUriCodec::wavelet_name_to_uri(&wave.name)),
        committed_version: Some(serialize_hashed_version(&h1.version(&wave).await).unwrap()),
        epoch_start: Vec::new(),
    });
    let answer = h1.federation().request("h2.example", commit).await.unwrap();
    assert!(
        matches!(answer, federation_response::Result::Fenced(_)),
        "{answer:?}"
    );
    let text_after = converged(&seats, &wave).await;
    assert_eq!(text_after, text);

    // Restarting after the rewind changes nothing.
    h1.stop().await;
    let h1 = sites.start(0).await;
    assert_eq!(h1.server().host_domain(&wave.name), "h2.example");
    h1.append(&users.alice.1, &wave, "[after restart]").await;
    let seats = [
        (&h1, users.alice.1.as_str()),
        (&h2, users.bob.1.as_str()),
        (&h3, users.carol.1.as_str()),
    ];
    let text = converged(&seats, &wave).await;
    each_once(&text, &markers);
    each_once(&text, &["[after restart]"]);
    // Snapshots at and after the fork were dropped: a reload agrees.
    for (node, token) in &seats {
        node.server().unload_all_waves();
        assert_eq!(
            node.text(token, &wave).await.as_deref(),
            Some(text.as_str())
        );
    }
    h1.stop().await;
    h2.stop().await;
    h3.stop().await;
}

/// The old host never comes back, but h3 got its last edits before the
/// takeover from a server that had not: h3 resubmits them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn orphans_held_by_a_replica_are_resubmitted_without_the_old_host() {
    let sites = Sites::new();
    let h1 = sites.start(0).await;
    let h2 = sites.start(1).await;
    let h3 = sites.start(2).await;
    let users = users(&h1, &h2, &h3).await;
    let wave = shared_wave(&h1, &h2, &h3, &users).await;

    h2.stop().await;
    for i in 0..2 {
        h1.append(&users.alice.1, &wave, &format!("[late{i}]"))
            .await;
    }
    let h1_version = h1.version(&wave).await;
    wait_until("h3 has h1's late edits", || async {
        h3.version(&wave).await == h1_version
    })
    .await;
    h1.stop().await;

    let h2 = sites.start(1).await;
    let fork = h2.federation().take_over(&wave.name, 2).await.unwrap();
    assert!(fork.version() < h1_version.version());
    h2.append(&users.bob.1, &wave, "[bob]").await;
    let seats = [(&h2, users.bob.1.as_str()), (&h3, users.carol.1.as_str())];
    wait_until("h3's orphans reach h2", || async {
        h2.text(&users.bob.1, &wave)
            .await
            .is_some_and(|text| text.contains("[late1]"))
    })
    .await;
    let text = converged(&seats, &wave).await;
    each_once(&text, &["[start]", "[late0]", "[late1]", "[bob]"]);
    assert_eq!(h2.version(&wave).await.version(), fork.version() + 3);
    for (node, token) in &seats {
        node.server().unload_all_waves();
        assert_eq!(
            node.text(token, &wave).await.as_deref(),
            Some(text.as_str())
        );
    }
    h2.stop().await;
    h3.stop().await;
}

/// The old host learns of the new epoch from h3 while h2 is down, rewinds,
/// and restarts with its orphans still pending; they reach h2 once it is
/// back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pending_orphans_survive_a_restart_of_the_old_host() {
    let sites = Sites::new();
    let h1 = sites.start(0).await;
    let h2 = sites.start(1).await;
    let h3 = sites.start(2).await;
    let users = users(&h1, &h2, &h3).await;
    let wave = shared_wave(&h1, &h2, &h3, &users).await;

    h1.stop().await;
    let (mut h1, setup) = sites.serve(0).await;
    let fork = h2.federation().take_over(&wave.name, 2).await.unwrap();
    wait_host(&h3, &wave, "h2.example").await;
    h2.stop().await;
    h1.append(&users.alice.1, &wave, "[orphan]").await;
    h1.federate(setup).await;
    // h3 fences h1 off; h1 rewinds but cannot reach h2.
    wait_host(&h1, &wave, "h2.example").await;
    wait_until("h1 rewinds", || async { h1.version(&wave).await == fork }).await;
    h1.stop().await;
    let h1 = sites.start(0).await;
    assert_eq!(h1.version(&wave).await, fork);
    let h2 = sites.start(1).await;
    let seats = [
        (&h1, users.alice.1.as_str()),
        (&h2, users.bob.1.as_str()),
        (&h3, users.carol.1.as_str()),
    ];
    wait_until("h1's orphan reaches h2", || async {
        h2.text(&users.bob.1, &wave)
            .await
            .is_some_and(|text| text.contains("[orphan]"))
    })
    .await;
    let text = converged(&seats, &wave).await;
    each_once(&text, &["[start]", "[orphan]"]);
    assert_eq!(h2.version(&wave).await.version(), fork.version() + 1);
    h1.stop().await;
    h2.stop().await;
    h3.stop().await;
}

/// A takeover needs an epoch above every known one; a server may host its
/// own wavelet in a later epoch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_takeover_needs_a_higher_epoch() {
    let sites = Sites::new();
    let h1 = sites.start(0).await;
    let users_h1 = h1.cookie("alice").await;
    let token = h1.token(&users_h1).await;
    let (wave, wavelet, _) = h1.create_wave(&token, &[&h1.address("alice")]).await;
    let name = WaveletName::of(
        DualIdSerialiser::MODERN.deserialise_wave_id(&wave).unwrap(),
        DualIdSerialiser::MODERN
            .deserialise_wavelet_id(&wavelet)
            .unwrap(),
    );
    // A lower or equal epoch is refused outright.
    assert!(h1.federation().take_over(&name, 0).await.is_err());
    // h1 may host its own wavelet in a later epoch.
    let start = h1.federation().take_over(&name, 3).await.unwrap();
    assert_eq!(h1.server().epoch_at(&name, start.version()), 3);
    assert!(h1.federation().take_over(&name, 2).await.is_err());
    h1.stop().await;
}
