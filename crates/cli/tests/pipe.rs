//! `rsntr pipe`: named binary streams — a registered `cat` endpoint
//! round-trips arbitrary bytes, an unadmitted peer is denied, and
//! `pipe accept` bridges an ad-hoc stream with cleanup.

#![cfg(unix)]

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rsntr::testutil::TempDir;
use rsntr::{Prefer, pipecmd, serve, store};

async fn admit_with_media(node: &serve::RunningNode, peer_hex: &str, source: &str) {
    let peer = peer_hex.to_string();
    let source = source.to_string();
    node.node()
        .db()
        .call(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO _peers (endpoint_id, added_at) \
                 VALUES (?1, datetime('now'))",
                [&peer],
            )
            .expect("admit");
            conn.execute(
                "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
                 VALUES (?1, ?2, 'audio-duplex', 'allow')",
                [&peer, &source],
            )
            .expect("duplex grant");
        })
        .await
        .expect("db call");
}

#[tokio::test(flavor = "multi_thread")]
async fn registered_pipe_round_trips_bytes_and_gates() {
    let ta = TempDir::new("pipe-a");
    let tb = TempDir::new("pipe-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    store::init_dir(tb.path()).expect("init b");

    pipecmd::pipe_add(tb.path(), Prefer::Local, "echo", "cat", false, None)
        .await
        .expect("pipe add");
    let listed = pipecmd::pipe_list(tb.path(), Prefer::Local)
        .await
        .expect("list");
    assert_eq!(listed.len(), 1);
    assert!(listed[0].duplex);

    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    let ticket = b.ready_ticket(Duration::from_secs(3)).await;
    store::peer_add(ta.path(), "b", &ticket, &[]).expect("a learns b");
    admit_with_media(&b, &a_id.to_string(), "echo").await;

    // Binary-safe round trip through the spawned client binary.
    let payload: Vec<u8> = (0..=255u8).cycle().take(64 * 1024 + 17).collect();
    let payload_clone = payload.clone();
    let dir = ta.path().to_path_buf();
    let (status, stdout, stderr) = tokio::task::spawn_blocking(move || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rsntr"))
            .args(["pipe", "open", "b", "echo", "-d"])
            .arg(&dir)
            .arg("--offline")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn pipe open");
        // Drain stdout concurrently: the echo fills the pipe buffer
        // while the payload is still being written.
        let mut out = child.stdout.take().expect("stdout");
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            out.read_to_end(&mut buf).expect("read stdout");
            buf
        });
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(&payload_clone)
            .expect("write payload");
        // Dropping stdin sends EOF; cat closes; the session ends.
        let stdout = reader.join().expect("reader thread");
        let out = child.wait_with_output().expect("pipe open output");
        (out.status, stdout, out.stderr)
    })
    .await
    .expect("join");
    assert_eq!(
        status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(stdout, payload, "byte-exact echo");

    // Without the media grant: denied, exit 2.
    let tc = TempDir::new("pipe-c");
    let c_id = store::init_dir(tc.path()).expect("init c");
    store::peer_add(tc.path(), "b", &ticket, &[]).expect("c learns b");
    let c_hex = c_id.to_string();
    b.node()
        .db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO _peers (endpoint_id, added_at) VALUES (?1, datetime('now'))",
                [&c_hex],
            )
            .expect("admit c without grant");
        })
        .await
        .expect("db call");
    let dir = tc.path().to_path_buf();
    let denied = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_rsntr"))
            .args(["pipe", "open", "b", "echo", "-d"])
            .arg(&dir)
            .arg("--offline")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("denied output")
    })
    .await
    .expect("join");
    assert_eq!(
        denied.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&denied.stderr)
    );

    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn accept_bridges_an_ad_hoc_stream_and_cleans_up() {
    let ta = TempDir::new("accept-a");
    let tb = TempDir::new("accept-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    store::init_dir(tb.path()).expect("init b");

    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    let ticket = b.ready_ticket(Duration::from_secs(3)).await;
    store::peer_add(ta.path(), "b", &ticket, &[]).expect("a learns b");
    admit_with_media(&b, &a_id.to_string(), "drop").await;

    // B side: accept one connection on "drop", stdout captured.
    let b_dir = tb.path().to_path_buf();
    let mut accept = Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(["pipe", "accept", "drop", "-d"])
        .arg(&b_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn accept");

    // The temporary endpoint appears.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let conn = store::open_db(tb.path()).expect("open b");
        let n: i64 = conn
            .query_row("SELECT count(*) FROM _media WHERE name = 'drop'", [], |r| {
                r.get(0)
            })
            .expect("count");
        if n == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "accept never registered");
        std::thread::sleep(Duration::from_millis(100));
    }

    // A side: pipe a payload in.
    let payload = b"ad-hoc bytes over an accepted pipe\n".to_vec();
    let payload_clone = payload.clone();
    let a_dir = ta.path().to_path_buf();
    let opened = tokio::task::spawn_blocking(move || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rsntr"))
            .args(["pipe", "open", "b", "drop", "-d"])
            .arg(&a_dir)
            .arg("--offline")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn open");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(&payload_clone)
            .expect("write");
        child.wait_with_output().expect("open output")
    })
    .await
    .expect("join");
    assert_eq!(
        opened.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&opened.stderr)
    );

    // The bytes reached the accept side's stdout; the accept process
    // exits after its one session.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = accept.try_wait().expect("try_wait") {
            assert!(status.success(), "accept exited {status}");
            break;
        }
        assert!(Instant::now() < deadline, "accept did not finish");
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut got = Vec::new();
    accept
        .stdout
        .take()
        .expect("accept stdout")
        .read_to_end(&mut got)
        .expect("read accept stdout");
    assert_eq!(got, payload);

    // The temporary row is gone.
    {
        let conn = store::open_db(tb.path()).expect("open b");
        let n: i64 = conn
            .query_row("SELECT count(*) FROM _media WHERE name = 'drop'", [], |r| {
                r.get(0)
            })
            .expect("count");
        assert_eq!(n, 0, "ephemeral endpoint removed");
    }

    b.shutdown().await;
}
