//! The runtime against the counter guests (Rust and JavaScript), on
//! Cranelift and on Pulley.

use std::io::Read;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use resonator_apps::{
    AppEngine, AppError, Component, Deadlines, Effect, EngineOptions, Event, Instance,
    InstanceConfig, LogLevel, Manifest, Peer, Permission, Snapshot, UiEvent, UiNode, WaveMessage,
    spawn,
};

#[derive(Clone, Copy, Debug)]
enum Guest {
    Rust,
    Js,
}

fn wasm(guest: Guest) -> Vec<u8> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    match guest {
        Guest::Rust => std::fs::read(format!("{dir}/counter_rs.wasm")).unwrap(),
        Guest::Js => {
            let gz = std::fs::read(format!("{dir}/counter_js.wasm.gz")).unwrap();
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(&gz[..])
                .read_to_end(&mut out)
                .unwrap();
            out
        }
    }
}

fn cranelift() -> &'static AppEngine {
    static ENGINE: OnceLock<AppEngine> = OnceLock::new();
    ENGINE.get_or_init(|| AppEngine::new(EngineOptions::default()).unwrap())
}

fn pulley() -> &'static AppEngine {
    static ENGINE: OnceLock<AppEngine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        AppEngine::new(EngineOptions {
            pulley: true,
            ..EngineOptions::default()
        })
        .unwrap()
    })
}

/// Compiled components, one per engine and guest (compiling the JS guest
/// takes a while).
fn component(engine: &'static AppEngine, guest: Guest) -> Component {
    static CACHE: OnceLock<std::sync::Mutex<Vec<(usize, u8, Component)>>> = OnceLock::new();
    let key = (engine as *const AppEngine as usize, guest as u8);
    let cache = CACHE.get_or_init(Default::default);
    if let Some((_, _, c)) = cache
        .lock()
        .unwrap()
        .iter()
        .find(|(e, g, _)| (*e, *g) == key)
    {
        return c.clone();
    }
    let compiled = engine.compile(&wasm(guest)).unwrap();
    cache.lock().unwrap().push((key.0, key.1, compiled.clone()));
    compiled
}

fn manifest(permissions: &[Permission], memory_mb: Option<u32>) -> Manifest {
    Manifest {
        name: "counter".into(),
        version: "1".into(),
        description: None,
        permissions: permissions.to_vec(),
        memory_mb,
    }
}

fn config(manifest: Manifest, snapshot: Snapshot, deadlines: Deadlines) -> InstanceConfig {
    InstanceConfig {
        app_id: "sha256:00".into(),
        manifest,
        self_peer: Peer {
            id: "me".into(),
            participant: "alice@example.com".into(),
            device: "laptop".into(),
            local: false,
        },
        snapshot,
        deadlines,
    }
}

/// Pulley is an interpreter, and SpiderMonkey on it starts slowly.
fn deadlines(engine: &AppEngine) -> Deadlines {
    if engine.is_pulley() {
        Deadlines {
            event: Duration::from_secs(20),
            render: Duration::from_secs(5),
        }
    } else {
        Deadlines {
            event: Duration::from_secs(2),
            render: Duration::from_secs(1),
        }
    }
}

fn start(
    engine: &'static AppEngine,
    guest: Guest,
    permissions: &[Permission],
    snapshot: Snapshot,
) -> (Instance, Vec<Effect>) {
    Instance::start(
        engine,
        &component(engine, guest),
        config(manifest(permissions, None), snapshot, deadlines(engine)),
    )
    .unwrap()
}

fn click() -> UiEvent {
    UiEvent {
        node: 2,
        name: "click".into(),
        value: None,
        fields: vec![],
    }
}

fn event(name: &str) -> UiEvent {
    UiEvent {
        node: 0,
        name: name.into(),
        value: None,
        fields: vec![],
    }
}

fn counter_nodes(count: &str, error: Option<&str>) -> Vec<UiNode> {
    let node =
        |parent, tag: &str, text: Option<&str>, attrs: &[(&str, &str)], on: &[&str]| UiNode {
            parent,
            tag: tag.into(),
            text: text.map(Into::into),
            attrs: attrs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            on: on.iter().map(|s| s.to_string()).collect(),
        };
    let mut nodes = vec![
        node(None, "div", None, &[("class", "app-row")], &[]),
        node(Some(0), "span", Some(count), &[], &[]),
        node(Some(0), "button", Some("+1"), &[], &["click"]),
    ];
    if let Some(e) = error {
        nodes.push(node(Some(0), "p", Some(e), &[("class", "app-error")], &[]));
    }
    nodes
}

const ALL: &[Permission] = &[Permission::Wave, Permission::Kv];

/// The whole counter conversation; returns everything observed so the two
/// guests can be compared.
fn counter_run(engine: &'static AppEngine, guest: Guest) -> Vec<String> {
    let mut seen = Vec::new();
    let (mut app, init) = start(engine, guest, ALL, Snapshot::default());
    assert_eq!(
        init,
        vec![Effect::Log {
            level: LogLevel::Info,
            message: "counter started".into()
        }],
        "{guest:?}"
    );
    seen.push(format!("{init:?}"));
    assert_eq!(
        app.render(0).unwrap(),
        counter_nodes("0", None),
        "{guest:?}"
    );

    let effects = app.on_ui(0, &click()).unwrap();
    assert_eq!(
        effects,
        vec![
            Effect::KvSet {
                key: "count".into(),
                value: b"1".to_vec()
            },
            Effect::Post {
                kind: "inc".into(),
                body: "1".into()
            },
            Effect::Invalidate { view: 0 },
        ],
        "{guest:?}"
    );
    seen.push(format!("{effects:?}"));
    assert_eq!(
        app.render(0).unwrap(),
        counter_nodes("1", None),
        "{guest:?}"
    );
    assert_eq!(app.snapshot().kv.get("count").unwrap(), b"1");

    let message = |seq, body: &str| WaveMessage {
        seq,
        author: "bob@example.com".into(),
        kind: "inc".into(),
        body: body.into(),
    };
    let effects = app
        .on_messages(vec![message(1, "5"), message(2, "3")])
        .unwrap();
    assert_eq!(effects, vec![Effect::Invalidate { view: 0 }], "{guest:?}");
    // Already seen: dropped without a call.
    assert_eq!(app.on_messages(vec![message(2, "9")]).unwrap(), vec![]);
    let nodes = app.render(0).unwrap();
    assert_eq!(nodes, counter_nodes("5", None), "{guest:?}");
    seen.push(format!("{nodes:?}"));
    seen
}

#[test]
fn rust_and_js_counters_behave_the_same() {
    let rust = counter_run(cranelift(), Guest::Rust);
    let js = counter_run(cranelift(), Guest::Js);
    assert_eq!(rust, js);
}

#[test]
fn kv_comes_from_the_snapshot() {
    for guest in [Guest::Rust, Guest::Js] {
        let mut snapshot = Snapshot::default();
        snapshot.kv.insert("count".into(), b"7".to_vec());
        let (mut app, _) = start(cranelift(), guest, ALL, snapshot);
        assert_eq!(
            app.render(0).unwrap(),
            counter_nodes("7", None),
            "{guest:?}"
        );
    }
}

#[test]
fn ungranted_imports_are_denied() {
    for guest in [Guest::Rust, Guest::Js] {
        let mut snapshot = Snapshot::default();
        snapshot.kv.insert("count".into(), b"7".to_vec());
        let (mut app, _) = start(cranelift(), guest, &[], snapshot);
        // kv.get gives nothing without the grant.
        assert_eq!(
            app.render(0).unwrap(),
            counter_nodes("0", None),
            "{guest:?}"
        );
        let effects = app.on_ui(0, &click()).unwrap();
        assert_eq!(effects, vec![Effect::Invalidate { view: 0 }], "{guest:?}");
        assert_eq!(
            app.render(0).unwrap(),
            counter_nodes("1", Some("kv: permission denied; wave: permission denied")),
            "{guest:?}"
        );
    }
}

#[test]
fn an_endless_loop_is_stopped_at_the_deadline() {
    for guest in [Guest::Rust, Guest::Js] {
        let (mut app, _) = start(cranelift(), guest, ALL, Snapshot::default());
        let began = Instant::now();
        let error = app.on_ui(0, &event("spin")).unwrap_err();
        let took = began.elapsed();
        assert_eq!(error, AppError::Timeout, "{guest:?}");
        assert!(took < Duration::from_secs(3), "{guest:?} took {took:?}");
        // A failed instance takes no more calls.
        assert!(matches!(app.render(0), Err(AppError::Failed(_))));
        assert!(app.failure().is_some());
    }
}

#[test]
fn the_default_deadline_is_short() {
    let engine = cranelift();
    let (mut app, _) = Instance::start(
        engine,
        &component(engine, Guest::Rust),
        config(
            manifest(ALL, None),
            Snapshot::default(),
            Deadlines::default(),
        ),
    )
    .unwrap();
    let began = Instant::now();
    assert_eq!(app.on_ui(0, &event("spin")).unwrap_err(), AppError::Timeout);
    let took = began.elapsed();
    assert!(
        took >= Duration::from_millis(190) && took < Duration::from_millis(400),
        "took {took:?}"
    );
}

#[test]
fn too_much_memory_fails_the_instance() {
    for guest in [Guest::Rust, Guest::Js] {
        let engine = cranelift();
        let (mut app, _) = Instance::start(
            engine,
            &component(engine, guest),
            config(
                manifest(ALL, Some(64)),
                Snapshot::default(),
                deadlines(engine),
            ),
        )
        .unwrap();
        let error = app.on_ui(0, &event("hog")).unwrap_err();
        assert!(matches!(error, AppError::Trap(_)), "{guest:?}: {error:?}");
        assert!(app.failure().is_some());
    }
}

#[test]
fn a_cache_hit_skips_compiling() {
    let dir = tempfile::tempdir().unwrap();
    let options = EngineOptions {
        cache_dir: Some(dir.path().to_owned()),
        pulley: false,
    };
    let wasm = wasm(Guest::Rust);
    let first = AppEngine::new(options.clone()).unwrap();
    first.compile(&wasm).unwrap();
    assert_eq!(first.cache_hits(), 0);
    let second = AppEngine::new(options).unwrap();
    second.compile(&wasm).unwrap();
    assert_eq!(second.cache_hits(), 1);
}

#[test]
fn pulley_runs_the_same_counters() {
    let rust = counter_run(pulley(), Guest::Rust);
    let native = counter_run(cranelift(), Guest::Rust);
    assert_eq!(rust, native);
    let js = counter_run(pulley(), Guest::Js);
    assert_eq!(js, native);
}

#[test]
fn pulley_stops_an_endless_loop() {
    let engine = pulley();
    let (mut app, _) = Instance::start(
        engine,
        &component(engine, Guest::Rust),
        config(
            manifest(ALL, None),
            Snapshot::default(),
            Deadlines::default(),
        ),
    )
    .unwrap();
    assert_eq!(app.on_ui(0, &event("spin")).unwrap_err(), AppError::Timeout);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_worker_runs_events_in_order() {
    let engine = cranelift();
    let (app, _) = Instance::start(
        engine,
        &component(engine, Guest::Rust),
        config(manifest(ALL, None), Snapshot::default(), deadlines(engine)),
    )
    .unwrap();
    let (handle, mut reports) = spawn("counter", app).unwrap();
    for _ in 0..3 {
        handle
            .send(Event::Ui {
                view: 0,
                event: click(),
            })
            .await
            .unwrap();
    }
    for n in 1..=3 {
        let report = reports.recv().await.unwrap();
        assert!(report.failure.is_none());
        assert!(report.effects.contains(&Effect::Post {
            kind: "inc".into(),
            body: n.to_string()
        }));
    }
    assert_eq!(handle.render(0).await.unwrap(), counter_nodes("3", None));

    handle
        .send(Event::Ui {
            view: 0,
            event: event("spin"),
        })
        .await
        .unwrap();
    let report = reports.recv().await.unwrap();
    assert_eq!(report.failure, Some(AppError::Timeout));
    // The worker stops after a failure.
    assert!(reports.recv().await.is_none());
    assert!(handle.render(0).await.is_err());
}
