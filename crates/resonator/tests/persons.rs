//! Person identity over federation, in one process (as `federation.rs`
//! and `epochs.rs`): two people, P and Q, each a person key with two
//! device nodes, the first of which hosts the person's wavelets (and
//! holds the person key). Waves of P with Q reach all four devices, a
//! non-holder device submits through its holder, a device cannot speak
//! for another person, revocation cuts a device off without rewinding
//! what it wrote, a person key recovered from its phrase certifies a new
//! device, and the host role moves between a person's devices.

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use resonator_agents::Integrations;
use resonator_federation::dns::StaticTxtResolver;
use resonator_federation::endpoint::{EchoHandler, EndpointOptions, KeyDomainAuthority};
use resonator_federation::person::{RecoveryPhrase, cert_to_hex};
use resonator_federation::{
    Authorisation, EndpointAddr, Federation, FederationEndpoint, FederationSetup, FileKeyStore,
    KeyStore, LocalIdentity, PersonKey, ServerKey, Tuning,
};
use resonator_model::id::{DualIdSerialiser, IdSerialiser, WaveletName};
use resonator_model::proto::federation_rpc::{Echo, federation_request};
use resonator_server::app::App;
use resonator_server::config::{Config, FederationMode, FederationPeer};
use resonator_server::waveserver::{WaveServerOptions, WaveletHost};
use serde_json::{Value, json};
use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(30);

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Where every node of these tests publishes and looks up pairing codes
/// (no internet).
fn rendezvous() -> Arc<resonator_federation::pair_code::MemoryRendezvous> {
    static RENDEZVOUS: std::sync::LazyLock<Arc<resonator_federation::pair_code::MemoryRendezvous>> =
        std::sync::LazyLock::new(Default::default);
    Arc::clone(&RENDEZVOUS)
}

fn tuning() -> Tuning {
    Tuning {
        retry_min: Duration::from_millis(50),
        retry_max: Duration::from_millis(400),
        commit_wait: Duration::from_secs(5),
        revocation_poll: Duration::from_millis(200),
        lease_renew_anchor: Duration::from_millis(500),
        lease_renew: Duration::from_millis(400),
        sync_interval: Duration::from_millis(300),
        ..Tuning::default()
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// A person: its key (and phrase), as the test's admin.
struct Person {
    key: PersonKey,
    phrase: RecoveryPhrase,
}

impl Person {
    fn new() -> Person {
        let (key, phrase) = PersonKey::generate();
        Person { key, phrase }
    }

    fn domain(&self) -> String {
        self.key.domain()
    }
}

/// One device's directory, key and port.
struct Device {
    dir: TempDir,
    key: ServerKey,
    port: u16,
    domain: String,
}

impl Device {
    fn new(person: &Person) -> Device {
        let dir = tempfile::tempdir().unwrap();
        let key = ServerKey::load_or_create(&dir.path().join("federation.key")).unwrap();
        Device {
            dir,
            key,
            port: free_udp_port(),
            domain: person.domain(),
        }
    }

    fn z32(&self) -> String {
        self.key.key_domain()
    }

    fn peer(&self) -> FederationPeer {
        FederationPeer {
            domain: self.domain.clone(),
            key: self.z32(),
            addr: format!("127.0.0.1:{}", self.port),
        }
    }

    /// Admin device: stores the person key from its phrase.
    fn make_admin(&self, phrase: &RecoveryPhrase) {
        FileKeyStore::new(self.dir.path().join("person.key"))
            .store(phrase)
            .unwrap();
    }

    /// The config of this device: admin (holding the person key) or
    /// certified by `person`; an `anchor` is the preferred holder of the
    /// person's host lease; `peers` are all other devices.
    fn config(
        &self,
        person: &Person,
        admin: bool,
        anchor: bool,
        peers: &[&Device],
        allow: &[String],
    ) -> Config {
        let mut config = Config {
            listen: vec!["127.0.0.1:0".to_owned()],
            database_path: self.dir.path().join("resonator.db"),
            register_max_per_hour: 0,
            snapshot_interval: 2,
            ..Config::default()
        };
        config.federation.enabled = true;
        config.federation.mode = FederationMode::Allowlist;
        config.federation.allow = allow.to_vec();
        config.federation.relay = "off".to_owned();
        config.federation.discovery = "off".to_owned();
        config.federation.listen = format!("127.0.0.1:{}", self.port);
        config.federation.request_timeout_secs = 2;
        config.federation.peers = peers.iter().map(|p| p.peer()).collect();
        config.person.enabled = true;
        config.person.admin = admin;
        config.person.device_name = format!("device {}", &self.z32()[..6]);
        if !admin {
            config.person.cert = cert_to_hex(&person.key.issue_cert(
                &self.key.public(),
                "phone",
                false,
                now(),
            ));
        }
        config.person.anchor = anchor;
        FederationSetup::configure_person(&mut config).unwrap();
        assert_eq!(config.domain, person.domain());
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
        Node::start_with(config, rendezvous()).await
    }

    /// Starts with pairing codes on `rendezvous`.
    async fn start_with(
        config: Config,
        rendezvous: Arc<dyn resonator_federation::pair_code::Rendezvous>,
    ) -> Node {
        let (mut node, setup) = Node::serve_with(config, rendezvous).await;
        node.federate(setup).await;
        node
    }

    /// Serves without the federation endpoint: cut off from the others.
    async fn serve(config: Config) -> (Node, FederationSetup) {
        Node::serve_with(config, rendezvous()).await
    }

    async fn serve_with(
        config: Config,
        rendezvous: Arc<dyn resonator_federation::pair_code::Rendezvous>,
    ) -> (Node, FederationSetup) {
        let mut setup = FederationSetup::load(&config)
            .expect("federation setup")
            .expect("federation enabled")
            .with_tuning(tuning())
            .with_resolver(Arc::new(StaticTxtResolver::new()))
            .with_rendezvous(rendezvous);
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

    /// A session cookie of `name` (registered on this device).
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
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    /// A token of `name` (registered on this device).
    async fn user(&self, name: &str) -> String {
        let cookie = self.cookie(name).await;
        let response = self
            .http
            .post(self.url("/auth/tokens"))
            .header("Cookie", &cookie)
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

    async fn create_wave(&self, token: &str, participants: &[&str]) -> Wave {
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
        let wave = data["waveId"].as_str().unwrap().to_owned();
        let wavelet = data["waveletId"].as_str().unwrap().to_owned();
        Wave {
            name: WaveletName::of(
                DualIdSerialiser::MODERN.deserialise_wave_id(&wave).unwrap(),
                DualIdSerialiser::MODERN
                    .deserialise_wavelet_id(&wavelet)
                    .unwrap(),
            ),
            wave,
            wavelet,
            root: data["blipId"].as_str().unwrap().to_owned(),
        }
    }

    fn append_ops(wave: &Wave, text: &str) -> Value {
        json!([{"id": "m", "method": "document.modify", "params": {
            "waveId": wave.wave, "waveletId": wave.wavelet, "blipId": wave.root,
            "modifyAction": {"modifyHow": "INSERT_AFTER", "values": [text]}
        }}])
    }

    async fn append(&self, token: &str, wave: &Wave, text: &str) {
        self.run(token, Self::append_ops(wave, text)).await;
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
}

struct Wave {
    wave: String,
    wavelet: String,
    root: String,
    name: WaveletName,
}

/// Waits until every node of `nodes` sees `holder` holding its person's
/// host lease.
async fn wait_for_holder(nodes: &[&Node], holder: &Node) {
    let key = holder.federation().endpoint().id();
    wait_until("the host lease", || async {
        holder.federation().lease_state().held_here
            && nodes
                .iter()
                .all(|node| node.federation().lease_state().holder == Some(key))
    })
    .await;
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

type Seat<'a> = (&'a Node, &'a str);

/// Waits until every node has the same history and its user sees the same
/// text containing every marker; returns that text.
async fn converged(seats: &[Seat<'_>], wave: &Wave, markers: &[&str]) -> String {
    wait_until("convergence", || async {
        let (first, token) = seats[0];
        let version = first.version(wave).await;
        let Some(text) = first.text(token, wave).await else {
            return false;
        };
        if !markers.iter().all(|marker| text.contains(marker)) {
            return false;
        }
        for (node, token) in &seats[1..] {
            if node.version(wave).await != version
                || node.text(token, wave).await.as_deref() != Some(text.as_str())
            {
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
    let text = seats[0].0.text(seats[0].1, wave).await.unwrap();
    for marker in markers {
        assert_eq!(text.matches(marker).count(), 1, "{marker} once in {text:?}");
    }
    text
}

/// P and Q, two devices each; device 0 holds the person key and hosts.
/// `spare` adds a third device of P (`p[2]`), configured everywhere but
/// not started.
struct World {
    p: Person,
    q: Person,
    p_devices: Vec<Device>,
    q_devices: Vec<Device>,
}

impl World {
    fn new(spare: bool) -> World {
        let p = Person::new();
        let q = Person::new();
        let p_devices: Vec<Device> = (0..if spare { 3 } else { 2 })
            .map(|_| Device::new(&p))
            .collect();
        let q_devices: Vec<Device> = (0..2).map(|_| Device::new(&q)).collect();
        p_devices[0].make_admin(&p.phrase);
        q_devices[0].make_admin(&q.phrase);
        World {
            p,
            q,
            p_devices,
            q_devices,
        }
    }

    fn allow(&self) -> Vec<String> {
        vec![self.p.domain(), self.q.domain()]
    }

    /// Every device but `me`, holders first.
    fn peers_of(&self, me: &Device) -> Vec<&Device> {
        self.p_devices
            .iter()
            .chain(&self.q_devices)
            .filter(|device| device.z32() != me.z32())
            .collect()
    }

    /// Device `i` of P cut off from everyone: no peers, and listening
    /// where nobody looks for it.
    fn p_cut_off(&self, i: usize) -> Config {
        let mut config = self.p_config(i);
        config.federation.peers.clear();
        config.federation.listen = format!("127.0.0.1:{}", free_udp_port());
        config
    }

    fn p_config(&self, i: usize) -> Config {
        let device = &self.p_devices[i];
        device.config(
            &self.p,
            i == 0,
            i == 0,
            &self.peers_of(device),
            &self.allow(),
        )
    }

    fn q_config(&self, i: usize) -> Config {
        let device = &self.q_devices[i];
        device.config(
            &self.q,
            i == 0,
            i == 0,
            &self.peers_of(device),
            &self.allow(),
        )
    }
}

/// The four devices running, with alice (P) and bob (Q) on each of their
/// person's devices.
struct Running {
    p1: Node,
    p2: Node,
    q1: Node,
    q2: Node,
    alice1: String,
    alice2: String,
    bob1: String,
    bob2: String,
}

impl Running {
    async fn start(world: &World) -> Running {
        let p1 = Node::start(world.p_config(0)).await;
        let p2 = Node::start(world.p_config(1)).await;
        let q1 = Node::start(world.q_config(0)).await;
        let q2 = Node::start(world.q_config(1)).await;
        wait_for_holder(&[&p1, &p2], &p1).await;
        wait_for_holder(&[&q1, &q2], &q1).await;
        let alice1 = p1.user("alice").await;
        let alice2 = p2.user("alice").await;
        let bob1 = q1.user("bob").await;
        let bob2 = q2.user("bob").await;
        Running {
            p1,
            p2,
            q1,
            q2,
            alice1,
            alice2,
            bob1,
            bob2,
        }
    }

    fn seats(&self) -> Vec<Seat<'_>> {
        vec![
            (&self.p1, &self.alice1),
            (&self.p2, &self.alice2),
            (&self.q1, &self.bob1),
            (&self.q2, &self.bob2),
        ]
    }

    /// A wave of alice with bob, created on P's holder.
    async fn shared_wave(&self) -> Wave {
        let wave = self
            .p1
            .create_wave(
                &self.alice1,
                &[&self.p1.address("alice"), &self.q1.address("bob")],
            )
            .await;
        self.p1.append(&self.alice1, &wave, "[start]").await;
        converged(&self.seats(), &wave, &["[start]"]).await;
        wave
    }

    async fn stop(self) {
        for node in [self.p1, self.p2, self.q1, self.q2] {
            node.stop().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wave_of_two_people_reaches_all_their_devices_and_converges() {
    let world = World::new(false);
    let run = Running::start(&world).await;
    assert_eq!(run.p1.domain, world.p.domain());
    assert_eq!(run.q2.domain, world.q.domain());
    let wave = run.shared_wave().await;
    assert!(run.p1.server().is_local_wavelet(&wave.name));
    for node in [&run.p2, &run.q1, &run.q2] {
        assert!(!node.server().is_local_wavelet(&wave.name));
    }
    // Everyone edits, on every device.
    let ((), (), (), ()) = tokio::join!(
        run.p1.append(&run.alice1, &wave, "[p1]"),
        run.p2.append(&run.alice2, &wave, "[p2]"),
        run.q1.append(&run.bob1, &wave, "[q1]"),
        run.q2.append(&run.bob2, &wave, "[q2]"),
    );
    converged(
        &run.seats(),
        &wave,
        &["[start]", "[p1]", "[p2]", "[q1]", "[q2]"],
    )
    .await;
    // Q's devices dial P's holder for P's wavelets.
    let p1_key = world.p_devices[0].key.public();
    assert_eq!(
        run.q2
            .federation()
            .trust()
            .resolve(&world.p.domain())
            .await
            .unwrap(),
        Some(p1_key)
    );
    run.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_that_is_not_the_holder_submits_through_the_holder() {
    let world = World::new(false);
    let run = Running::start(&world).await;
    // alice creates a wave on her second device: the holder hosts it.
    let wave = run
        .p2
        .create_wave(
            &run.alice2,
            &[&run.p2.address("alice"), &run.q1.address("bob")],
        )
        .await;
    assert_eq!(
        run.p2.server().wavelet_host(&wave.name),
        WaveletHost::Delegated {
            domain: world.p.domain(),
            holder: world.p_devices[0].key.public().as_bytes().to_vec(),
        }
    );
    assert!(run.p1.server().is_local_wavelet(&wave.name));
    run.p2.append(&run.alice2, &wave, "[from p2]").await;
    run.q2.append(&run.bob2, &wave, "[from q2]").await;
    converged(&run.seats(), &wave, &["[from p2]", "[from q2]"]).await;
    // The deltas are signed by the device that took them in.
    let p2_signer = resonator_federation::key::signer_id(&world.p_devices[1].key.public());
    let history = run.p1.history(&wave).await;
    assert!(
        history.iter().any(|bytes| bytes
            .windows(p2_signer.len())
            .any(|w| w == p2_signer.as_slice())),
        "a delta signed by P's second device"
    );
    run.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_of_another_person_cannot_speak_for_a_domain() {
    let world = World::new(false);
    let run = Running::start(&world).await;
    let p1_key = world.p_devices[0].key.public();
    let p1_addr = EndpointAddr::new(p1_key)
        .with_ip_addr(SocketAddr::from(([127, 0, 0, 1], world.p_devices[0].port)));
    // A device certified by Q claims P's domain.
    for forged in [false, true] {
        let rogue = ServerKey::generate();
        let mut cert = world
            .q
            .key
            .issue_cert(&rogue.public(), "rogue", true, now());
        if forged {
            // Claims to be P's certificate, with Q's signature.
            cert.cert = world
                .p
                .key
                .issue_cert(&rogue.public(), "rogue", true, now())
                .cert;
        }
        let endpoint = FederationEndpoint::bind_device(
            rogue.secret().clone(),
            EndpointOptions::local(),
            LocalIdentity {
                domains: vec![world.p.domain()],
                capabilities: Vec::new(),
                rotations: Vec::new(),
            },
            Some(cert),
            Arc::new(KeyDomainAuthority),
            Arc::new(EchoHandler),
        )
        .await
        .unwrap();
        endpoint.add_peer_addr(p1_addr.clone());
        let refused = endpoint.connect(p1_key, None).await;
        assert!(
            matches!(
                refused,
                Err(resonator_federation::endpoint::RequestError::Refused { .. })
            ),
            "{refused:?}"
        );
        assert_ne!(
            run.p1
                .federation()
                .trust()
                .check(&rogue.public(), &world.p.domain(), &[])
                .await
                .unwrap(),
            Authorisation::Authorised
        );
        // Q's own domain it may speak for: Q's devices accept it.
        endpoint.shutdown().await;
    }
    run.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_device_is_cut_off_but_what_it_wrote_stays() {
    let world = World::new(false);
    let p1 = Node::start(world.p_config(0)).await;
    let p2 = Node::start(world.p_config(1)).await;
    let q1 = Node::start(world.q_config(0)).await;
    // Q's second device is away until after the revocation.
    let (mut q2, q2_setup) = Node::serve(world.q_config(1)).await;
    let alice1 = p1.user("alice").await;
    let alice2 = p2.user("alice").await;
    let bob1 = q1.user("bob").await;
    let bob2 = q2.user("bob").await;
    let wave = p1
        .create_wave(&alice1, &[&p1.address("alice"), &q1.address("bob")])
        .await;
    wait_until("P's second device gets the wave", || async {
        p2.text(&alice2, &wave).await.is_some()
    })
    .await;
    p2.append(&alice2, &wave, "[before]").await;
    let seats = [(&p1, alice1.as_str()), (&p2, &alice2), (&q1, &bob1)];
    converged(&seats, &wave, &["[before]"]).await;

    // P revokes its second device (on its admin device).
    let p2_key = world.p_devices[1].key.public();
    let list = world.p.key.revoke(None, [p2_key], now());
    assert!(p1.federation().publish_revocations(&list).await.unwrap());
    assert!(!p1.federation().publish_revocations(&list).await.unwrap());
    wait_until("Q's holder hears of the revocation", || async {
        q1.federation().trust().is_revoked(&p2_key)
    })
    .await;

    // Its next Hello is refused, by its own person's devices and others.
    for domain in [world.q.domain(), world.p.domain()] {
        let refused = p2
            .federation()
            .request(&domain, federation_request::Op::Echo(Echo::default()))
            .await;
        assert!(refused.is_err(), "{domain}: {refused:?}");
    }
    assert_eq!(
        q1.federation()
            .trust()
            .check(&p2_key, &world.p.domain(), &[])
            .await
            .unwrap(),
        Authorisation::Revoked
    );
    // Its new submissions are refused.
    let body = p2
        .data_api(&alice2, Node::append_ops(&wave, "[after]"))
        .await;
    assert!(body.as_array().unwrap()[0].get("error").is_some(), "{body}");
    p1.append(&alice1, &wave, "[still]").await;
    let text = converged(
        &[(&p1, alice1.as_str()), (&q1, &bob1)],
        &wave,
        &["[before]", "[still]"],
    )
    .await;
    assert!(!text.contains("[after]"));

    // Q's second device comes back: it gets the history with the revoked
    // device's delta from before, and accepts it.
    q2.federate(q2_setup).await;
    converged(
        &[(&p1, alice1.as_str()), (&q1, &bob1), (&q2, &bob2)],
        &wave,
        &["[before]", "[still]"],
    )
    .await;
    wait_until("Q's second device hears of the revocation", || async {
        q2.federation().trust().is_revoked(&p2_key)
    })
    .await;
    for node in [p1, p2, q1, q2] {
        node.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_person_key_recovered_from_its_phrase_certifies_a_new_device() {
    let world = World::new(true);
    let run = Running::start(&world).await;
    let wave = run.shared_wave().await;

    // A fresh device: the person key from the phrase alone.
    let spare = &world.p_devices[2];
    let phrase = RecoveryPhrase::parse(&world.p.phrase.words().to_uppercase()).unwrap();
    spare.make_admin(&phrase);
    let recovered = PersonKey::from_phrase(&phrase);
    assert_eq!(recovered.public(), world.p.key.public());
    let p3 = Node::start(world.p_config(2)).await;
    let alice3 = p3.user("alice").await;
    // Its self-issued certificate is accepted: it creates a wave through
    // the holder, which Q's devices get.
    let fresh = p3
        .create_wave(&alice3, &[&p3.address("alice"), &run.q1.address("bob")])
        .await;
    p3.append(&alice3, &fresh, "[from the new device]").await;
    converged(
        &[
            (&run.p1, run.alice1.as_str()),
            (&run.q1, &run.bob1),
            (&p3, &alice3),
        ],
        &fresh,
        &["[from the new device]"],
    )
    .await;
    let p3_key = world.p_devices[2].key.public();
    assert_eq!(
        run.q1
            .federation()
            .trust()
            .check(&p3_key, &world.p.domain(), &[])
            .await
            .unwrap(),
        Authorisation::Authorised
    );
    // A certificate the recovered key issues for yet another device
    // verifies as P's.
    let other = ServerKey::generate();
    let cert = recovered.issue_cert(&other.public(), "tablet", false, now());
    let learned = run.q2.federation().trust().learn_cert(&cert).await.unwrap();
    assert_eq!(learned.person, world.p.key.public());
    assert!(
        run.q2
            .federation()
            .trust()
            .verify_signer(&other.public(), &world.p.domain())
            .await
            .unwrap()
    );
    drop(wave);
    p3.stop().await;
    run.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_host_role_moves_between_a_persons_devices() {
    let world = World::new(false);
    let run = Running::start(&world).await;
    let wave = run.shared_wave().await;
    let p2_key = world.p_devices[1].key.public();

    // P's second device takes over (PersonDevices lets it).
    run.p2.federation().take_over(&wave.name, 1).await.unwrap();
    assert!(run.p2.server().is_local_wavelet(&wave.name));
    for node in [&run.p1, &run.q1, &run.q2] {
        wait_until(&format!("{} adopts the epoch", node.domain), || async {
            node.server().epochs(&wave.name).len() == 1
        })
        .await;
        assert!(!node.server().is_local_wavelet(&wave.name));
    }
    assert_eq!(
        run.p1.server().wavelet_host(&wave.name),
        WaveletHost::Delegated {
            domain: world.p.domain(),
            holder: p2_key.as_bytes().to_vec(),
        }
    );
    // Every device's edits now go to the new holder.
    run.p1.append(&run.alice1, &wave, "[p1 after]").await;
    run.q1.append(&run.bob1, &wave, "[q1 after]").await;
    run.p2.append(&run.alice2, &wave, "[p2 after]").await;
    converged(
        &run.seats(),
        &wave,
        &["[start]", "[p1 after]", "[q1 after]", "[p2 after]"],
    )
    .await;
    run.stop().await;
}

impl Node {
    /// Restarts this device with `config` (its directory kept).
    async fn restart(self, config: Config) -> Node {
        self.stop().await;
        Node::start(config).await
    }

    fn key(&self) -> resonator_federation::PublicKey {
        self.federation().endpoint().id()
    }

    fn lease(&self) -> resonator_federation::LeaseState {
        self.federation().lease_state()
    }

    /// The host epoch of `wave` here (0 for none).
    fn epoch(&self, wave: &Wave) -> u64 {
        self.server()
            .epochs(&wave.name)
            .last()
            .map_or(0, |record| record.epoch)
    }

    /// Marks the wave read for `token`'s user.
    async fn mark_read(&self, token: &str, wave: &Wave) {
        self.run(
            token,
            json!([{"id": "r", "method": "robot.folderAction", "params": {
                "waveId": wave.wave, "waveletId": wave.wavelet, "modifyHow": "markAsRead"
            }}]),
        )
        .await;
    }

    /// How many blips of `wave` are unread for `token`'s user (by its
    /// inbox digest; -1 if not in the inbox).
    async fn unread(&self, token: &str, wave: &Wave) -> i64 {
        let body = self
            .run(
                token,
                json!([{"id": "s", "method": "robot.search",
                        "params": {"query": "in:inbox", "numResults": 50}}]),
            )
            .await;
        body[0]["data"]["searchResults"]["digests"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|digest| digest["waveId"].as_str() == Some(wave.wave.as_str()))
            .and_then(|digest| digest["unreadCount"].as_i64())
            .unwrap_or(-1)
    }

    /// Uploads `bytes` as attachment `id` of the wave.
    async fn upload(&self, cookie: &str, id: &str, wave: &str, bytes: &[u8]) -> u16 {
        let form = reqwest::multipart::Form::new()
            .text("attachmentId", id.to_owned())
            .text("waveRef", wave.to_owned())
            .part(
                "uploadFormElement",
                reqwest::multipart::Part::bytes(bytes.to_vec()).file_name("notes.txt"),
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

    /// `GET /attachment/<id>?waveRef=<wave>`: the status and the body.
    async fn download(&self, cookie: &str, id: &str, wave: &str) -> (u16, Vec<u8>) {
        let response = self
            .http
            .get(self.url(&format!("/attachment/{id}")))
            .query(&[("waveRef", wave)])
            .header("Cookie", cookie)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.bytes().await.unwrap().to_vec())
    }
}

/// P with an anchor (A, the admin device), a laptop (L) and a phone (F);
/// Q with one device. The anchor gets the lease; when it dies the laptop
/// takes over at the next epoch and Q's edits keep flowing; the phone,
/// cut off, never takes over; the anchor comes back, steps down without
/// losing anything and gets the lease back by handover. A wave of Q
/// reaches P through the one device it can reach, and the phone catches
/// up (waves, edits and read state) when it is back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_host_lease_follows_a_persons_devices() {
    let world = World::new(true);
    let a = Node::start(world.p_config(0)).await;
    let l = Node::start(world.p_config(1)).await;
    let f = Node::start(world.p_config(2)).await;
    let q = Node::start(world.q_config(0)).await;
    wait_for_holder(&[&a, &l, &f], &a).await;
    assert!(a.lease().anchor && !l.lease().anchor);
    let a_key = a.key();
    let alice_a = a.user("alice").await;
    let alice_l = l.user("alice").await;
    let alice_f = f.user("alice").await;
    let bob = q.user("bob").await;
    let wave = a
        .create_wave(&alice_a, &[&a.address("alice"), &q.address("bob")])
        .await;
    a.append(&alice_a, &wave, "[start]").await;
    converged(
        &[
            (&a, alice_a.as_str()),
            (&l, &alice_l),
            (&f, &alice_f),
            (&q, &bob),
        ],
        &wave,
        &["[start]"],
    )
    .await;
    let term = a.lease().term;

    // The phone goes away (but keeps running); then the anchor dies.
    let f = f.restart(world.p_cut_off(2)).await;
    a.stop().await;
    wait_until("the laptop takes the lease over", || async {
        l.lease().held_here && l.lease().term > term
    })
    .await;
    assert_eq!(l.epoch(&wave), 1, "taken over at the next epoch");
    assert!(l.server().is_local_wavelet(&wave.name));
    wait_until("Q follows the laptop", || async { q.epoch(&wave) == 1 }).await;
    q.append(&bob, &wave, "[q while a is away]").await;
    l.append(&alice_l, &wave, "[l while a is away]").await;
    let seats = [(&l, alice_l.as_str()), (&q, &bob)];
    converged(
        &seats,
        &wave,
        &["[start]", "[q while a is away]", "[l while a is away]"],
    )
    .await;
    // Q's wave with alice reaches P through the laptop alone.
    let q_wave = q
        .create_wave(&bob, &[&q.address("bob"), &l.address("alice")])
        .await;
    q.append(&bob, &q_wave, "[q wave]").await;
    converged(&seats, &q_wave, &["[q wave]"]).await;
    // The phone, cut off, never takes the lease, however long it waits.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!f.lease().held_here && !f.lease().online, "{:?}", f.lease());
    assert_eq!(f.epoch(&wave), 0);

    // The anchor comes back: it follows the laptop, then gets the lease
    // back by handover and takes the wavelets over again.
    let a = Node::start(world.p_config(0)).await;
    wait_until("the anchor holds the lease again", || async {
        a.lease().held_here && l.lease().holder == Some(a_key)
    })
    .await;
    let lease = a.federation().lease().unwrap();
    assert_eq!(lease.handover_from, Some(l.key()));
    wait_until("the anchor hosts the wave again", || async {
        a.epoch(&wave) == 2 && a.server().is_local_wavelet(&wave.name)
    })
    .await;
    a.append(&alice_a, &wave, "[a back]").await;
    let all = [
        "[start]",
        "[q while a is away]",
        "[l while a is away]",
        "[a back]",
    ];
    converged(
        &[(&a, alice_a.as_str()), (&l, &alice_l), (&q, &bob)],
        &wave,
        &all,
    )
    .await;
    // Read state marked on the laptop (while the phone is away).
    assert!(f.unread(&alice_f, &wave).await != 0);
    l.mark_read(&alice_l, &wave).await;
    assert_eq!(l.unread(&alice_l, &wave).await, 0);

    // The phone comes back: it catches up with all of it.
    let f = f.restart(world.p_config(2)).await;
    let everyone = [
        (&a, alice_a.as_str()),
        (&l, &alice_l),
        (&f, &alice_f),
        (&q, &bob),
    ];
    converged(&everyone, &wave, &all).await;
    converged(&everyone, &q_wave, &["[q wave]"]).await;
    wait_until("read state reaches the phone", || async {
        f.unread(&alice_f, &wave).await == 0
    })
    .await;
    assert!(!f.lease().held_here);
    for node in [a, l, f, q] {
        node.stop().await;
    }
}

/// The anchor hands the lease to the phone; the phone (no anchor) hands
/// it back to the reachable anchor with a signed handover.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_signed_handover_moves_the_lease() {
    let world = World::new(true);
    let a = Node::start(world.p_config(0)).await;
    let f = Node::start(world.p_config(2)).await;
    let q = Node::start(world.q_config(0)).await;
    wait_for_holder(&[&a, &f], &a).await;
    let alice_a = a.user("alice").await;
    let alice_f = f.user("alice").await;
    let bob = q.user("bob").await;
    let wave = a
        .create_wave(&alice_a, &[&a.address("alice"), &q.address("bob")])
        .await;
    a.append(&alice_a, &wave, "[start]").await;
    let term = a.lease().term;

    // Not the holder: no handover.
    assert!(f.federation().handover(a.key()).await.is_err());
    a.federation().handover(f.key()).await.unwrap();
    wait_until("the phone held the lease and gave it back", || async {
        a.federation()
            .lease()
            .is_some_and(|lease| lease.term == term + 2 && lease.device == a.key())
    })
    .await;
    let lease = a.federation().lease().unwrap();
    assert_eq!(lease.handover_from, Some(f.key()));
    assert_eq!(lease.prev, Some(f.key()));
    wait_until("each took the wave over in turn", || async {
        a.epoch(&wave) == 2 && f.epoch(&wave) == 2 && q.epoch(&wave) == 2
    })
    .await;
    f.append(&alice_f, &wave, "[f]").await;
    q.append(&bob, &wave, "[q]").await;
    converged(
        &[(&a, alice_a.as_str()), (&f, &alice_f), (&q, &bob)],
        &wave,
        &["[start]", "[f]", "[q]"],
    )
    .await;
    for node in [a, f, q] {
        node.stop().await;
    }
}

/// Edits made on a phone with nobody to reach (to P's and to Q's waves)
/// wait in its queue, survive a restart, and are applied once each when
/// it is back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pending_edits_are_sent_once_after_a_restart() {
    let world = World::new(true);
    let a = Node::start(world.p_config(0)).await;
    let f = Node::start(world.p_config(2)).await;
    let q = Node::start(world.q_config(0)).await;
    wait_for_holder(&[&a, &f], &a).await;
    let alice_a = a.user("alice").await;
    let alice_f = f.user("alice").await;
    let bob = q.user("bob").await;
    let p_wave = a
        .create_wave(&alice_a, &[&a.address("alice"), &q.address("bob")])
        .await;
    a.append(&alice_a, &p_wave, "[p start]").await;
    let q_wave = q
        .create_wave(&bob, &[&q.address("bob"), &a.address("alice")])
        .await;
    q.append(&bob, &q_wave, "[q start]").await;
    let seats = [(&a, alice_a.as_str()), (&f, &alice_f), (&q, &bob)];
    converged(&seats, &p_wave, &["[p start]"]).await;
    converged(&seats, &q_wave, &["[q start]"]).await;

    let f = f.restart(world.p_cut_off(2)).await;
    for (wave, text) in [(&p_wave, "[f p one]"), (&q_wave, "[f q one]")] {
        let body = f.data_api(&alice_f, Node::append_ops(wave, text)).await;
        assert!(body[0].get("error").is_none(), "{body}");
    }
    f.append(&alice_f, &p_wave, "[f p two]").await;
    assert_eq!(f.server().pending_count().await, 3);
    a.append(&alice_a, &p_wave, "[a meanwhile]").await;

    // Restarted while cut off: still queued.
    let f = f.restart(world.p_cut_off(2)).await;
    assert_eq!(f.server().pending_count().await, 3);
    // Back: sent in order, once.
    let f = f.restart(world.p_config(2)).await;
    let seats = [(&a, alice_a.as_str()), (&f, &alice_f), (&q, &bob)];
    converged(
        &seats,
        &p_wave,
        &["[p start]", "[a meanwhile]", "[f p one]", "[f p two]"],
    )
    .await;
    converged(&seats, &q_wave, &["[q start]", "[f q one]"]).await;
    wait_until("the queue is empty", || async {
        f.server().pending_count().await == 0
    })
    .await;
    for node in [a, f, q] {
        node.stop().await;
    }
}

/// The anchor is cut off but keeps its lease and its edits; the laptop
/// takes over meanwhile. When they meet again the newer term wins and
/// nothing written on either side is lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_split_brain_heals_without_losing_edits() {
    let world = World::new(false);
    let a = Node::start(world.p_config(0)).await;
    let l = Node::start(world.p_config(1)).await;
    let q = Node::start(world.q_config(0)).await;
    wait_for_holder(&[&a, &l], &a).await;
    let alice_a = a.user("alice").await;
    let alice_l = l.user("alice").await;
    let bob = q.user("bob").await;
    let wave = a
        .create_wave(&alice_a, &[&a.address("alice"), &q.address("bob")])
        .await;
    a.append(&alice_a, &wave, "[start]").await;
    converged(
        &[(&a, alice_a.as_str()), (&l, &alice_l), (&q, &bob)],
        &wave,
        &["[start]"],
    )
    .await;

    let a = a.restart(world.p_cut_off(0)).await;
    wait_until("the laptop takes over", || async {
        l.lease().held_here && l.epoch(&wave) >= 1
    })
    .await;
    // Both think they host the wave now.
    wait_until("the anchor still holds its lease", || async {
        a.lease().held_here && a.server().is_local_wavelet(&wave.name)
    })
    .await;
    a.append(&alice_a, &wave, "[a alone]").await;
    q.append(&bob, &wave, "[q]").await;
    l.append(&alice_l, &wave, "[l]").await;

    let a = a.restart(world.p_config(0)).await;
    let seats = [(&a, alice_a.as_str()), (&l, &alice_l), (&q, &bob)];
    converged(&seats, &wave, &["[start]", "[a alone]", "[q]", "[l]"]).await;
    wait_until("one holder", || async {
        let holder = a.lease().holder;
        holder.is_some()
            && l.lease().holder == holder
            && (a.lease().held_here ^ l.lease().held_here)
    })
    .await;
    a.append(&alice_a, &wave, "[after]").await;
    converged(
        &seats,
        &wave,
        &["[start]", "[a alone]", "[q]", "[l]", "[after]"],
    )
    .await;
    for node in [a, l, q] {
        node.stop().await;
    }
}

/// An attachment uploaded on one device of P is fetched by another
/// device of P and by Q.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attachments_travel_between_a_persons_devices() {
    let world = World::new(false);
    let run = Running::start(&world).await;
    let wave = run.shared_wave().await;
    let alice_l = run.p2.cookie("alice").await;
    let alice_a = run.p1.cookie("alice").await;
    let bob = run.q1.cookie("bob").await;
    let mallory = run.q1.cookie("mallory").await;
    let id = format!("{}/notes", world.p.domain());
    let bytes = b"notes of alice, uploaded on her laptop".to_vec();
    assert_eq!(run.p2.upload(&alice_l, &id, &wave.wave, &bytes).await, 201);
    assert_eq!(
        run.p1.download(&alice_a, &id, &wave.wave).await,
        (200, bytes.clone())
    );
    assert_eq!(
        run.q1.download(&bob, &id, &wave.wave).await,
        (200, bytes.clone())
    );
    assert_eq!(
        run.q2
            .download(&run.q2.cookie("bob").await, &id, &wave.wave)
            .await
            .0,
        200
    );
    assert_eq!(run.q1.download(&mallory, &id, &wave.wave).await.0, 403);
    run.stop().await;
}

// --- Attachments copied ahead between a person's devices ---

/// A PNG of `width` × `height` pixels.
fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
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
    fn attachments(&self) -> &resonator_server::attachment::AttachmentService {
        &self.app.as_ref().unwrap().state().attachments
    }

    /// Whether this device holds the bytes of attachment `id` (uploaded
    /// here or copied), without asking anyone.
    async fn holds(&self, id: &str) -> bool {
        let id = resonator_server::attachment::AttachmentId::deserialise(id).unwrap();
        matches!(
            self.attachments().metadata(&id).await,
            Ok(Some(metadata)) if !metadata.evicted
        )
    }

    /// Uploads `bytes` as attachment `id` of the wave, named `file_name`.
    async fn upload_file(
        &self,
        cookie: &str,
        id: &str,
        wave: &str,
        bytes: &[u8],
        file_name: &str,
    ) -> u16 {
        let form = reqwest::multipart::Form::new()
            .text("attachmentId", id.to_owned())
            .text("waveRef", wave.to_owned())
            .part(
                "uploadFormElement",
                reqwest::multipart::Part::bytes(bytes.to_vec()).file_name(file_name.to_owned()),
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

    /// `GET /thumbnail/<id>?waveRef=<wave>`: the status and the body.
    async fn thumbnail(&self, cookie: &str, id: &str, wave: &str) -> (u16, Vec<u8>) {
        let response = self
            .http
            .get(self.url(&format!("/thumbnail/{id}")))
            .query(&[("waveRef", wave)])
            .header("Cookie", cookie)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.bytes().await.unwrap().to_vec())
    }
}

/// P's two devices (Q stays off) with alice on each and a wave of hers
/// that both have; `configure` adjusts both configs.
async fn two_devices_of_p(
    world: &World,
    configure: impl Fn(&mut Config),
) -> (Node, Node, String, String, Wave) {
    let mut config = world.p_config(0);
    configure(&mut config);
    let p1 = Node::start(config).await;
    let mut config = world.p_config(1);
    configure(&mut config);
    let p2 = Node::start(config).await;
    wait_for_holder(&[&p1, &p2], &p1).await;
    let alice1 = p1.user("alice").await;
    let alice2 = p2.user("alice").await;
    let wave = p1.create_wave(&alice1, &[&p1.address("alice")]).await;
    p1.append(&alice1, &wave, "[start]").await;
    converged(&[(&p1, &alice1), (&p2, &alice2)], &wave, &["[start]"]).await;
    let cookie1 = p1.cookie("alice").await;
    let cookie2 = p2.cookie("alice").await;
    (p1, p2, cookie1, cookie2, wave)
}

/// An image uploaded on one device of P (the tablet, p2) is copied to the
/// other (p1) at once, without anyone there asking for it; once the
/// tablet is asleep (stopped), p1 still serves it and its thumbnail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_upload_is_copied_ahead_to_a_persons_other_devices() {
    let world = World::new(false);
    let (p1, p2, cookie1, cookie2, wave) = two_devices_of_p(&world, |_| {}).await;
    let id = format!("{}/photo", world.p.domain());
    let picture = png(320, 240);
    assert_eq!(
        p2.upload_file(&cookie2, &id, &wave.wave, &picture, "photo.png")
            .await,
        201
    );
    wait_until("the copy on p1", || p1.holds(&id)).await;
    // Copied, not fetched for a user: p1's copy came without a download.
    let copy = p1
        .attachments()
        .metadata(&resonator_server::attachment::AttachmentId::deserialise(&id).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(copy.size, picture.len() as u64);
    assert!(copy.image.is_some(), "the copy has its thumbnail");
    assert_eq!(copy.origin.as_deref(), Some(world.p.domain().as_str()));

    p2.stop().await;
    assert_eq!(p1.download(&cookie1, &id, &wave.wave).await, (200, picture));
    let (status, thumbnail) = p1.thumbnail(&cookie1, &id, &wave.wave).await;
    assert_eq!(status, 200);
    assert!(!thumbnail.is_empty());
    p1.stop().await;
}

/// A device that is away during an upload gets the attachment when it is
/// back and syncs, while the device it was uploaded to is still awake.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_away_during_an_upload_copies_it_when_back() {
    let world = World::new(false);
    let (p1, p2, cookie1, _, wave) = two_devices_of_p(&world, |_| {}).await;
    p2.stop().await;
    let ids: Vec<String> = (0..3)
        .map(|i| format!("{}/away-{i}", world.p.domain()))
        .collect();
    let pictures: Vec<Vec<u8>> = (0..3).map(|i| png(64 + i, 48)).collect();
    for (id, picture) in ids.iter().zip(&pictures) {
        assert_eq!(
            p1.upload_file(&cookie1, id, &wave.wave, picture, "away.png")
                .await,
            201
        );
    }
    // Away for longer than telling it could take (request timeout 2 s):
    // only the catch-up after sync brings the copies.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let p2 = Node::start(world.p_config(1)).await;
    for id in &ids {
        wait_until("the copy on p2", || p2.holds(id)).await;
    }
    assert_eq!(p2.attachments().remote_fetch_count(), 3);
    // p1 asleep: p2 serves its copies.
    p1.stop().await;
    let cookie2 = p2.cookie("alice").await;
    for (id, picture) in ids.iter().zip(&pictures) {
        assert_eq!(
            p2.download(&cookie2, id, &wave.wave).await,
            (200, picture.clone())
        );
        assert_eq!(p2.thumbnail(&cookie2, id, &wave.wave).await.0, 200);
    }
    p2.stop().await;
}

/// Attachments over `replicate_max_mb` are not copied ahead (neither when
/// uploaded nor at catch-up); they are fetched when opened. With
/// `replicate_own_attachments` off nothing is copied ahead.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_attachments_within_the_limit_are_copied_ahead() {
    let world = World::new(false);
    let (p1, p2, cookie1, cookie2, wave) = two_devices_of_p(&world, |config| {
        config.federation.replicate_max_mb = 1;
    })
    .await;
    let large_id = format!("{}/large", world.p.domain());
    let large = vec![7u8; 1024 * 1024 + 1];
    assert_eq!(
        p2.upload_file(&cookie2, &large_id, &wave.wave, &large, "large.bin")
            .await,
        201
    );
    let small_id = format!("{}/small", world.p.domain());
    assert_eq!(
        p2.upload_file(&cookie2, &small_id, &wave.wave, b"small", "small.txt")
            .await,
        201
    );
    wait_until("the small copy on p1", || p1.holds(&small_id)).await;
    // A few sync rounds (with their catch-up) later, still not copied.
    tokio::time::sleep(tuning().sync_interval * 4).await;
    assert!(!p1.holds(&large_id).await);
    assert_eq!(p1.attachments().remote_fetch_count(), 1);
    // Fetched when opened.
    assert_eq!(
        p1.download(&cookie1, &large_id, &wave.wave).await,
        (200, large)
    );
    p1.stop().await;
    p2.stop().await;

    // Switched off: an upload stays where it was made.
    let world = World::new(false);
    let (p1, p2, cookie1, _, wave) = two_devices_of_p(&world, |config| {
        config.federation.replicate_own_attachments = false;
    })
    .await;
    let id = format!("{}/kept", world.p.domain());
    assert_eq!(
        p1.upload_file(&cookie1, &id, &wave.wave, b"kept", "kept.txt")
            .await,
        201
    );
    tokio::time::sleep(tuning().sync_interval * 4).await;
    assert!(!p2.holds(&id).await);
    assert_eq!(p2.attachments().remote_fetch_count(), 0);
    p1.stop().await;
    p2.stop().await;
}

/// A device whose cache has no room for a copy skips it: copying ahead
/// never evicts what users fetched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_without_room_in_its_cache_skips_copies() {
    let world = World::new(false);
    let (p1, p2, _, cookie2, wave) = two_devices_of_p(&world, |config| {
        config.federation.attachment_cache_mb = 1;
    })
    .await;
    let first = format!("{}/first", world.p.domain());
    assert_eq!(
        p2.upload_file(
            &cookie2,
            &first,
            &wave.wave,
            &vec![1u8; 700 * 1024],
            "a.bin"
        )
        .await,
        201
    );
    wait_until("the first copy on p1", || p1.holds(&first)).await;
    let second = format!("{}/second", world.p.domain());
    assert_eq!(
        p2.upload_file(
            &cookie2,
            &second,
            &wave.wave,
            &vec![2u8; 700 * 1024],
            "b.bin"
        )
        .await,
        201
    );
    tokio::time::sleep(tuning().sync_interval * 4).await;
    assert!(!p1.holds(&second).await);
    assert!(p1.holds(&first).await, "nothing was evicted");
    p1.stop().await;
    p2.stop().await;
}

/// A phone in device mode (`resonator_node`) that holds its person's host
/// lease hands it to the person's other device when the app goes to the
/// background, waits until that device holds it, and syncs with it when
/// it comes back. (With a reachable anchor, the phone's lease task hands
/// the lease to the anchor on its own; `enter_background` does it at
/// once, anchors first.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_going_to_the_background_hands_the_lease_over() {
    let world = World::new(false);
    let desktop_device = &world.p_devices[0];
    let desktop = Node::start(desktop_device.config(
        &world.p,
        true,
        false,
        &world.peers_of(desktop_device),
        &world.allow(),
    ))
    .await;
    let mut config = world.p_config(1);
    config.mode = resonator_server::config::Mode::Device;
    let phone = resonator_node::launch_with(
        config,
        resonator_node::LaunchOptions {
            tuning: Some(tuning()),
            resolver: Some(Arc::new(StaticTxtResolver::new())),
        },
    )
    .await
    .expect("the phone starts");
    let phone_federation = phone.federation().expect("federation");
    let phone_key = phone_federation.endpoint().id();
    assert_eq!(phone.config().domain, world.p.domain());
    assert!(phone.token().is_some());

    // Whoever took the lease first, the phone ends up holding it.
    wait_until("a holder", || async {
        desktop.federation().lease().is_some()
    })
    .await;
    if desktop.federation().lease_state().held_here {
        desktop.federation().handover(phone_key).await.unwrap();
    }
    wait_until("the phone holds the lease", || async {
        phone_federation.lease_state().held_here
            && desktop.federation().lease_state().holder == Some(phone_key)
    })
    .await;
    wait_until("the phone sees the desktop", || async {
        phone_federation
            .own_devices()
            .iter()
            .any(|device| device.key == desktop.key() && device.reachable && !device.anchor)
    })
    .await;

    let report = phone.enter_background(Duration::from_secs(10)).await;
    assert_eq!(report.handed_over_to, Some(desktop.key().to_z32()));
    assert!(report.handover_complete, "{report:?}");
    assert_eq!(report.pending_left, 0);
    assert!(!phone_federation.lease_state().held_here);
    wait_for_holder(&[&desktop], &desktop).await;

    // Not the holder: nothing to hand over.
    let again = phone.enter_background(Duration::from_secs(2)).await;
    assert_eq!(again.handed_over_to, None);

    let back = phone.enter_foreground(Duration::from_secs(10)).await;
    assert_eq!(back.devices_synced, 1, "{back:?}");
    let lease = back.lease.expect("person mode");
    assert!(!lease.held_here);
    assert_eq!(lease.holder, Some(desktop.key()));
    phone.shutdown().await;
    desktop.stop().await;
}

// --- Phase 5: pairing, recovery, continuing a wave, co-hosts ---

/// A fresh device's config: a person of its own (as device mode makes
/// on the first start), or, once paired, whatever its certificate says.
fn fresh_config(device: &Device, peers: &[&Device], allow: &[String]) -> Config {
    let mut config = Config {
        listen: vec!["127.0.0.1:0".to_owned()],
        database_path: device.dir.path().join("resonator.db"),
        register_max_per_hour: 0,
        snapshot_interval: 2,
        ..Config::default()
    };
    config.federation.enabled = true;
    config.federation.mode = FederationMode::Allowlist;
    config.federation.allow = allow.to_vec();
    config.federation.relay = "off".to_owned();
    config.federation.discovery = "off".to_owned();
    config.federation.listen = format!("127.0.0.1:{}", device.port);
    config.federation.request_timeout_secs = 2;
    config.federation.peers = peers.iter().map(|p| p.peer()).collect();
    config.person.enabled = true;
    config.person.admin = true;
    config.person.device_name = format!("fresh {}", &device.z32()[..6]);
    FederationSetup::configure_person(&mut config).unwrap();
    config
}

/// A fresh device with a person key of its own.
fn fresh_device(p: &Person) -> Device {
    let device = Device::new(p);
    device.make_admin(&Person::new().phrase);
    device
}

fn pair_error(
    result: Result<
        resonator_federation::control::JoinSummary,
        resonator_federation::pairing::PairError,
    >,
) -> String {
    match result {
        Ok(summary) => panic!("paired: {summary:?}"),
        Err(error) => error.to_string(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_pairs_by_link_once_and_catches_up() {
    use resonator_federation::pairing::{decode_link, encode_link};
    let world = World::new(false);
    // P's admin device, alone so far.
    let p1 = Node::start(world.p_cut_off(0)).await;
    wait_for_holder(&[&p1], &p1).await;
    let alice1 = p1.user("alice").await;
    let wave = p1.create_wave(&alice1, &[&p1.address("alice")]).await;
    p1.append(&alice1, &wave, "[before pairing]").await;
    p1.mark_read(&alice1, &wave).await;

    let b_device = fresh_device(&world.p);
    let c_device = fresh_device(&world.p);
    let mut allow = world.allow();
    let b_config = fresh_config(&b_device, &[], &allow);
    let c_config = fresh_config(&c_device, &[], &allow);
    allow.push(b_config.domain.clone());
    allow.push(c_config.domain.clone());
    let b = Node::start(fresh_config(&b_device, &[], &allow)).await;
    let c = Node::start(fresh_config(&c_device, &[], &allow)).await;
    assert_ne!(b.domain, world.p.domain());
    let ops = p1.federation().person();
    let wait = Duration::from_secs(2);

    // A wrong secret is refused, and so is a link that ran out.
    let invitation = ops.invite(false).await.unwrap();
    let mut forged = decode_link(&invitation.link).unwrap();
    forged.secret.as_mut().unwrap()[0] ^= 1;
    let error = pair_error(
        b.federation()
            .person()
            .join(&encode_link(&forged), "b", wait)
            .await,
    );
    assert!(error.contains("refused"), "{error}");
    let mut stale = decode_link(&invitation.link).unwrap();
    stale.expires_at = Some(now() - 1);
    let error = pair_error(
        b.federation()
            .person()
            .join(&encode_link(&stale), "b", wait)
            .await,
    );
    assert!(error.contains("expired"), "{error}");
    // Expired at the inviting device (whatever the link says).
    let expired = ops.invite(false).await.unwrap();
    let store = p1.server().database().federation_store();
    let mut record = store
        .pair_invitation(&expired.pending.id)
        .await
        .unwrap()
        .unwrap();
    store.delete_pair_invitation(&record.id).await.unwrap();
    record.expires_at = now() - 1;
    store.put_pair_invitation(record).await.unwrap();
    pair_error(b.federation().person().join(&expired.link, "b", wait).await);

    // The right link pairs b, once.
    let summary = b
        .federation()
        .person()
        .join(&invitation.link, "tablet", wait)
        .await
        .unwrap();
    assert_eq!(summary.person, world.p.domain());
    assert!(!summary.admin);
    let error = pair_error(
        c.federation()
            .person()
            .join(&invitation.link, "c", wait)
            .await,
    );
    assert!(error.contains("refused"), "{error}");

    // An admin pairing waits for confirmation on the inviting device.
    let admin = ops.invite(true).await.unwrap();
    let error = pair_error(
        c.federation()
            .person()
            .join(&admin.link, "laptop", Duration::from_millis(500))
            .await,
    );
    assert!(error.contains("confirm"), "{error}");
    let pending = ops.invitations().await.unwrap();
    let asked = pending.iter().find(|p| p.id == admin.pending.id).unwrap();
    assert_eq!(asked.state, "requested");
    assert_eq!(asked.joiner_name.as_deref(), Some("laptop"));
    let confirming = async {
        wait_until("confirmation", || async {
            ops.confirm(&admin.pending.id).await.is_ok()
        })
        .await;
    };
    let (joined, ()) = tokio::join!(
        c.federation()
            .person()
            .join(&admin.link, "laptop", Duration::from_secs(20)),
        confirming
    );
    let joined = joined.unwrap();
    assert!(joined.admin);
    let stored = FileKeyStore::new(c_device.dir.path().join("person.key"))
        .load()
        .unwrap()
        .unwrap();
    assert_eq!(
        PersonKey::from_phrase(&stored).public(),
        world.p.key.public()
    );

    // b starts again as P's device and catches up: waves and read state.
    let b = b.restart(fresh_config(&b_device, &[], &allow)).await;
    assert_eq!(b.domain, world.p.domain());
    let c = c.restart(fresh_config(&c_device, &[], &allow)).await;
    assert_eq!(c.domain, world.p.domain());
    let version = p1.version(&wave).await;
    wait_until("the paired device has the wave", || async {
        b.server().current_version(&wave.name).await.ok() == Some(version.clone())
    })
    .await;
    let alice_b = b.user("alice").await;
    wait_until("read state reaches the paired device", || async {
        b.unread(&alice_b, &wave).await == 0
    })
    .await;
    let devices = p1.federation().person().overview().unwrap().devices;
    assert!(devices.iter().any(|d| d.name == "tablet" && !d.admin));
    assert!(devices.iter().any(|d| d.name == "laptop" && d.admin));
    for node in [p1, b, c] {
        node.stop().await;
    }
}

/// `code` with other words than its own.
fn wrong_words(code: &str) -> String {
    let (nameplate, words) = code.split_once('-').unwrap();
    let other = if words == "abandon-ability" {
        "zoo-zoo"
    } else {
        "abandon-ability"
    };
    format!("{nameplate}-{other}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_pairs_by_code_and_a_code_takes_three_wrong_tries() {
    use resonator_federation::pair_code::{PairCode, lookup};
    let world = World::new(false);
    let p1 = Node::start(world.p_cut_off(0)).await;
    wait_for_holder(&[&p1], &p1).await;
    let alice1 = p1.user("alice").await;
    let wave = p1.create_wave(&alice1, &[&p1.address("alice")]).await;
    p1.append(&alice1, &wave, "[before pairing by code]").await;

    let b_device = fresh_device(&world.p);
    let c_device = fresh_device(&world.p);
    let mut allow = world.allow();
    allow.push(fresh_config(&b_device, &[], &allow).domain);
    allow.push(fresh_config(&c_device, &[], &allow).domain);
    let b = Node::start(fresh_config(&b_device, &[], &allow)).await;
    let c = Node::start(fresh_config(&c_device, &[], &allow)).await;
    let ops = p1.federation().person();
    let store = p1.server().database().federation_store();
    let wait = Duration::from_secs(2);
    let failures = |id: String| {
        let store = store.clone();
        async move {
            store
                .pair_invitation(&id)
                .await
                .unwrap()
                .map(|record| record.code_failures)
        }
    };

    // Not a code, and a code nobody published.
    let error = pair_error(b.federation().person().join("hello there", "b", wait).await);
    assert!(error.contains("digits"), "{error}");
    let unknown = PairCode::generate().unwrap().to_string();
    let error = pair_error(b.federation().person().join(&unknown, "b", wait).await);
    assert!(error.contains("not found"), "{error}");

    // Wrong words are refused and counted; the right code (typed loosely)
    // still pairs b, once.
    let invitation = ops.invite(false).await.unwrap();
    let code = invitation.code.clone().expect("a code");
    assert!(!format!("{invitation:?}").contains(&code));
    let error = pair_error(
        b.federation()
            .person()
            .join(&wrong_words(&code), "b", wait)
            .await,
    );
    assert_eq!(error, "wrong code (2 tries left)");
    assert_eq!(failures(invitation.pending.id.clone()).await, Some(1));
    let error = pair_error(
        b.federation()
            .person()
            .join(&wrong_words(&code), "b", wait)
            .await,
    );
    assert_eq!(error, "wrong code (1 try left)");
    let typed = format!("  {} ", code.to_uppercase().replace('-', " "));
    let summary = b
        .federation()
        .person()
        .join(&typed, "tablet", wait)
        .await
        .unwrap();
    assert_eq!(summary.person, world.p.domain());
    assert!(!summary.admin);
    assert_eq!(failures(invitation.pending.id.clone()).await, None);
    let error = pair_error(c.federation().person().join(&code, "c", wait).await);
    assert!(error.contains("not found"), "{error}");
    // The record is withdrawn once the code is used.
    let nameplate = PairCode::parse(&code).unwrap().nameplate().to_owned();
    wait_until("the rendezvous record is withdrawn", || async {
        lookup(rendezvous().as_ref(), &nameplate, now())
            .await
            .unwrap()
            .is_none()
    })
    .await;

    // Three wrong codes burn the invitation, even for the right one later.
    let burnt = ops.invite(false).await.unwrap();
    let burnt_code = burnt.code.clone().unwrap();
    for left in ["2 tries left", "1 try left", "used up"] {
        let error = pair_error(
            c.federation()
                .person()
                .join(&wrong_words(&burnt_code), "c", wait)
                .await,
        );
        assert!(error.contains(left), "{error}");
    }
    assert_eq!(failures(burnt.pending.id.clone()).await, None);
    let error = pair_error(c.federation().person().join(&burnt_code, "c", wait).await);
    assert!(error.contains("not found"), "{error}");
    // The link of a burnt invitation is dead too.
    let error = pair_error(c.federation().person().join(&burnt.link, "c", wait).await);
    assert!(error.contains("refused"), "{error}");

    // A code that expired at the inviting device is not found.
    let expired = ops.invite(false).await.unwrap();
    let mut record = store
        .pair_invitation(&expired.pending.id)
        .await
        .unwrap()
        .unwrap();
    store.delete_pair_invitation(&record.id).await.unwrap();
    record.expires_at = now() - 1;
    store.put_pair_invitation(record).await.unwrap();
    let error = pair_error(
        c.federation()
            .person()
            .join(expired.code.as_deref().unwrap(), "c", wait)
            .await,
    );
    assert!(error.contains("not found"), "{error}");

    // A withdrawn invitation's code is gone.
    let cancelled = ops.invite(false).await.unwrap();
    assert!(ops.cancel(&cancelled.pending.id).await.unwrap());
    let error = pair_error(
        c.federation()
            .person()
            .join(cancelled.code.as_deref().unwrap(), "c", wait)
            .await,
    );
    assert!(error.contains("not found"), "{error}");

    // An admin pairing by code waits for confirmation, then hands the
    // person key over.
    let admin = ops.invite(true).await.unwrap();
    let admin_code = admin.code.clone().unwrap();
    let error = pair_error(
        c.federation()
            .person()
            .join(&admin_code, "laptop", Duration::from_millis(500))
            .await,
    );
    assert!(error.contains("confirm"), "{error}");
    let asked = ops.invitations().await.unwrap();
    let asked = asked.iter().find(|p| p.id == admin.pending.id).unwrap();
    assert_eq!(asked.state, "requested");
    assert_eq!(asked.joiner_name.as_deref(), Some("laptop"));
    let confirming = async {
        wait_until("confirmation", || async {
            ops.confirm(&admin.pending.id).await.is_ok()
        })
        .await;
    };
    let (joined, ()) = tokio::join!(
        c.federation()
            .person()
            .join(&admin_code, "laptop", Duration::from_secs(20)),
        confirming
    );
    assert!(joined.unwrap().admin);
    let stored = FileKeyStore::new(c_device.dir.path().join("person.key"))
        .load()
        .unwrap()
        .unwrap();
    assert_eq!(
        PersonKey::from_phrase(&stored).public(),
        world.p.key.public()
    );

    // b starts again as P's device and syncs the wave.
    let b = b.restart(fresh_config(&b_device, &[], &allow)).await;
    assert_eq!(b.domain, world.p.domain());
    let c = c.restart(fresh_config(&c_device, &[], &allow)).await;
    assert_eq!(c.domain, world.p.domain());
    let version = p1.version(&wave).await;
    wait_until("the device paired by code has the wave", || async {
        b.server().current_version(&wave.name).await.ok() == Some(version.clone())
    })
    .await;
    let devices = p1.federation().person().overview().unwrap().devices;
    assert!(devices.iter().any(|d| d.name == "tablet" && !d.admin));
    assert!(devices.iter().any(|d| d.name == "laptop" && d.admin));
    for node in [p1, b, c] {
        node.stop().await;
    }
}

/// Pairing by code through n0's public pkarr relay (the devices talk
/// over loopback); run by hand:
/// `cargo test -p resonator --test persons -- --ignored over_n0`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the internet"]
async fn a_device_pairs_by_code_over_n0s_relay() {
    let n0: Arc<dyn resonator_federation::pair_code::Rendezvous> = Arc::new(
        resonator_federation::pair_code::PkarrRendezvous::new(
            resonator_server::config::DEFAULT_PAIRING_RENDEZVOUS,
        )
        .unwrap(),
    );
    let world = World::new(false);
    let p1 = Node::start_with(world.p_cut_off(0), Arc::clone(&n0)).await;
    let b_device = fresh_device(&world.p);
    let mut allow = world.allow();
    allow.push(fresh_config(&b_device, &[], &allow).domain);
    let b = Node::start_with(fresh_config(&b_device, &[], &allow), n0).await;
    let invitation = p1.federation().person().invite(false).await.unwrap();
    let code = invitation.code.expect("published on n0's relay");
    let summary = b
        .federation()
        .person()
        .join(&code, "over n0", Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(summary.person, world.p.domain());
    for node in [p1, b] {
        node.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_person_recovered_on_a_fresh_device_pulls_its_waves_back() {
    let world = World::new(true);
    let run = Running::start(&world).await;
    let wave = run.shared_wave().await;
    run.q1.append(&run.bob1, &wave, "[bob was here]").await;
    converged(&run.seats(), &wave, &["[start]", "[bob was here]"]).await;
    let Running {
        p1,
        p2,
        q1,
        q2,
        bob1,
        bob2,
        ..
    } = run;
    // P loses both devices.
    p1.stop().await;
    p2.stop().await;

    // A fresh device of its own, then recovered from the phrase with Q's
    // first device to ask.
    let spare = &world.p_devices[2];
    spare.make_admin(&Person::new().phrase);
    let mut allow = world.allow();
    allow.push(fresh_config(spare, &[], &allow).domain);
    let mut config = fresh_config(spare, &[], &allow);
    assert_ne!(config.domain, world.p.domain());
    let q1_peer = format!(
        "{}@127.0.0.1:{}",
        world.q_devices[0].z32(),
        world.q_devices[0].port
    );
    resonator_federation::control::prepare_recovery(&config, &world.p.phrase, true, vec![q1_peer])
        .unwrap();
    FederationSetup::configure_person(&mut config).unwrap();
    assert_eq!(config.domain, world.p.domain());
    let p3 = Node::start(config).await;
    assert_eq!(p3.domain, world.p.domain());

    // The shared wave comes back from Q, whole.
    let version = q1.version(&wave).await;
    wait_until("the wave is pulled back", || async {
        p3.server().current_version(&wave.name).await.ok() == Some(version.clone())
    })
    .await;
    assert_eq!(p3.history(&wave).await, q1.history(&wave).await);
    wait_until("the recovery finishes", || async {
        !spare.dir.path().join("recovery.json").exists()
    })
    .await;
    // The old devices are revoked everywhere it reached, and the new one
    // takes the lease and hosts.
    let old = [
        world.p_devices[0].key.public(),
        world.p_devices[1].key.public(),
    ];
    wait_until("Q learns the revocation", || async {
        old.iter()
            .all(|key| q1.federation().trust().is_revoked(key))
    })
    .await;
    wait_until("the recovered device holds the lease", || async {
        p3.lease().held_here
    })
    .await;
    let p3_key = p3.federation().endpoint().id();
    wait_until("Q knows the recovered device hosts the wave", || async {
        [&q1, &q2].iter().all(|node| {
            node.server()
                .epochs(&wave.name)
                .last()
                .is_some_and(|record| record.holder == p3_key.as_bytes().to_vec())
        })
    })
    .await;
    let alice3 = p3.user("alice").await;
    q1.append(&bob1, &wave, "[after recovery]").await;
    converged(
        &[(&p3, alice3.as_str()), (&q1, &bob1), (&q2, &bob2)],
        &wave,
        &["[start]", "[bob was here]", "[after recovery]"],
    )
    .await;
    // An old device that comes back is refused.
    assert_ne!(
        q1.federation()
            .trust()
            .check(&old[1], &world.p.domain(), &[])
            .await
            .unwrap(),
        Authorisation::Authorised
    );
    // The old device starts again: Q refuses it and the recovered device
    // keeps the lease.
    let p2 = Node::start(world.p_config(1)).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(q1.federation().trust().is_revoked(&old[1]));
    assert!(p3.lease().held_here);
    for node in [p2, p3, q1, q2] {
        node.stop().await;
    }
}

impl Node {
    /// `GET /waves/host-status` for `cookie`'s user.
    async fn host_status(&self, cookie: &str, wave: &Wave) -> Value {
        let response = self
            .http
            .get(self.url("/waves/host-status"))
            .query(&[("wave", wave.wave.as_str())])
            .header("Cookie", cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response.json().await.unwrap()
    }

    /// `POST /waves/continue`: the status and the body.
    async fn continue_wave(&self, cookie: &str, wave: &Wave) -> (u16, Value) {
        let response = self
            .http
            .post(self.url("/waves/continue"))
            .header("Cookie", cookie)
            .header("Origin", format!("http://{}", self.addr))
            .json(&json!({"wave": wave.wave}))
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    /// The participants of a wave's root wavelet, sorted.
    async fn participants(&self, wave: &Wave) -> Vec<String> {
        let mut list: Vec<String> = self
            .server()
            .with_wavelet(&wave.name, |data| {
                data.participants()
                    .iter()
                    .map(ToString::to_string)
                    .collect()
            })
            .await
            .unwrap()
            .unwrap();
        list.sort();
        list
    }

    /// The `continued-from` document of a wave's root wavelet, as XML.
    async fn continued_from(&self, wave: &Wave) -> Option<String> {
        self.server()
            .with_wavelet(&wave.name, |data| {
                data.documents()
                    .find(|document| {
                        document.id() == resonator_server::waveserver::fork::CONTINUED_FROM_DOC
                    })
                    .map(|document| document.content().to_xml_string())
            })
            .await
            .ok()
            .flatten()
            .flatten()
    }
}

/// The root conversation of wave `id` (as `/waves/continue` names it),
/// with the root blip of `like`.
fn wave_of(id: &str, like: &Wave) -> Wave {
    let wave_id = DualIdSerialiser::MODERN.deserialise_wave_id(id).unwrap();
    let wavelet = format!("{}/conv+root", wave_id.domain());
    Wave {
        name: WaveletName::of(
            wave_id,
            DualIdSerialiser::MODERN
                .deserialise_wavelet_id(&wavelet)
                .unwrap(),
        ),
        wave: id.to_owned(),
        wavelet,
        root: like.root.clone(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_frozen_wave_continues_as_a_new_wave() {
    let world = World::new(false);
    let run = Running::start(&world).await;
    let wave = run.shared_wave().await;
    run.q1.append(&run.bob1, &wave, "[bob]").await;
    let text = converged(&run.seats(), &wave, &["[start]", "[bob]"]).await;
    let bob_cookie = run.q1.cookie("bob").await;
    // P's devices are up: not frozen, and not to be continued.
    let status = run.q1.host_status(&bob_cookie, &wave).await;
    assert_eq!(status["frozen"], false, "{status}");
    assert_eq!(run.q1.continue_wave(&bob_cookie, &wave).await.0, 409);
    let Running {
        p1,
        p2,
        q1,
        q2,
        bob1,
        bob2,
        ..
    } = run;
    p1.stop().await;
    p2.stop().await;
    // The lease runs out and nobody of P answers: frozen.
    wait_until("the host is frozen", || async {
        q1.host_status(&bob_cookie, &wave).await["frozen"] == true
    })
    .await;
    let (status, body) = q1.continue_wave(&bob_cookie, &wave).await;
    assert_eq!(status, 200, "{body}");
    let fork = wave_of(body["wave"].as_str().unwrap(), &wave);
    assert_eq!(fork.name.wavelet_id.domain(), q1.domain);
    // Same text, same participants, and a link back.
    assert_eq!(q1.text(&bob1, &fork).await.as_deref(), Some(text.as_str()));
    assert_eq!(q1.participants(&fork).await, q1.participants(&wave).await);
    let link = q1
        .continued_from(&fork)
        .await
        .expect("a continued-from link");
    assert!(link.contains(&wave.name.wave_id.to_string()), "{link}");
    assert!(
        link.contains(&format!(
            "version=\"{}\"",
            q1.version(&wave).await.version()
        )),
        "{link}"
    );
    // The new wave is local: bob writes on, and it reaches his other device.
    let status = q1.host_status(&bob_cookie, &fork).await;
    assert_eq!(status["local"], true, "{status}");
    q1.append(&bob1, &fork, "[continued]").await;
    let seats: Vec<Seat<'_>> = vec![(&q1, &bob1), (&q2, &bob2)];
    converged(&seats, &fork, &["[start]", "[bob]", "[continued]"]).await;
    // A local wave is not continued over the API; a fork of a fork links
    // to the fork, not to the first wave.
    assert_eq!(q1.continue_wave(&bob_cookie, &fork).await.0, 409);
    let bob = resonator_model::id::ParticipantId::of(&q1.address("bob")).unwrap();
    let nested = q1
        .server()
        .fork_wave(&fork.name.wave_id, &bob)
        .await
        .unwrap();
    let nested = wave_of(&nested.serialise().unwrap(), &wave);
    let link = q1.continued_from(&nested).await.unwrap();
    assert!(link.contains(&fork.name.wave_id.to_string()), "{link}");
    assert!(!link.contains(&wave.name.wave_id.to_string()), "{link}");
    for node in [q1, q2] {
        node.stop().await;
    }
}

/// P grants Q's domain co-hosting: Q holds back while P's lease runs;
/// once P's devices are gone and the lease ran out, Q's online device
/// claims it and hosts P's wave, and edits flow on. A withdrawn grant
/// lets nobody claim.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_co_host_takes_over_only_after_the_lease_ran_out() {
    for withdrawn in [false, true] {
        let world = World::new(false);
        let run = Running::start(&world).await;
        let wave = run.shared_wave().await;
        let p_person = world.p.key.public();
        let q_domain = world.q.domain();
        let until = if withdrawn {
            0
        } else {
            i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis(),
            )
            .unwrap()
                + 3_600_000
        };
        run.p1
            .federation()
            .person()
            .co_host(&q_domain, until)
            .await
            .unwrap();
        wait_until("Q learns the grant", || async {
            run.q1
                .federation()
                .trust()
                .co_hosts()
                .grant(&p_person, &q_domain)
                .is_some()
        })
        .await;
        // P's lease is renewed: Q never claims it.
        tokio::time::sleep(Duration::from_secs(3)).await;
        for node in [&run.q1, &run.q2] {
            let lease = node.federation().lease_of(&p_person).unwrap();
            assert!(lease.co_host_domain.is_none(), "claimed while P is up");
        }
        assert!(run.p1.lease().held_here);
        let Running {
            p1,
            p2,
            q1,
            q2,
            bob1,
            bob2,
            ..
        } = run;
        p1.stop().await;
        p2.stop().await;
        let claimed = || async {
            [&q1, &q2].iter().any(|node| {
                node.federation()
                    .lease_of(&p_person)
                    .is_some_and(|lease| lease.co_host_domain.as_deref() == Some(q_domain.as_str()))
            })
        };
        if withdrawn {
            tokio::time::sleep(Duration::from_secs(5)).await;
            assert!(!claimed().await, "a withdrawn grant claimed the lease");
        } else {
            wait_until("Q claims P's lease", claimed).await;
            wait_until("Q hosts P's wave", || async {
                q1.server().is_local_wavelet(&wave.name) || q2.server().is_local_wavelet(&wave.name)
            })
            .await;
            q1.append(&bob1, &wave, "[co-hosted]").await;
            let seats: Vec<Seat<'_>> = vec![(&q1, &bob1), (&q2, &bob2)];
            converged(&seats, &wave, &["[start]", "[co-hosted]"]).await;
        }
        for node in [q1, q2] {
            node.stop().await;
        }
    }
}

/// A name and avatar set on one device of a person reach the other, and
/// the later change wins.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn names_and_avatars_follow_a_person_to_every_device() {
    let world = World::new(false);
    let (p1, p2, cookie1, cookie2, _wave) = two_devices_of_p(&world, |_| {}).await;
    let me = |node: &Node, cookie: &str| {
        let request = node
            .http
            .get(format!("http://{}/profile/me", node.addr))
            .header("Cookie", cookie.to_owned());
        async move {
            request
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        }
    };
    let set_name = |node: &Node, cookie: &str, name: &str| {
        let request = node
            .http
            .post(format!("http://{}/profile/me", node.addr))
            .header("Cookie", cookie.to_owned())
            .json(&serde_json::json!({ "name": name }));
        async move { assert_eq!(request.send().await.unwrap().status(), 200) }
    };
    set_name(&p2, &cookie2, "Alice on the tablet").await;
    let avatar = p2
        .http
        .put(format!("http://{}/profile/me/avatar", p2.addr))
        .header("Cookie", cookie2.clone())
        .body(png(64, 64))
        .send()
        .await
        .unwrap();
    assert_eq!(avatar.status(), 200);
    wait_until("the name and avatar on p1", || async {
        let seen = me(&p1, &cookie1).await;
        seen["name"] == "Alice on the tablet" && seen["hasAvatar"] == true
    })
    .await;
    set_name(&p1, &cookie1, "Alice").await;
    wait_until("the newer name on p2", || async {
        me(&p2, &cookie2).await["name"] == "Alice"
    })
    .await;
    assert_eq!(me(&p2, &cookie2).await["hasAvatar"], true);
    p2.stop().await;
    p1.stop().await;
}
