//! The device directory of a person (`resonator_federation::directory`),
//! in one process (as `persons.rs`): a server that has never heard from
//! any device of a person reaches the person's devices through the
//! record an admin device publishes under the person key; a revoked
//! device a stale record still lists is not trusted; and a server whose
//! domain is its own key is reached as before.
//!
//! No internet: every node of a test shares one in-memory pkarr relay,
//! and the addresses iroh's discovery would find are given to the
//! endpoints directly.

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use resonator_agents::Integrations;
use resonator_federation::directory::DeviceDirectory;
use resonator_federation::dns::StaticTxtResolver;
use resonator_federation::pair_code::{MemoryRendezvous, Rendezvous};
use resonator_federation::person::{RecoveryPhrase, cert_to_hex};
use resonator_federation::{
    Authorisation, EndpointAddr, Federation, FederationSetup, FileKeyStore, KeyStore, PersonKey,
    PublicKey, ServerKey, Tuning,
};
use resonator_model::id::{DualIdSerialiser, IdSerialiser, WaveletName};
use resonator_server::app::App;
use resonator_server::config::{Config, FederationMode, FederationPeer};
use resonator_server::storage::OutboxStatus;
use resonator_server::waveserver::WaveServerOptions;
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

/// A node's directory, key, federation port and domain (a person's, or
/// for a server its own key domain).
struct Site {
    dir: TempDir,
    key: ServerKey,
    port: u16,
    domain: String,
}

impl Site {
    fn new(person: Option<&Person>) -> Site {
        let dir = tempfile::tempdir().unwrap();
        let key = ServerKey::load_or_create(&dir.path().join("federation.key")).unwrap();
        let domain = person.map_or_else(|| key.key_domain(), Person::domain);
        Site {
            dir,
            key,
            port: free_udp_port(),
            domain,
        }
    }

    fn public(&self) -> PublicKey {
        self.key.public()
    }

    fn addr(&self) -> SocketAddr {
        format!("127.0.0.1:{}", self.port).parse().unwrap()
    }

    fn peer(&self) -> FederationPeer {
        FederationPeer {
            domain: self.domain.clone(),
            key: self.key.key_domain(),
            addr: self.addr().to_string(),
        }
    }

    fn base_config(&self, peers: &[&Site]) -> Config {
        let mut config = Config {
            domain: self.domain.clone(),
            listen: vec!["127.0.0.1:0".to_owned()],
            database_path: self.dir.path().join("resonator.db"),
            register_max_per_hour: 0,
            snapshot_interval: 2,
            ..Config::default()
        };
        config.federation.enabled = true;
        config.federation.mode = FederationMode::Open;
        config.federation.relay = "off".to_owned();
        config.federation.discovery = "off".to_owned();
        config.federation.listen = self.addr().to_string();
        config.federation.request_timeout_secs = 2;
        config.federation.peers = peers.iter().map(|p| p.peer()).collect();
        config
    }

    /// A server: its domain is its key domain; no person.
    fn server_config(&self) -> Config {
        self.base_config(&[])
    }

    /// A device of `person`: `admin` holds the person key, otherwise it
    /// has a certificate; `peers` are the person's other devices.
    fn device_config(&self, person: &Person, admin: bool, peers: &[&Site]) -> Config {
        let mut config = self.base_config(peers);
        config.person.enabled = true;
        config.person.admin = admin;
        config.person.device_name = format!("device {}", &self.key.key_domain()[..6]);
        if admin {
            FileKeyStore::new(self.dir.path().join("person.key"))
                .store(&person.phrase)
                .unwrap();
        } else {
            config.person.cert =
                cert_to_hex(&person.key.issue_cert(&self.public(), "phone", false, now()));
        }
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
    async fn start(config: Config, rendezvous: &Arc<MemoryRendezvous>) -> Node {
        let mut setup = FederationSetup::load(&config)
            .expect("federation setup")
            .expect("federation enabled")
            .with_tuning(tuning())
            .with_resolver(Arc::new(StaticTxtResolver::new()))
            .with_rendezvous(Arc::clone(rendezvous) as Arc<dyn Rendezvous>);
        let mut options = WaveServerOptions::from_config(&config);
        options.delta_signer = Some(setup.delta_signer());
        let mut prepared = App::prepare_with(&config, options).await.expect("prepare");
        setup.subscribe(prepared.server());
        setup.serve_app(prepared.state());
        let (integrations, routes) = Integrations::mount(&mut prepared).await.expect("mount");
        let app = prepared.serve(routes).await.expect("serve");
        let federation = setup.start(app.server()).await.expect("federation");
        Node {
            domain: config.domain.clone(),
            addr: app.local_addrs()[0],
            app: Some(app),
            integrations: Some(integrations),
            federation: Some(federation),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
        }
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

    /// Lets this node find `site` by key, as iroh's discovery would.
    fn knows(&self, site: &Site) {
        self.federation()
            .endpoint()
            .add_peer_addr(EndpointAddr::new(site.public()).with_ip_addr(site.addr()));
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn address(&self, name: &str) -> String {
        format!("{name}@{}", self.domain)
    }

    /// A token of `name` (registered here).
    async fn user(&self, name: &str) -> String {
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
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
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

    /// Creates a wave of `participants` (as `token`'s user); its root
    /// wavelet's name.
    async fn create_wave(&self, token: &str, participants: &[&str]) -> WaveletName {
        let response = self
            .http
            .post(self.url("/robot/dataapi"))
            .bearer_auth(token)
            .json(&json!([{
                "id": "create",
                "method": "robot.createWavelet",
                "params": {"waveletData": {
                    "waveId": format!("{}/TBD_wave", self.domain),
                    "waveletId": format!("{}/conv+root", self.domain),
                    "rootBlipId": "TBD_blip",
                    "participants": participants,
                }}
            }]))
            .send()
            .await
            .unwrap();
        let body: Value = response.json().await.unwrap();
        assert!(body[0].get("error").is_none(), "{body}");
        let data = &body[0]["data"];
        WaveletName::of(
            DualIdSerialiser::MODERN
                .deserialise_wave_id(data["waveId"].as_str().unwrap())
                .unwrap(),
            DualIdSerialiser::MODERN
                .deserialise_wavelet_id(data["waveletId"].as_str().unwrap())
                .unwrap(),
        )
    }

    async fn version(&self, name: &WaveletName) -> u64 {
        self.server().current_version(name).await.unwrap().version()
    }

    /// Whether this node has `name` at the version `host` has it.
    async fn caught_up(&self, name: &WaveletName, host: &Node) -> bool {
        let version = self.version(name).await;
        version > 0 && version == host.version(name).await
    }

    /// The outbox rows of `domain` (and its devices) for `name`, as
    /// `(target, status)`.
    async fn outbox(&self, domain: &str, name: &WaveletName) -> Vec<(String, OutboxStatus)> {
        let uri = resonator_model::id::IdUriCodec::wavelet_name_to_uri(name);
        self.federation()
            .outbox()
            .await
            .unwrap()
            .into_iter()
            .filter(|row| row.wavelet == uri && row.domain.starts_with(domain))
            .map(|row| (row.domain, row.status))
            .collect()
    }
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

/// The device directory published for `person`, if any.
async fn published(rendezvous: &MemoryRendezvous, person: &Person) -> Option<DeviceDirectory> {
    rendezvous
        .resolve(&person.key.public())
        .await
        .unwrap()
        .as_ref()
        .and_then(DeviceDirectory::from_packet)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_that_never_heard_of_a_person_reaches_its_devices() {
    let rendezvous = Arc::new(MemoryRendezvous::new());
    let p = Person::new();
    let (admin, phone, server) = (Site::new(Some(&p)), Site::new(Some(&p)), Site::new(None));
    let a = Node::start(admin.device_config(&p, true, &[&phone]), &rendezvous).await;
    let b = Node::start(phone.device_config(&p, false, &[&admin]), &rendezvous).await;
    // The admin device lists both devices once it has met the phone.
    wait_until("the directory lists both devices", || async {
        published(&rendezvous, &p).await.is_some_and(|record| {
            record.devices.len() == 2
                && record.devices.contains(&admin.public())
                && record.devices.contains(&phone.public())
        })
    })
    .await;

    // A server with no peers and no certificates of P.
    let m = Node::start(server.server_config(), &rendezvous).await;
    assert!(m.federation().trust().devices_of(&p.domain()).is_empty());
    for (node, site) in [(&m, &admin), (&m, &phone), (&a, &server), (&b, &server)] {
        node.knows(site);
    }
    let mia = m.user("mia").await;
    let wave = m
        .create_wave(&mia, &[&m.address("mia"), &format!("x@{}", p.domain())])
        .await;
    for (device, what) in [(&a, "the admin device"), (&b, "the phone")] {
        wait_until(&format!("{what} gets the wave"), || async {
            device.caught_up(&wave, &m).await
        })
        .await;
    }
    // The person domain's row went to one row per device, and the
    // devices' certificates are known now: P is reached directly next.
    let device_row = |site: &Site| {
        (
            format!("{}/{}", p.domain(), site.key.key_domain()),
            OutboxStatus::Delivered,
        )
    };
    wait_until("both device rows are delivered", || async {
        let rows = m.outbox(&p.domain(), &wave).await;
        rows.contains(&device_row(&admin)) && rows.contains(&device_row(&phone))
    })
    .await;
    let rows = m.outbox(&p.domain(), &wave).await;
    assert!(
        rows.iter()
            .all(|(target, status)| target != &p.domain() || *status == OutboxStatus::Expired),
        "{rows:?}"
    );
    let mut known = m.federation().trust().devices_of(&p.domain());
    known.sort_by_key(|key| *key.as_bytes());
    let mut expected = vec![admin.public(), phone.public()];
    expected.sort_by_key(|key| *key.as_bytes());
    assert_eq!(known, expected);
    // P's domain resolves to a device, not to the person key.
    let resolved = m
        .federation()
        .trust()
        .resolve(&p.domain())
        .await
        .unwrap()
        .unwrap();
    assert!(expected.contains(&resolved));
    for node in [m, a, b] {
        node.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_device_listed_by_a_stale_record_is_not_trusted() {
    let rendezvous = Arc::new(MemoryRendezvous::new());
    let p = Person::new();
    let (admin, lost, server) = (Site::new(Some(&p)), Site::new(Some(&p)), Site::new(None));
    // The lost device is up but cut off: it never hears it was revoked.
    let a = Node::start(admin.device_config(&p, true, &[]), &rendezvous).await;
    let r = Node::start(lost.device_config(&p, false, &[]), &rendezvous).await;
    let list = p.key.revoke(None, [lost.public()], now());
    assert!(a.federation().publish_revocations(&list).await.unwrap());
    wait_until(
        "the admin device publishes after the revocation",
        || async {
            published(&rendezvous, &p)
                .await
                .is_some_and(|record| record.revocation_seq == 1)
        },
    )
    .await;
    assert!(!r.federation().trust().is_revoked(&lost.public()));
    // A stale record (newer on the relay all the same) lists the lost
    // device.
    tokio::time::sleep(Duration::from_millis(5)).await;
    let stale = DeviceDirectory {
        devices: vec![lost.public(), admin.public()],
        revocation_seq: 0,
    };
    let (packet, _) = stale.to_packet(&p.key).unwrap();
    rendezvous.publish(&packet).await.unwrap();
    assert_eq!(published(&rendezvous, &p).await, Some(stale));

    let m = Node::start(server.server_config(), &rendezvous).await;
    for (node, site) in [(&m, &admin), (&m, &lost), (&a, &server), (&r, &server)] {
        node.knows(site);
    }
    let mia = m.user("mia").await;
    let wave = m
        .create_wave(&mia, &[&m.address("mia"), &format!("x@{}", p.domain())])
        .await;
    wait_until("the admin device gets the wave", || async {
        a.caught_up(&wave, &m).await
    })
    .await;
    // The server greeted both, learnt the revocation from the admin
    // device and gave the lost one no row.
    let trust = m.federation().trust();
    assert!(trust.is_revoked(&lost.public()));
    assert_eq!(
        trust.check(&lost.public(), &p.domain(), &[]).await.unwrap(),
        Authorisation::Revoked
    );
    assert_eq!(trust.devices_of(&p.domain()), vec![admin.public()]);
    let rows = m.outbox(&p.domain(), &wave).await;
    assert!(
        !rows
            .iter()
            .any(|(target, _)| target.ends_with(&lost.key.key_domain())),
        "{rows:?}"
    );
    // Later updates do not reach it either.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(r.version(&wave).await, 0);
    for node in [m, a, r] {
        node.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_whose_domain_is_its_key_is_reached_as_before() {
    let rendezvous = Arc::new(MemoryRendezvous::new());
    let (first, second) = (Site::new(None), Site::new(None));
    let m = Node::start(first.server_config(), &rendezvous).await;
    let n = Node::start(second.server_config(), &rendezvous).await;
    m.knows(&second);
    n.knows(&first);
    let mia = m.user("mia").await;
    let wave = m
        .create_wave(&mia, &[&m.address("mia"), &n.address("nia")])
        .await;
    wait_until("the other server gets the wave", || async {
        n.caught_up(&wave, &m).await
    })
    .await;
    // No record: one row for the domain itself, delivered to its key.
    wait_until("the row is delivered", || async {
        m.outbox(&n.domain, &wave).await == vec![(n.domain.clone(), OutboxStatus::Delivered)]
    })
    .await;
    // Servers publish no directory.
    assert!(rendezvous.is_empty());
    assert_eq!(
        m.federation().trust().resolve(&n.domain).await.unwrap(),
        Some(second.public())
    );
    for node in [m, n] {
        node.stop().await;
    }
}
