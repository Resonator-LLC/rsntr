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
use resonator_server::apps::{AppManager, AppSignal, GRANT_KEY, OpenError, app_wavelet};
use resonator_server::attachment::AttachmentId;
use resonator_server::waveserver::WaveletProvider;

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
    bundle_of(name, COUNTER, r#"["wave","kv"]"#)
}

fn bundle_of(name: &str, wasm: &[u8], permissions: &str) -> (Vec<u8>, String) {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("manifest.json", options).unwrap();
    let manifest = format!(r#"{{"name":"{name}","version":"1","permissions":{permissions}}}"#);
    zip.write_all(manifest.as_bytes()).unwrap();
    zip.start_file("app.wasm", options).unwrap();
    zip.write_all(wasm).unwrap();
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
    setup_with(idle_secs, bundle("counter")).await
}

async fn setup_with(
    idle_secs: u64,
    (bytes, app): (Vec<u8>, String),
) -> (TestServer, WaveId, String, String) {
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
async fn posts_go_through_the_app_wavelet_to_every_instance() {
    let (server, wave, app, bundle) = setup(300).await;
    insert_gadget(&server, &wave, &app, &bundle).await;
    let apps = apps(&server);
    let (alice, bob) = (user("alice"), user("bob"));
    allow(&server, &alice, &wave, &app).await;
    allow(&server, &bob, &wave, &app).await;
    let alices = apps.open(&alice, &wave, GADGET).await.unwrap();
    let bobs = apps.open(&bob, &wave, GADGET).await.unwrap();

    // Each click posts ("inc", count); Bob's counter follows the messages.
    for _ in 0..3 {
        alices.event(0, click()).await.unwrap();
    }
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        if count(&bobs.render(0).await.unwrap()) == "3" {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Bob's counter did not follow"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The app wavelet has the conversation's participants and one document
    // per message, by Alice.
    let name = app_wavelet(&wave, GADGET).unwrap();
    let snapshot = |server: &TestServer| {
        let server = server.app().server().clone();
        let name = name.clone();
        async move { server.get_snapshot(&name).await.unwrap().unwrap().snapshot }
    };
    let data = snapshot(&server).await;
    assert_eq!(data.participants(), [alice.clone(), bob.clone()]);
    let messages: Vec<_> = data
        .documents()
        .filter(|d| d.id().starts_with("msg+"))
        .collect();
    assert_eq!(messages.len(), 3);
    assert!(messages.iter().all(|d| *d.author() == alice));

    // Someone joining the conversation joins the app wavelet too.
    let carol = user("carol");
    edit_root(server.app().server(), &alice, &wave, |conversation| {
        conversation.add_participant(&carol).unwrap();
    })
    .await;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !snapshot(&server).await.participants().contains(&carol) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "Carol was not added"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Carol's instance, once she allows it, follows the next post (the
    // counter reads only its kv at start, not the earlier messages).
    allow(&server, &carol, &wave, &app).await;
    let carols = apps.open(&carol, &wave, GADGET).await.unwrap();
    assert_eq!(count(&carols.render(0).await.unwrap()), "0");
    alices.event(0, click()).await.unwrap();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while count(&carols.render(0).await.unwrap()) != "4" {
        assert!(
            tokio::time::Instant::now() < deadline,
            "Carol's counter did not catch up"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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

#[tokio::test]
async fn the_socket_streams_the_view_and_takes_events() {
    use resonator_model::proto::app::{
        AppEventRequest, AppEventResponse, AppOpenRequest, AppUpdate,
    };
    use resonator_model::proto::rpc::CancelRpc;

    let (server, wave, app, bundle) = setup(300).await;
    let cookie = server.user("alice").await;
    insert_gadget(&server, &wave, &app, &bundle).await;
    let mut client = net::WaveClient::with_cookie(&server, &cookie).await;
    let wave_id = wave.serialise().unwrap();
    let open = AppOpenRequest {
        wave_id: Some(wave_id.clone()),
        gadget_id: Some(GADGET.to_owned()),
        view: None,
    };

    // Not allowed yet: the prompt's facts, then the end.
    let seq = client.send(&open).await;
    assert_eq!(
        client
            .next_for(seq)
            .await
            .decode::<AppUpdate>()
            .status
            .unwrap(),
        "loading"
    );
    let prompt: AppUpdate = client.next_for(seq).await.decode();
    assert_eq!(prompt.status.as_deref(), Some("needs-grant"));
    assert_eq!(prompt.app_id.as_deref(), Some(app.as_str()));
    assert_eq!(prompt.name.as_deref(), Some("counter"));
    assert_eq!(prompt.permissions, ["wave", "kv"]);
    assert_eq!(client.next_for(seq).await.failure(), None);

    allow(&server, &user("alice"), &wave, &app).await;
    let seq = client.send(&open).await;
    assert_eq!(
        client
            .next_for(seq)
            .await
            .decode::<AppUpdate>()
            .status
            .unwrap(),
        "loading"
    );
    let first: AppUpdate = client.next_for(seq).await.decode();
    assert_eq!(first.status.as_deref(), Some("running"));
    assert_eq!(first.nodes[1].text.as_deref(), Some("0"));
    assert_eq!(first.nodes[2].on, ["click"]);

    let event = client
        .send(&AppEventRequest {
            wave_id: Some(wave_id.clone()),
            gadget_id: Some(GADGET.to_owned()),
            view: None,
            node: Some(2),
            name: Some("click".to_owned()),
            value: None,
            fields: vec![],
        })
        .await;
    let answer: AppEventResponse = client.next_for(event).await.decode();
    assert_eq!(answer.error_message, None);
    let second: AppUpdate = client.next_for(seq).await.decode();
    assert_eq!(second.nodes[1].text.as_deref(), Some("1"));

    client.send_as(seq, &CancelRpc {}).await;
    assert_eq!(client.next_for(seq).await.failure(), None);
    server.shutdown().await;
}

const PEERS: &[u8] = include_bytes!("../../resonator-apps/tests/fixtures/peers_rs.wasm");

/// The peers guest's view: (number of peers, what reached it).
async fn peers_view(session: &resonator_server::apps::AppSession) -> (String, String) {
    let nodes = session.render(0).await.unwrap();
    let text = |i: usize| nodes[i].text.clone().unwrap_or_default();
    (text(1), text(2))
}

fn press(node: u32) -> UiEvent {
    UiEvent {
        node,
        name: "click".into(),
        value: None,
        fields: vec![],
    }
}

async fn wait_for_view(
    session: &resonator_server::apps::AppSession,
    what: &str,
    done: impl Fn(&(String, String)) -> bool,
) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let view = peers_view(session).await;
        if done(&view) {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "{what}: {view:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn instances_on_one_node_are_peers_and_stream_to_each_other() {
    let (server, wave, app, bundle) =
        setup_with(300, bundle_of("peers", PEERS, r#"["peers","streams"]"#)).await;
    insert_gadget(&server, &wave, &app, &bundle).await;
    let apps = apps(&server);
    let (alice, bob) = (user("alice"), user("bob"));
    allow(&server, &alice, &wave, &app).await;
    allow(&server, &bob, &wave, &app).await;
    let alices = apps.open(&alice, &wave, GADGET).await.unwrap();
    let bobs = apps.open(&bob, &wave, GADGET).await.unwrap();
    wait_for_view(&alices, "Alice sees Bob", |(n, _)| n == "1").await;
    wait_for_view(&bobs, "Bob sees Alice", |(n, _)| n == "1").await;

    alices.event(0, press(3)).await.unwrap();
    wait_for_view(&bobs, "Bob got the ping", |(_, seen)| seen == "ping").await;
    wait_for_view(&alices, "Alice got the pong", |(_, seen)| seen == "pong").await;

    alices.event(0, press(4)).await.unwrap();
    wait_for_view(&bobs, "Bob got both streams", |(_, seen)| {
        ["open:r", "r:hi", "open:d", "d:hi"]
            .iter()
            .all(|w| seen.contains(w))
    })
    .await;
    wait_for_view(&alices, "Alice got the answers", |(_, seen)| {
        ["r:back", "d:back", "closed"]
            .iter()
            .all(|w| seen.contains(w))
    })
    .await;

    // A stopped instance is no peer.
    drop(bobs);
    apps.stop_all();
    let alices = apps.open(&alice, &wave, GADGET).await.unwrap();
    assert_eq!(peers_view(&alices).await.0, "0");
    server.shutdown().await;
}

#[tokio::test]
async fn only_participants_of_the_wave_say_hello() {
    let (server, wave, app, bundle) =
        setup_with(300, bundle_of("peers", PEERS, r#"["peers","streams"]"#)).await;
    insert_gadget(&server, &wave, &app, &bundle).await;
    let apps = apps(&server);
    let alice = user("alice");
    allow(&server, &alice, &wave, &app).await;
    let alices = apps.open(&alice, &wave, GADGET).await.unwrap();
    let remote = |name: &str| resonator_server::apps::RemotePeer {
        device: "devicekey".to_owned(),
        participant: participant(&format!("{name}@elsewhere.example")),
    };
    assert!(
        apps.hello(remote("mallory"), &wave, GADGET, &app)
            .await
            .is_err()
    );
    // Data from a peer that never said hello goes nowhere.
    assert!(
        apps.peer_data(
            &remote("mallory"),
            &wave,
            GADGET,
            &app,
            &alice,
            b"x".to_vec()
        )
        .is_err()
    );

    // A participant on another device, with the same app, is a peer.
    let dana = remote("dana");
    edit_root(server.app().server(), &alice, &wave, |conversation| {
        conversation.add_participant(&dana.participant).unwrap();
    })
    .await;
    assert!(
        apps.hello(dana.clone(), &wave, GADGET, "sha256:other")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        apps.hello(dana.clone(), &wave, GADGET, &app).await.unwrap(),
        std::slice::from_ref(&alice)
    );
    wait_for_view(&alices, "Alice sees Dana", |(n, _)| n == "1").await;
    apps.peer_data(&dana, &wave, GADGET, &app, &alice, b"hi".to_vec())
        .unwrap();
    wait_for_view(&alices, "Alice got Dana's data", |(_, seen)| seen == "hi").await;
    apps.device_gone("devicekey");
    wait_for_view(&alices, "Dana's device is gone", |(n, _)| n == "0").await;
    server.shutdown().await;
}

const WALKIE: &[u8] = include_bytes!("../../resonator-apps/tests/fixtures/walkie.wasm");

#[tokio::test]
async fn the_viewer_is_a_peer_with_a_microphone_and_a_speaker() {
    let (server, wave, app, bundle) = setup_with(
        300,
        bundle_of("walkie", WALKIE, r#"["peers","streams","media"]"#),
    )
    .await;
    insert_gadget(&server, &wave, &app, &bundle).await;
    let apps = apps(&server);
    let alice = user("alice");
    allow(&server, &alice, &wave, &app).await;
    let alices = apps.open(&alice, &wave, GADGET).await.unwrap();
    let mut signals = alices.signals();
    let texts =
        |nodes: Vec<UiNode>| -> Vec<String> { nodes.into_iter().filter_map(|n| n.text).collect() };
    // The viewer is no other person.
    let view = texts(alices.render(0).await.unwrap());
    assert!(
        view.contains(&"Nobody else has it open".to_owned()),
        "{view:?}"
    );

    // Microphone frames reach the app; with Echo on they come back on
    // channel 0.
    alices.media(vec![1, 2, 3], false).unwrap();
    let echo = alices
        .render(0)
        .await
        .unwrap()
        .iter()
        .position(|n| n.text.as_deref() == Some("Echo: off"))
        .unwrap() as u32;
    alices.event(0, press(echo)).await.unwrap();
    alices.media(vec![4, 5, 6], false).unwrap();
    let frame = tokio::time::timeout(TIMEOUT, async {
        loop {
            if let Ok(AppSignal::Media(frame)) = signals.recv().await {
                return frame;
            }
        }
    })
    .await
    .expect("no frame for the speaker");
    assert_eq!(frame, [0, 4, 5, 6]);
    // The walkie redraws now and then, not for every frame.
    let view = texts(alices.render(0).await.unwrap());
    assert!(view.contains(&"sent 2".to_owned()), "{view:?}");
    server.shutdown().await;
}

#[tokio::test]
async fn apps_without_the_media_permission_take_no_frames() {
    let (server, wave, app, bundle) = setup(300).await;
    insert_gadget(&server, &wave, &app, &bundle).await;
    let apps = apps(&server);
    let alice = user("alice");
    allow(&server, &alice, &wave, &app).await;
    let alices = apps.open(&alice, &wave, GADGET).await.unwrap();
    assert!(alices.media(vec![1, 2, 3], false).is_err());
    assert!(alices.media(vec![1, 2, 3], true).is_err());
    server.shutdown().await;
}

const WALKIE_CAM: &[u8] = include_bytes!("../../resonator-apps/tests/fixtures/walkie_cam.wasm");

#[tokio::test]
async fn camera_frames_reach_the_app_and_video_comes_back_to_the_screen() {
    let (server, wave, app, bundle) = setup_with(
        300,
        bundle_of(
            "walkie-cam",
            WALKIE_CAM,
            r#"["peers","streams","media","camera"]"#,
        ),
    )
    .await;
    insert_gadget(&server, &wave, &app, &bundle).await;
    let apps = apps(&server);
    let alice = user("alice");
    allow(&server, &alice, &wave, &app).await;
    let alices = apps.open(&alice, &wave, GADGET).await.unwrap();
    let mut signals = alices.signals();
    let echo = alices
        .render(0)
        .await
        .unwrap()
        .iter()
        .position(|n| n.text.as_deref() == Some("Echo: off"))
        .unwrap() as u32;
    alices.event(0, press(echo)).await.unwrap();
    // A key frame (flags 1) of the camera comes back on channel 0 as
    // video, and a microphone frame as sound.
    alices.media(vec![1, 7, 8, 9], true).unwrap();
    alices.media(vec![4, 5], false).unwrap();
    let (mut video, mut sound) = (None, None);
    tokio::time::timeout(TIMEOUT, async {
        while video.is_none() || sound.is_none() {
            match signals.recv().await {
                Ok(AppSignal::Video(frame)) => video = Some(frame),
                Ok(AppSignal::Media(frame)) => sound = Some(frame),
                _ => {}
            }
        }
    })
    .await
    .expect("no video or sound came back");
    assert_eq!(video.unwrap(), [0, 1, 7, 8, 9]);
    assert_eq!(sound.unwrap(), [0, 4, 5]);
    server.shutdown().await;
}

#[tokio::test]
async fn apps_without_the_camera_permission_take_no_video() {
    let (server, wave, app, bundle) = setup_with(
        300,
        bundle_of("walkie", WALKIE, r#"["peers","streams","media"]"#),
    )
    .await;
    insert_gadget(&server, &wave, &app, &bundle).await;
    let apps = apps(&server);
    let alice = user("alice");
    allow(&server, &alice, &wave, &app).await;
    let alices = apps.open(&alice, &wave, GADGET).await.unwrap();
    assert!(alices.media(vec![1, 2, 3], true).is_err());
    assert!(alices.media(vec![1, 2, 3], false).is_ok());
    server.shutdown().await;
}
