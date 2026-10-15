//! M2: writes made by OTHER processes reach a serving node's
//! entrainments, hooks, and outbox (docs/owner-channel.md finding 1 is
//! closed for every writer, not just socket-routed ones).

#![cfg(unix)]

use std::process::Command;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use resonator_protocol::{EnvelopeObject, decode_envelope, encode_envelope};
use rsntr::testutil::TempDir;
use rsntr::{chat, serve, store};

const IO_TIMEOUT: Duration = Duration::from_secs(10);

async fn send_frame(conn: &mut UnixStream, obj: &EnvelopeObject) {
    let mut out = BytesMut::new();
    encode_envelope(obj, &mut out).expect("encode");
    tokio::time::timeout(IO_TIMEOUT, conn.write_all(&out))
        .await
        .expect("write timeout")
        .expect("write");
}

async fn read_frame(
    conn: &mut UnixStream,
    buf: &mut BytesMut,
    within: Duration,
) -> Option<EnvelopeObject> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if let Some(obj) = decode_envelope(buf).expect("decode") {
            return Some(obj);
        }
        match tokio::time::timeout_at(deadline, conn.read_buf(buf)).await {
            Ok(Ok(0)) | Err(_) => return None,
            Ok(Ok(_)) => {}
            Ok(Err(e)) => panic!("read: {e}"),
        }
    }
}

fn local_sql(dir: &std::path::Path, sql: &str) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(["--json", "--local", "sql"])
        .arg(dir)
        .arg(sql)
        .output()
        .expect("run sql");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_local_cli_write_vibrates_a_socket_entrainment_and_fires_a_hook() {
    let tmp = TempDir::new("foreign");
    store::init_dir(tmp.path()).expect("init");
    chat::chat_init(tmp.path()).expect("chat");
    let node = serve::start_node(tmp.path(), true).await.expect("serve");

    // A hook that records each message event into a file.
    let marker = tmp.path().join("hook-fired");
    let cmd = format!("cat >> {}", marker.display());
    rsntr::hooks::hook_add(tmp.path(), rsntr::Prefer::Auto, "message", &cmd)
        .await
        .expect("hook add");

    // Entrain the chat inbox point over the control socket.
    let mut watcher = rsntr::owner_socket::connect(tmp.path())
        .await
        .expect("connect");
    let mut buf = BytesMut::new();
    send_frame(
        &mut watcher,
        &EnvelopeObject::Entrain(resonator_protocol::Entrain {
            id: "01FOREIGNENTRAIN0000000001".into(),
            point: resonator_node::chat::CHAT_INBOX_POINT.into(),
        }),
    )
    .await;
    assert!(matches!(
        read_frame(&mut watcher, &mut buf, IO_TIMEOUT).await,
        Some(EnvelopeObject::Done(_))
    ));

    // A foreign write: another process, in-process pipeline, never the
    // socket (the old finding 1).
    let started = Instant::now();
    let (code, err) = local_sql(
        tmp.path(),
        "INSERT INTO chat_messages (id, scope, sender, at, body) \
         VALUES ('01FOREIGNMSG00000000000001', 'x', 'someone', '2026-09-08T00:00:00Z', 'from elsewhere')",
    );
    assert_eq!(code, 0, "{err}");
    match read_frame(&mut watcher, &mut buf, Duration::from_secs(3)).await {
        Some(EnvelopeObject::Vibration(_)) => {}
        other => panic!("expected a vibration for the foreign write, got {other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(3));

    // The hook runner saw it too (through the same watcher).
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = std::fs::read_to_string(&marker)
            && text.contains("from elsewhere")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the hook never fired for the foreign write"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Many foreign writers beside the server: no busy or snapshot errors.
    let dir = tmp.path().to_path_buf();
    let writers: Vec<_> = (0..3)
        .map(|w| {
            let d = dir.clone();
            std::thread::spawn(move || {
                let mut failures = Vec::new();
                for i in 0..25 {
                    let (code, err) = local_sql(
                        &d,
                        &format!(
                            "INSERT INTO chat_messages (id, scope, sender, at, body) \
                             VALUES ('01LOAD{w}{i:020}', 'x', 'w{w}', '2026-09-08T00:00:00Z', 'n{i}')"
                        ),
                    );
                    if code != 0 {
                        failures.push(err);
                    }
                }
                failures
            })
        })
        .collect();
    for w in writers {
        let failures = w.join().unwrap();
        assert!(failures.is_empty(), "{failures:?}");
    }
    let n: i64 = store::open_db(tmp.path())
        .unwrap()
        .query_row("SELECT count(*) FROM chat_messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 76);
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_local_chat_send_is_delivered_without_waiting_for_the_poll() {
    let ta = TempDir::new("fw-a");
    let tb = TempDir::new("fw-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    store::init_dir(tb.path()).expect("init b");
    chat::chat_init(ta.path()).expect("chat a");
    chat::chat_init(tb.path()).expect("chat b");
    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    store::peer_add(tb.path(), "a", &a_id.to_string(), &[]).expect("b admits a");
    store::peer_add(
        ta.path(),
        "b",
        &b.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("a learns b");
    let a = serve::start_node(ta.path(), true).await.expect("serve a");

    // `--local`: the send lands on a separate connection in a separate
    // process; only the watcher can wake a's outbox before its 2 s poll.
    let started = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args([
            "--json",
            "--local",
            "chat",
            "send",
            "b",
            "quick, no socket",
            "-d",
        ])
        .arg(ta.path())
        .output()
        .expect("send");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    let id = v["message_id"].as_str().unwrap().to_string();
    let deadline = Instant::now() + Duration::from_secs(5);
    let arrived = loop {
        let log = chat::chat_log(tb.path(), &a_id.to_string(), 10, None).expect("log");
        if log.iter().any(|e| e.id == id) {
            break started.elapsed();
        }
        assert!(Instant::now() < deadline, "never delivered");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        arrived < Duration::from_millis(1800),
        "took {arrived:?}: the poll, not the watcher"
    );
    a.shutdown().await;
    b.shutdown().await;
}
