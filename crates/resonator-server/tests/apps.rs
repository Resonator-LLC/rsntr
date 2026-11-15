//! Wave apps on a running server: a gadget with an app runs once its user
//! allows it, keeps its kv across restarts, and stops when the gadget goes
//! away or nobody looks at it.
#![cfg(feature = "apps")]

mod common;
#[path = "common/net.rs"]
mod net;
#[path = "common/waves.rs"]
mod waves;

use std::io::Write;
use std::time::Duration;

use resonator_apps::{UiEvent, UiNode};
use resonator_model::docop::{Attributes, DocInitialization, DocOpBuilder};
use resonator_model::id::{ParticipantId, WaveId};
use resonator_server::apps::{AppManager, AppSignal, GRANT_KEY, OpenError};
use resonator_server::attachment::AttachmentId;

use common::{DOMAIN, participant};
use net::{TIMEOUT, TestServer};
use waves::{create_wave, edit_root, edit_supplement, root_name};

const COUNTER: &[u8] = include_bytes!("../../resonator-apps/tests/fixtures/counter_rs.wasm");
const GADGET: &str = "g1";

fn user(name: &str) -> ParticipantId {
    participant(&format!("{name}@{DOMAIN}"))
}

/// A bundle of the Rust counter.
fn bundle(name: &str) -> (Vec<u8>, String) {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("manifest.json", options).unwrap();
    let manifest = format!(r#"{{"name":"{name}","version":"1","permissions":["wave","kv"]}}"#);
    zip.write_all(manifest.as_bytes()).unwrap();
    zip.start_file("app.wasm", options).unwrap();
    zip.write_all(COUNTER).unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    let id = resonator_apps::Bundle::from_zip(&bytes).unwrap().id();
    (bytes, id)
}

fn gadget(app: &str, bundle: &str) -> DocInitialization {
    let attributes =
        Attributes::from_pairs([("app", app), ("bundle", bundle), ("id", GADGET)]).unwrap();
    DocOpBuilder::new()
        .element_start("body", Attributes::new())
        .element_start("line", Attributes::new())
        .element_end()
        .element_start("gadget", attributes)
        .element_end()
        .element_end()
        .build_initialization()
        .unwrap()
}

/// A server with Alice's wave, which Bob is on, holding the counter's
/// bundle; with `[apps]` on.
async fn setup(idle_secs: u64) -> (TestServer, WaveId, String, String) {
    let server = TestServer::start_with(|config| {
        config.apps.enabled = true;
        config.apps.idle_secs = idle_secs;
    })
    .await;
    let alice = user("alice");
    let wave = create_wave(
        server.app().server(),
        &alice,
        "w+app",
        &[&user("bob")],
        "Apps",
    )
    .await;
    let (bytes, app) = bundle("counter");
    let attachment = format!("{DOMAIN}/counter1");
    server
        .app()
        .state()
        .attachments
        .store(
            &AttachmentId::deserialise(&attachment).unwrap(),
            &root_name(&wave),
            "counter.zip",
            Some(&alice),
            &bytes,
        )
        .await
        .unwrap();
    (server, wave, app, attachment)
}

async fn insert_gadget(server: &TestServer, wave: &WaveId, app: &str, bundle: &str) -> String {
    let mut blip = String::new();
    edit_root(
        server.app().server(),
        &user("alice"),
        wave,
        |conversation| {
            blip = conversation
                .append_blip_with_content("", Some(&gadget(app, bundle)))
                .unwrap();
        },
    )
    .await;
    blip
}

async fn allow(server: &TestServer, who: &ParticipantId, wave: &WaveId, app: &str) {
    edit_supplement(server.app().server(), who, wave, |supplement| {
        supplement
            .set_gadget_state(GADGET, GRANT_KEY, Some(app))
            .unwrap();
    })
    .await;
}

fn apps(server: &TestServer) -> AppManager {
    server.app().state().apps.clone().expect("apps on")
}

fn count(nodes: &[UiNode]) -> &str {
    nodes[1].text.as_deref().unwrap()
}

fn click() -> UiEvent {
    UiEvent {
        node: 2,
        name: "click".into(),
        value: None,
        fields: vec![],
    }
}

async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !done() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn an_allowed_gadget_runs_and_keeps_its_kv() {
    let (server, wave, app, bundle) = setup(1).await;
    insert_gadget(&server, &wave, &app, &bundle).await;
    let apps = apps(&server);
    let alice = user("alice");

    match apps.open(&alice, &wave, GADGET).await {
        Err(OpenError::NeedsGrant {
            app: wanted,
            manifest,
        }) => {
            assert_eq!(wanted, app);
            assert_eq!(manifest.name, "counter");
        }
        other => panic!("expected NeedsGrant, got {other:?}"),
    }
    assert_eq!(apps.running(), 0);

    allow(&server, &alice, &wave, &app).await;
    let session = apps.open(&alice, &wave, GADGET).await.unwrap();
    assert_eq!(session.app_id(), app);
    assert_eq!(count(&session.render(0).await.unwrap()), "0");
    let mut signals = session.signals();
    session.event(0, click()).await.unwrap();
    session.event(0, click()).await.unwrap();
    let signal = tokio::time::timeout(TIMEOUT, signals.recv()).await.unwrap();
    assert_eq!(signal.unwrap(), AppSignal::Invalidate(0));
    assert_eq!(count(&session.render(0).await.unwrap()), "2");

    // A second viewer shares the instance.
    let second = apps.open(&alice, &wave, GADGET).await.unwrap();
    assert_eq!(count(&second.render(0).await.unwrap()), "2");
    assert_eq!(apps.running(), 1);

    // Nobody looking: it stops after idle_secs, and starts again from
    // its kv.
    drop(session);
    drop(second);
    wait_until("idle stop", || apps.running() == 0).await;
    let again = apps.open(&alice, &wave, GADGET).await.unwrap();
    assert_eq!(count(&again.render(0).await.unwrap()), "2");

    // Bob's instance is his own: he has not allowed it yet.
    assert!(matches!(
        apps.open(&user("bob"), &wave, GADGET).await,
        Err(OpenError::NeedsGrant { .. })
    ));
    assert!(matches!(
        apps.open(&user("carol"), &wave, GADGET).await,
        Err(OpenError::NotParticipant)
    ));
    server.shutdown().await;
}

#[tokio::test]
async fn removing_the_gadget_or_the_grant_stops_the_app() {
    let (server, wave, app, bundle) = setup(300).await;
    let blip = insert_gadget(&server, &wave, &app, &bundle).await;
    let apps = apps(&server);
    let alice = user("alice");
    allow(&server, &alice, &wave, &app).await;

    let session = apps.open(&alice, &wave, GADGET).await.unwrap();
    let mut signals = session.signals();
    allow(&server, &alice, &wave, "sha256:other").await;
    let signal = tokio::time::timeout(TIMEOUT, signals.recv()).await.unwrap();
    assert_eq!(signal.unwrap(), AppSignal::Stopped);
    assert!(!session.is_running());
    assert_eq!(apps.running(), 0);

    allow(&server, &alice, &wave, &app).await;
    let session = apps.open(&alice, &wave, GADGET).await.unwrap();
    let mut signals = session.signals();
    edit_root(server.app().server(), &alice, &wave, |conversation| {
        conversation.delete_blip(&blip).unwrap();
    })
    .await;
    let signal = tokio::time::timeout(TIMEOUT, signals.recv()).await.unwrap();
    assert_eq!(signal.unwrap(), AppSignal::Stopped);
    assert_eq!(apps.running(), 0);
    assert!(matches!(
        apps.open(&alice, &wave, GADGET).await,
        Err(OpenError::NotFound)
    ));
    server.shutdown().await;
}

#[tokio::test]
async fn a_bundle_that_does_not_match_is_refused() {
    let (server, wave, _, bundle) = setup(300).await;
    let (_, other) = bundle_id_of_another();
    insert_gadget(&server, &wave, &other, &bundle).await;
    allow(&server, &user("alice"), &wave, &other).await;
    assert!(matches!(
        apps(&server).open(&user("alice"), &wave, GADGET).await,
        Err(OpenError::HashMismatch)
    ));
    assert_eq!(apps(&server).running(), 0);
    server.shutdown().await;
}

fn bundle_id_of_another() -> (Vec<u8>, String) {
    bundle("something else")
}

#[tokio::test]
async fn apps_are_off_unless_enabled() {
    let server = TestServer::start().await;
    assert!(server.app().state().apps.is_none());
    server.shutdown().await;
}
