//! A media source whose bytes come from the embedding host rather than
//! from a shell command: what lets a node inside a phone serve its
//! camera, where iOS cannot exec a binary at all and the camera is an
//! in-process API. Over the wire it is an ordinary media feed, so a
//! viewer watches it with the same command it uses for a Pi camera.

use std::ops::Not;
use std::time::Duration;

use rsntr::client::MediaChunk;
use rsntr::testutil::TempDir;
use rsntr::{client, serve, store};

#[tokio::test(flavor = "multi_thread")]
async fn a_host_published_feed_serves_a_real_viewer() {
    let ta = TempDir::new("hostmedia-a"); // publisher (stands in for the phone)
    let tb = TempDir::new("hostmedia-b"); // viewer
    let a_id = store::init_dir(ta.path()).expect("init a");
    let b_id = store::init_dir(tb.path()).expect("init b");

    // The _media row is the declaration and the policy anchor; an empty
    // command means the host provides the bytes.
    {
        let conn = store::open_db(ta.path()).expect("open a");
        conn.execute(
            "INSERT INTO _media (name, command, content_type) \
             VALUES ('camera', '', 'video/mp4; codecs=\"avc1.4d401f\"')",
            [],
        )
        .expect("declare the source");
        conn.execute(
            "INSERT INTO _policy (peer_or_group, table_name, action, effect, note) \
             VALUES (?1, 'camera', 'media', 'allow', 'test viewer')",
            [b_id.to_string()],
        )
        .expect("grant the viewer");
        conn.execute(
            "INSERT OR IGNORE INTO _peers (endpoint_id, name, added_at) \
             VALUES (?1, 'viewer', datetime('now'))",
            [b_id.to_string()],
        )
        .expect("admit the viewer");
    }

    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    store::peer_add(
        tb.path(),
        "phone",
        &a.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("b learns a");

    // Watching before anything is published says so, rather than hanging.
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let dir = tb.path().to_path_buf();
    let early = tokio::spawn(async move {
        client::run_media_channel(&dir, "phone", "camera", true, tx, None).await
    });
    let first = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("an answer, not a hang");
    match first {
        Some(MediaChunk::Failed(e)) => {
            let reason = e.reason.unwrap_or_default();
            assert!(
                reason.contains("host-provided") && reason.contains("publishing"),
                "the error must name the cause: {reason}"
            );
        }
        other => panic!("expected a clear refusal while nothing publishes, got {other:?}"),
    }
    let _ = early.await.expect("join");

    // The app publishes and starts writing frames.
    let feed = a
        .node()
        .media_feeds()
        .publish("camera", "video/mp4; codecs=\"avc1.4d401f\"");
    let joined: std::sync::Arc<std::sync::atomic::AtomicUsize> = Default::default();
    let seen = joined.clone();
    feed.set_viewer_observer(Some(std::sync::Arc::new(move |n| {
        seen.store(n, std::sync::atomic::Ordering::SeqCst)
    })));

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let dir = tb.path().to_path_buf();
    let watching = tokio::spawn(async move {
        client::run_media_channel(&dir, "phone", "camera", true, tx, None).await
    });

    // The header carries the live feed's codec.
    match tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("header in time")
        .expect("header")
    {
        MediaChunk::Header { content_type } => {
            assert!(content_type.contains("avc1.4d401f"), "{content_type}")
        }
        other => panic!("expected the media header first, got {other:?}"),
    }

    // The app is told a viewer joined, so an encoder can force a keyframe.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while joined.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "no viewer notification"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(feed.viewers(), 1);

    // Frames written by the host arrive at the viewer, in order.
    let writer = tokio::spawn(async move {
        for i in 0..20u32 {
            feed.write(format!("frame-{i:03}").as_bytes());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        feed
    });
    let mut got = Vec::new();
    while got.len() < 5 {
        match tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("a frame in time")
        {
            Some(MediaChunk::Data(bytes)) => got.push(String::from_utf8_lossy(&bytes).to_string()),
            Some(other) => panic!("expected frame data, got {other:?}"),
            None => break,
        }
    }
    let joined_text = got.join("");
    assert!(joined_text.contains("frame-000"), "{got:?}");
    assert!(
        joined_text.contains("frame-001") && joined_text.contains("frame-002"),
        "frames arrive in order: {got:?}"
    );

    // Withdrawing ends the viewer's stream cleanly.
    let feed = writer.await.expect("writer");
    assert!(a.node().media_feeds().withdraw("camera"));
    drop(feed);
    while (tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("end in time"))
    .is_some()
    {}
    watching
        .await
        .expect("join")
        .expect("the feed ends cleanly");
    assert!(a.node().media_feeds().published().is_empty());

    let _ = a_id;
    a.shutdown().await;
}

/// A command-backed source that fails to start (a busy camera, a wrong
/// device) used to answer with a media header and then a clean end,
/// which a viewer cannot tell from a feed that simply finished: the
/// symptom the kontrolator fleet reported as "delivered zero bytes and
/// hung". It must name the failure instead, with what the source said.
#[tokio::test(flavor = "multi_thread")]
async fn a_source_that_produces_nothing_says_why() {
    let ta = TempDir::new("failmedia-a");
    let tb = TempDir::new("failmedia-b");
    store::init_dir(ta.path()).expect("init a");
    let b_id = store::init_dir(tb.path()).expect("init b");
    {
        let conn = store::open_db(ta.path()).expect("open a");
        conn.execute_batch(
            "INSERT INTO _media (name, command, content_type) VALUES \
               ('broken', 'echo \"cannot open /dev/video0: Device or resource busy\" >&2; exit 1', 'video/mp2t'), \
               ('working', 'printf best-effort-frame', 'video/mp2t');",
        )
        .expect("declare sources");
        for src in ["broken", "working"] {
            conn.execute(
                "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
                 VALUES (?1, ?2, 'media', 'allow')",
                (b_id.to_string(), src),
            )
            .expect("grant");
        }
        conn.execute(
            "INSERT OR IGNORE INTO _peers (endpoint_id, name, added_at) \
             VALUES (?1, 'viewer', datetime('now'))",
            [b_id.to_string()],
        )
        .expect("admit");
    }
    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    store::peer_add(
        tb.path(),
        "cam",
        &a.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("b learns a");

    // The failing source: an error naming the source and its own words,
    // and no header, because the feed never started.
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let dir = tb.path().to_path_buf();
    let watch = tokio::spawn(async move {
        client::run_media_channel(&dir, "cam", "broken", true, tx, None).await
    });
    let mut chunks = Vec::new();
    while let Some(c) = tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .expect("an answer, not a hang")
    {
        chunks.push(c);
    }
    let _ = watch.await.expect("join");
    assert!(
        !chunks
            .iter()
            .any(|c| matches!(c, MediaChunk::Header { .. })),
        "a feed that never started must not announce itself: {chunks:?}"
    );
    match chunks.first() {
        Some(MediaChunk::Failed(e)) => {
            let reason = e.reason.clone().unwrap_or_default();
            assert!(reason.contains("broken"), "names the source: {reason}");
            assert!(
                reason.contains("Device or resource busy"),
                "carries what the source said: {reason}"
            );
        }
        other => panic!("expected a named failure, got {other:?}"),
    }

    // A source that does produce data is unaffected: header, then bytes.
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let dir = tb.path().to_path_buf();
    let watch = tokio::spawn(async move {
        client::run_media_channel(&dir, "cam", "working", true, tx, None).await
    });
    let mut got = Vec::new();
    while let Some(c) = tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .expect("an answer")
    {
        got.push(c);
    }
    // The run reports the path that carried it. These two nodes are
    // bound offline, so there is no relay to be had: the report must
    // say direct and name a real address. This is the fact `has_relay`
    // cannot give, and the only honest basis for a directness claim.
    let path = watch
        .await
        .expect("join")
        .expect("media run")
        .expect("a path for a connection that just delivered bytes");
    assert!(
        !path.relay,
        "an offline connection is never relayed: {path:?}"
    );
    assert!(
        path.remote.is_some(),
        "a selected path has an address: {path:?}"
    );
    assert!(path.direct >= 1, "at least one direct path: {path:?}");
    assert!(
        path.describe().starts_with("direct via"),
        "human line reads right: {}",
        path.describe()
    );
    assert!(
        matches!(got.first(), Some(MediaChunk::Header { .. })),
        "a real feed still announces itself first: {got:?}"
    );
    let data: String = got
        .iter()
        .filter_map(|c| match c {
            MediaChunk::Data(b) => Some(String::from_utf8_lossy(b).to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(data, "best-effort-frame");

    a.shutdown().await;
}

/// A source that opens but never produces (a camera that is present and
/// silent) leaves the wire legitimately quiet, so the serving side has
/// nothing to report. The client is the one that must give up: the open
/// gets a deadline, and blowing it is exit 3, not an infinite wait.
#[tokio::test(flavor = "multi_thread")]
async fn an_open_that_never_produces_times_out() {
    let ta = TempDir::new("hangmedia-a");
    let tb = TempDir::new("hangmedia-b");
    store::init_dir(ta.path()).expect("init a");
    let b_id = store::init_dir(tb.path()).expect("init b");
    {
        let conn = store::open_db(ta.path()).expect("open a");
        conn.execute_batch(
            "INSERT INTO _media (name, command, content_type) VALUES \
               ('silent', 'sleep 600', 'video/mp2t');",
        )
        .expect("declare source");
        conn.execute(
            "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
             VALUES (?1, 'silent', 'media', 'allow')",
            [b_id.to_string()],
        )
        .expect("grant");
        conn.execute(
            "INSERT OR IGNORE INTO _peers (endpoint_id, name, added_at) \
             VALUES (?1, 'viewer', datetime('now'))",
            [b_id.to_string()],
        )
        .expect("admit");
    }
    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    store::peer_add(
        tb.path(),
        "cam",
        &a.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("b learns a");

    let started = std::time::Instant::now();
    let out = tokio::task::spawn_blocking({
        let dir = tb.path().to_path_buf();
        move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
                .args(["watch", "cam", "silent", "-d"])
                .arg(&dir)
                .args(["--offline", "--first-byte-timeout", "2"])
                .output()
                .expect("running rsntr media")
        }
    })
    .await
    .expect("join");
    let elapsed = started.elapsed();

    assert_eq!(
        out.status.code(),
        Some(rsntr::EXIT_TIMEOUT),
        "a blown open deadline is exit 3: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        elapsed < Duration::from_secs(20),
        "it gave up near the deadline, not at the source's own pace: {elapsed:?}"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("silent"), "names the source: {err}");
    assert!(err.contains("within 2s"), "names the deadline: {err}");
    assert!(out.stdout.is_empty(), "no feed bytes were invented");

    a.shutdown().await;
}

/// A live source never ends, so a path report only at close would never
/// arrive for the very case that needs it: watching a camera for ninety
/// seconds and asking whether the connection went direct. The report
/// must land while the feed is still running.
#[tokio::test(flavor = "multi_thread")]
async fn a_running_feed_reports_its_path_before_it_ends() {
    let ta = TempDir::new("livepath-a");
    let tb = TempDir::new("livepath-b");
    store::init_dir(ta.path()).expect("init a");
    let b_id = store::init_dir(tb.path()).expect("init b");
    {
        let conn = store::open_db(ta.path()).expect("open a");
        conn.execute_batch(
            "INSERT INTO _media (name, command, content_type) VALUES \
               ('live', 'printf x; sleep 120', 'video/mp2t');",
        )
        .expect("declare source");
        conn.execute(
            "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
             VALUES (?1, 'live', 'media', 'allow')",
            [b_id.to_string()],
        )
        .expect("grant");
        conn.execute(
            "INSERT OR IGNORE INTO _peers (endpoint_id, name, added_at) \
             VALUES (?1, 'viewer', datetime('now'))",
            [b_id.to_string()],
        )
        .expect("admit");
    }
    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    store::peer_add(
        tb.path(),
        "cam",
        &a.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("b learns a");

    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let (ptx, mut prx) = tokio::sync::mpsc::channel(4);
    let dir = tb.path().to_path_buf();
    let watch = tokio::spawn(async move {
        client::run_media_channel(&dir, "cam", "live", true, tx, Some(ptx)).await
    });

    // The feed is open and will stay open for two minutes.
    let first = tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .expect("the header arrives");
    assert!(
        matches!(first, Some(MediaChunk::Header { .. })),
        "header first: {first:?}"
    );

    // The path report must arrive well inside the feed's lifetime.
    let path = tokio::time::timeout(Duration::from_secs(15), prx.recv())
        .await
        .expect("a path report while the feed runs")
        .expect("a report, not a closed channel");
    assert!(!path.relay, "offline is never relayed: {path:?}");
    assert!(path.remote.is_some(), "a selected path has an address");

    // Still live: the source has 100+ seconds left, so this proves the
    // report did not come from the feed ending.
    assert!(
        watch.is_finished().not(),
        "the feed was still running when the path was reported"
    );
    watch.abort();
    a.shutdown().await;
}

/// A viewer whose own directory is being served must not bind a second
/// endpoint for that identity: two endpoints under one id displace each
/// other on their relay, which leaves the daemon deaf for as long as the
/// client runs (docs/relay-identity-sharing.md). The serving process
/// does the dialling and copies the bytes, and the feed is unchanged.
#[tokio::test(flavor = "multi_thread")]
async fn a_serving_viewer_reaches_the_peer_through_its_own_daemon() {
    let ta = TempDir::new("proxy-a");
    let tb = TempDir::new("proxy-b");
    store::init_dir(ta.path()).expect("init a");
    let b_id = store::init_dir(tb.path()).expect("init b");
    {
        let conn = store::open_db(ta.path()).expect("open a");
        conn.execute_batch(
            "INSERT INTO _media (name, command, content_type) VALUES \
               ('feed', 'printf through-the-daemon', 'text/plain');",
        )
        .expect("declare source");
        conn.execute(
            "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
             VALUES (?1, 'feed', 'media', 'allow')",
            [b_id.to_string()],
        )
        .expect("grant");
        conn.execute(
            "INSERT OR IGNORE INTO _peers (endpoint_id, name, added_at) \
             VALUES (?1, 'viewer', datetime('now'))",
            [b_id.to_string()],
        )
        .expect("admit");
    }
    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    store::peer_add(
        tb.path(),
        "cam",
        &a.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("b learns a");
    // B serves its own directory too, which is the whole point: its
    // client commands must go through it rather than beside it.
    let b = serve::start_node(tb.path(), true).await.expect("serve b");

    let session = client::open_peer(tb.path(), "cam", true)
        .await
        .expect("opening a peer from a served directory");
    assert!(
        session.probe.is_proxied(),
        "a viewer with its own daemon must not bind a second endpoint"
    );
    drop(session);

    // And the feed itself is unchanged through the tunnel.
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let dir = tb.path().to_path_buf();
    let watch = tokio::spawn(async move {
        client::run_media_channel(&dir, "cam", "feed", true, tx, None).await
    });
    let mut got = Vec::new();
    while let Some(c) = tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .expect("an answer, not a hang")
    {
        got.push(c);
    }
    let _ = watch.await.expect("join");
    assert!(
        matches!(got.first(), Some(MediaChunk::Header { .. })),
        "header first through the tunnel: {got:?}"
    );
    let data: String = got
        .iter()
        .filter_map(|c| match c {
            MediaChunk::Data(b) => Some(String::from_utf8_lossy(b).to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(data, "through-the-daemon");

    b.shutdown().await;
    a.shutdown().await;
}
