//! Waking a sleeping device through the push relay, in one process: the
//! relay (`resonator-push`) with a fake APNs, a device D of person P with
//! a push token, and the host H of person Q. D seals its token and tells
//! H; H, with an update for D's wave while D is asleep, asks the relay to
//! wake D, once (rate limits hold), and never sees the token. A relay
//! answering `gone` makes H forget D's target, and without `[push] relay`
//! nothing of this happens.

use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use resonator_agents::Integrations;
use resonator_federation::dns::StaticTxtResolver;
use resonator_federation::person::cert_to_hex;
use resonator_federation::push::RawPushToken;
use resonator_federation::{
    Federation, FederationSetup, FileKeyStore, KeyStore, PersonKey, ServerKey, Tuning,
};
use resonator_model::id::{DualIdSerialiser, IdSerialiser};
use resonator_push::testing::{FakePushService, RunningRelay, relay_config};
use resonator_server::app::App;
use resonator_server::config::{Config, FederationMode, FederationPeer};
use resonator_server::waveserver::WaveServerOptions;
use serde_json::{Value, json};
use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(30);
/// The test's APNs token (32 bytes, hex).
const TOKEN: &str = "5ea1ed70c0ffee0ddba11fee1dead5ea1ed70c0ffee0ddba11fee1deadbeef01";

/// Everything logged by this test binary, to look for the token in.
fn logs() -> Arc<Mutex<Vec<u8>>> {
    static LOGS: std::sync::LazyLock<Arc<Mutex<Vec<u8>>>> = std::sync::LazyLock::new(|| {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::clone(&logs);
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(
                "info,resonator_federation=trace,resonator_server=debug,resonator_node=debug,\
                 resonator_push=debug",
            ))
            .with_ansi(false)
            .with_writer(move || LogWriter(Arc::clone(&writer)))
            .try_init();
        logs
    });
    Arc::clone(&LOGS)
}

struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn logged() -> String {
    String::from_utf8_lossy(&logs().lock().unwrap()).into_owned()
}

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
        wake_retry: Duration::from_millis(200),
        wake_window: Duration::from_secs(5),
        push_advertise: Duration::from_millis(200),
        ..Tuning::default()
    }
}

/// A device of a person: the first holds the person key, others are
/// certified by it.
struct Device {
    dir: TempDir,
    key: ServerKey,
    person: Arc<PersonKey>,
    /// Certified (not the admin device).
    cert: Option<String>,
    port: u16,
}

impl Device {
    /// The admin device of a new person.
    fn new() -> Device {
        let (person, phrase) = PersonKey::generate();
        let dir = tempfile::tempdir().unwrap();
        let key = ServerKey::load_or_create(&dir.path().join("federation.key")).unwrap();
        FileKeyStore::new(dir.path().join("person.key"))
            .store(&phrase)
            .unwrap();
        Device {
            dir,
            key,
            person: Arc::new(person),
            cert: None,
            port: free_udp_port(),
        }
    }

    /// Another device of `admin`'s person.
    fn another(admin: &Device) -> Device {
        let dir = tempfile::tempdir().unwrap();
        let key = ServerKey::load_or_create(&dir.path().join("federation.key")).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let cert = cert_to_hex(&admin.person.issue_cert(&key.public(), "phone", false, now));
        Device {
            dir,
            key,
            person: Arc::clone(&admin.person),
            cert: Some(cert),
            port: free_udp_port(),
        }
    }

    fn domain(&self) -> String {
        self.person.domain()
    }

    fn peer(&self) -> FederationPeer {
        FederationPeer {
            domain: self.domain(),
            key: self.key.key_domain(),
            addr: format!("127.0.0.1:{}", self.port),
        }
    }

    fn config(&self, other: &Device, relay: Option<&str>) -> Config {
        let mut config = Config {
            listen: vec!["127.0.0.1:0".to_owned()],
            database_path: self.dir.path().join("resonator.db"),
            register_max_per_hour: 0,
            ..Config::default()
        };
        config.federation.enabled = true;
        config.federation.mode = FederationMode::Allowlist;
        config.federation.allow = vec![self.domain(), other.domain()];
        config.federation.allow.dedup();
        config.federation.relay = "off".to_owned();
        config.federation.discovery = "off".to_owned();
        config.federation.listen = format!("127.0.0.1:{}", self.port);
        config.federation.request_timeout_secs = 2;
        config.federation.peers = vec![other.peer()];
        config.person.enabled = true;
        config.person.admin = self.cert.is_none();
        config.person.anchor = self.cert.is_none();
        if let Some(cert) = &self.cert {
            config.person.cert = cert.clone();
        }
        config.person.device_name = "device".to_owned();
        if let Some(relay) = relay {
            config.push.relay = relay.to_owned();
        }
        FederationSetup::configure_person(&mut config).unwrap();
        config
    }
}

struct Node {
    domain: String,
    database: std::path::PathBuf,
    app: Option<App>,
    integrations: Option<Integrations>,
    federation: Option<Federation>,
    addr: SocketAddr,
    http: reqwest::Client,
}

impl Node {
    async fn start(config: Config) -> Node {
        let setup = FederationSetup::load(&config)
            .expect("federation setup")
            .expect("federation enabled")
            .with_tuning(tuning())
            .with_resolver(Arc::new(StaticTxtResolver::new()));
        let mut setup = setup;
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
            database: config.database_path.clone(),
            addr: app.local_addrs()[0],
            app: Some(app),
            integrations: Some(integrations),
            federation: Some(federation),
            http: reqwest::Client::new(),
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

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn address(&self, name: &str) -> String {
        format!("{name}@{}", self.domain)
    }

    /// A token of `name` (registered here).
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

    /// A session cookie of `name` (registered here).
    async fn cookie(&self, name: &str) -> String {
        let password = format!("{name}-password-long");
        self.http
            .post(self.url("/auth/register"))
            .json(&json!({"address": name, "password": password}))
            .send()
            .await
            .unwrap();
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

    async fn run(&self, token: &str, operations: Value) -> Value {
        let response = self
            .http
            .post(self.url("/robot/dataapi"))
            .bearer_auth(token)
            .json(&operations)
            .send()
            .await
            .unwrap();
        let body: Value = response.json().await.unwrap();
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
        DualIdSerialiser::MODERN.deserialise_wave_id(&wave).unwrap();
        Wave {
            wave,
            wavelet,
            root: data["blipId"].as_str().unwrap().to_owned(),
        }
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

    async fn text(&self, token: &str, wave: &Wave) -> String {
        let response = self
            .http
            .post(self.url("/robot/dataapi"))
            .bearer_auth(token)
            .json(&json!([{"id": "f", "method": "robot.fetchWave",
                    "params": {"waveId": wave.wave, "waveletId": wave.wavelet}}]))
            .send()
            .await
            .unwrap();
        let body: Value = response.json().await.unwrap_or_default();
        body[0]["data"]["blips"][&wave.root]["content"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// The push targets this node knows (device z32 keys).
    fn push_targets(&self) -> Vec<String> {
        self.federation().push().map_or_else(Vec::new, |push| {
            push.targets()
                .into_iter()
                .map(|(device, _)| device.to_z32())
                .collect()
        })
    }

    /// The bytes of the database files (with the write-ahead log).
    fn database_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for suffix in ["", "-wal", "-shm"] {
            let mut path = self.database.clone().into_os_string();
            path.push(suffix);
            bytes.extend(std::fs::read(path).unwrap_or_default());
        }
        bytes
    }
}

struct Wave {
    wave: String,
    wavelet: String,
    root: String,
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

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn token() -> RawPushToken {
    RawPushToken {
        platform: "apns".to_owned(),
        token: TOKEN.to_owned(),
    }
}

/// D (alice, with a push token) and H (bob, hosting a wave with alice);
/// returns them once H knows how to wake D.
struct Setup {
    _relay_dir: TempDir,
    fake: FakePushService,
    relay: RunningRelay,
    d_device: Device,
    h_device: Device,
}

impl Setup {
    async fn new() -> Setup {
        logs();
        let relay_dir = tempfile::tempdir().unwrap();
        let fake = FakePushService::start().await;
        let (config, _) = relay_config(relay_dir.path(), &fake.url());
        let relay = RunningRelay::start(config).await;
        Setup {
            _relay_dir: relay_dir,
            fake,
            relay,
            d_device: Device::new(),
            h_device: Device::new(),
        }
    }

    /// Starts D and H, shares a wave of bob's with alice and waits until
    /// H knows D's target; returns them with bob's token and the wave.
    async fn meet(&self) -> (Node, Node, String, Wave) {
        let relay = self.relay.url();
        let d = Node::start(self.d_device.config(&self.h_device, Some(&relay))).await;
        let h = Node::start(self.h_device.config(&self.d_device, Some(&relay))).await;
        d.federation().set_push_token(Some(token()));
        let d_key = d.federation().endpoint().id().to_z32();
        wait_until("D seals its token", || async {
            d.push_targets().contains(&d_key)
        })
        .await;
        let alice = d.user("alice").await;
        let bob = h.user("bob").await;
        let wave = h
            .create_wave(&bob, &[&h.address("bob"), &d.address("alice")])
            .await;
        h.append(&bob, &wave, "[hello]").await;
        wait_until("D gets the wave", || async {
            d.text(&alice, &wave).await.contains("[hello]")
        })
        .await;
        wait_until("H learns how to wake D", || async {
            h.push_targets().contains(&d_key)
        })
        .await;
        (d, h, bob, wave)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_wakes_a_sleeping_device_once_and_never_sees_its_token() {
    let setup = Setup::new().await;
    let (d, h, bob, wave) = setup.meet().await;
    assert!(setup.fake.pushes().is_empty());
    let sealed_by_d = {
        let push = d.federation().push().unwrap();
        assert_eq!(push.targets()[0].1, setup.relay.url());
        push.wakes_sent()
    };
    assert_eq!(sealed_by_d, 0);
    // D goes to sleep; H has updates for it.
    d.stop().await;
    for n in 0..5 {
        h.append(&bob, &wave, &format!("[while asleep {n}]")).await;
    }
    wait_until("the relay pushes to D", || async {
        !setup.fake.pushes().is_empty()
    })
    .await;
    // H keeps retrying D (soon, after the wake), but wakes it only once.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let pushes = setup.fake.pushes();
    assert_eq!(pushes.len(), 1, "one push");
    assert_eq!(pushes[0].path, format!("/3/device/{TOKEN}"));
    assert_eq!(pushes[0].headers["apns-push-type"], "background");
    assert_eq!(pushes[0].body, r#"{"aps":{"content-available":1}}"#);
    assert_eq!(h.federation().push().unwrap().wakes_sent(), 1);
    // The token is nowhere on H: not in its database, not in the logs.
    let database = h.database_bytes();
    assert!(!database.is_empty());
    let logs = logged();
    assert!(
        logs.contains("sealed this device's push token") && logs.contains("woke a device"),
        "the logs are captured"
    );
    for form in [TOKEN.to_owned(), TOKEN.to_uppercase()] {
        assert!(
            !contains(&database, form.as_bytes()),
            "token in H's database"
        );
        assert!(!logged().contains(&form), "token in the logs");
    }
    let raw: Vec<u8> = (0..TOKEN.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&TOKEN[i..i + 2], 16).unwrap())
        .collect();
    assert!(
        !contains(&database, &raw),
        "raw token bytes in H's database"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_token_the_relay_says_is_gone_is_forgotten() {
    let setup = Setup::new().await;
    let (d, h, bob, wave) = setup.meet().await;
    let d_key = d.federation().endpoint().id().to_z32();
    setup.fake.answer(410, json!({"reason": "Unregistered"}));
    d.stop().await;
    h.append(&bob, &wave, "[for nobody]").await;
    wait_until("H forgets D's target", || async {
        !h.push_targets().contains(&d_key)
    })
    .await;
    assert_eq!(setup.fake.pushes().len(), 1);
    // Gone from H's database too: H restarted knows no target.
    let h_config = setup
        .h_device
        .config(&setup.d_device, Some(&setup.relay.url()));
    h.stop().await;
    let h = Node::start(h_config).await;
    assert!(h.push_targets().is_empty());
    h.append(&bob, &wave, "[still nobody]").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(h.federation().push().unwrap().wakes_sent(), 0);
    assert_eq!(setup.fake.pushes().len(), 1);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_relay_nothing_is_sealed_told_or_woken() {
    logs();
    let d_device = Device::new();
    let h_device = Device::new();
    let d = Node::start(d_device.config(&h_device, None)).await;
    let h = Node::start(h_device.config(&d_device, None)).await;
    assert!(d.federation().push().is_none() && h.federation().push().is_none());
    // Taken and ignored.
    d.federation().set_push_token(Some(token()));
    let alice = d.user("alice").await;
    let bob = h.user("bob").await;
    let wave = h
        .create_wave(&bob, &[&h.address("bob"), &d.address("alice")])
        .await;
    h.append(&bob, &wave, "[hello]").await;
    wait_until("D gets the wave", || async {
        d.text(&alice, &wave).await.contains("[hello]")
    })
    .await;
    d.stop().await;
    h.append(&bob, &wave, "[asleep]").await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!contains(&h.database_bytes(), TOKEN.as_bytes()));
    h.stop().await;
    // Nothing learned or stored on either side.
    for device in [&d_device, &h_device] {
        let database = resonator_server::storage::Database::open(
            device.dir.path().join("resonator.db"),
            Default::default(),
        )
        .unwrap();
        assert!(
            database
                .federation_store()
                .push_targets()
                .await
                .unwrap()
                .is_empty()
        );
    }
}

/// An attachment uploaded on a device that is now asleep (the iPad) is
/// fetched by another device of the person (the iPhone): the fetch wakes
/// the uploader once and gets the attachment when it is back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fetching_an_attachment_wakes_the_device_that_has_it() {
    let setup = Setup::new().await;
    let relay = setup.relay.url();
    let phone_device = &setup.h_device;
    let tablet_device = Device::another(phone_device);
    // The fallback for an attachment not copied ahead: copying off.
    let mut tablet_config = tablet_device.config(phone_device, Some(&relay));
    tablet_config.federation.replicate_own_attachments = false;
    let mut phone_config = phone_device.config(&tablet_device, Some(&relay));
    phone_config.federation.replicate_own_attachments = false;
    let phone = Node::start(phone_config).await;
    let tablet = Node::start(tablet_config.clone()).await;
    tablet.federation().set_push_token(Some(token()));
    let tablet_key = tablet.federation().endpoint().id().to_z32();
    wait_until("the phone learns how to wake the tablet", || async {
        phone.push_targets().contains(&tablet_key)
    })
    .await;
    let alice_phone = phone.user("alice").await;
    let wave = phone
        .create_wave(&alice_phone, &[&phone.address("alice")])
        .await;
    let alice_tablet = tablet.cookie("alice").await;
    let id = format!("{}/photo", phone_device.domain());
    let bytes = b"a photo taken on the tablet".to_vec();
    wait_until("the tablet has the wave", || async {
        tablet.upload(&alice_tablet, &id, &wave.wave, &bytes).await == 201
    })
    .await;
    // The tablet goes to sleep; the phone asks for the photo.
    tablet.stop().await;
    let cookie = phone.cookie("alice").await;
    let fetch = {
        let (id, wave) = (id.clone(), wave.wave.clone());
        let phone = Arc::new(phone);
        let fetching = Arc::clone(&phone);
        (
            phone,
            tokio::spawn(async move { fetching.download(&cookie, &id, &wave).await }),
        )
    };
    let (phone, download) = fetch;
    wait_until("the tablet is woken", || async {
        !setup.fake.pushes().is_empty()
    })
    .await;
    // The push wakes the tablet: it comes back up.
    let tablet = Node::start(tablet_config).await;
    assert_eq!(download.await.unwrap(), (200, bytes));
    assert_eq!(setup.fake.pushes().len(), 1, "one wake");
    tablet.stop().await;
    Arc::into_inner(phone).unwrap().stop().await;
}
