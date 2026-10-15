//! M2: the owner channel over loopback TCP. A directory whose path is
//! too long for a unix socket (an iOS Files-app folder) is served over
//! `rsntr.owner` only; every client (the CLI, `await`, entrainments)
//! rides it; a wrong token is refused; a stale record is ignored.

#![cfg(unix)]

use std::process::Command;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use resonator_protocol::{Entrain, EnvelopeObject};
use resonator_transport::RequestStream;
use rsntr::testutil::TempDir;
use rsntr::{OwnerChannel, Prefer, chat, lease, owner, serve, store};

fn deep_dir(tmp: &TempDir) -> std::path::PathBuf {
    let deep = tmp.path().join("a".repeat(120));
    std::fs::create_dir_all(&deep).unwrap();
    deep
}

fn rsntr(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(args)
        .output()
        .expect("run rsntr");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deep_directory_is_served_over_tcp_only() {
    let tmp = TempDir::new("tcp");
    let deep = deep_dir(&tmp);
    let d = deep.to_str().unwrap();
    store::init_dir(&deep).expect("init");
    chat::chat_init(&deep).expect("chat");
    // On a desktop the temp-dir alias rescues a deep path; an iOS app's
    // temp dir is as deep as its Documents, so point TMPDIR at a long
    // directory to reproduce "no unix socket fits at all". Children
    // inherit it. (Process-wide; the other tests here tolerate it.)
    let long_tmp = tmp.path().join("t".repeat(120));
    std::fs::create_dir_all(&long_tmp).unwrap();
    unsafe { std::env::set_var("TMPDIR", &long_tmp) };
    assert!(
        !rsntr::owner_socket::fits(&deep),
        "neither the path nor an alias fits sun_path"
    );

    let node = serve::start_node(&deep, true).await.expect("serve");
    assert!(
        !deep.join("rsntr.sock").exists(),
        "no unix socket for a deep path"
    );
    assert!(node.owner_tcp_port().is_some());
    let record = owner::read_record(&deep).expect("rsntr.owner");
    assert_eq!(record.app, "network.resonator.rsntr");
    assert_eq!(record.token.len(), 64);
    {
        use std::os::unix::fs::MetadataExt;
        let mode = std::fs::metadata(owner::owner_path(&deep)).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    // An entrainment over TCP.
    let mut watcher = owner::connect(&deep).await.expect("connect over tcp");
    watcher
        .send(&EnvelopeObject::Entrain(Entrain {
            id: "01TCPENTRAIN00000000000001".into(),
            point: resonator_node::chat::CHAT_INBOX_POINT.into(),
        }))
        .await
        .expect("send entrain");
    assert!(matches!(
        watcher.recv().await.expect("ack"),
        Some(EnvelopeObject::Done(_))
    ));

    // The CLI with the default preference rides TCP (the write executes
    // on the serving connection, so the commit hook vibrates at once).
    let (code, out, err) = rsntr(&[
        "--json",
        "sql",
        d,
        "INSERT INTO chat_messages (id, scope, sender, at, body) \
         VALUES ('01TCPMSG000000000000000001', 'x', 'someone', '2026-09-08T00:00:00Z', 'over tcp')",
    ]);
    assert_eq!(code, 0, "{out} {err}");
    let frame = tokio::time::timeout(Duration::from_secs(3), watcher.recv())
        .await
        .expect("vibration within 3 s")
        .expect("stream ok");
    assert!(
        matches!(frame, Some(EnvelopeObject::Vibration(_))),
        "{frame:?}"
    );

    // --socket insists on the serving process and is satisfied by TCP.
    let (code, out, err) = rsntr(&[
        "--json",
        "--socket",
        "sql",
        d,
        "SELECT count(*) AS n FROM chat_messages",
    ]);
    assert_eq!(code, 0, "{out} {err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["rows"][0][0], 1, "{v}");

    // `rsntr await` entrains over TCP and wakes on a foreign-looking row.
    let cursor = deep.join("agent.cursor");
    let awaiter = {
        let (d, c) = (d.to_string(), cursor.to_str().unwrap().to_string());
        std::thread::spawn(move || {
            rsntr(&[
                "--json",
                "await",
                "-d",
                &d,
                "--cursor-file",
                &c,
                "--timeout",
                "10",
            ])
        })
    };
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let (code, _, err) = rsntr(&[
        "--json",
        "sql",
        d,
        "INSERT INTO chat_messages (id, scope, sender, at, body) \
         VALUES ('01TCPMSG000000000000000002', 'x', 'someone', '2026-09-08T00:00:01Z', 'wake')",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = awaiter.join().unwrap();
    assert_eq!(code, 0, "await: {out} {err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["event"], "message", "{v}");
    assert_eq!(v["id"], "01TCPMSG000000000000000002");

    node.shutdown().await;
    assert!(
        !owner::owner_path(&deep).exists(),
        "the record is removed at shutdown"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_token_is_refused_without_a_byte() {
    let tmp = TempDir::new("tcp-token");
    store::init_dir(tmp.path()).expect("init");
    let node = serve::start_node(tmp.path(), true).await.expect("serve");
    let record = owner::read_record(tmp.path()).expect("record");

    let mut conn = tokio::net::TcpStream::connect(("127.0.0.1", record.port))
        .await
        .expect("dial");
    let wrong = format!("{}\n", "0".repeat(64));
    conn.write_all(wrong.as_bytes()).await.unwrap();
    let mut byte = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(5), conn.read(&mut byte))
        .await
        .expect("the server closes promptly")
        .unwrap_or(0);
    assert_eq!(n, 0, "no ack, no error text: the connection just closes");

    // A tampered record makes connect_tcp fail as refused, and
    // Prefer::Auto falls back to in-process rather than hanging.
    let tampered = owner::OwnerRecord {
        token: "f".repeat(64),
        ..record
    };
    std::fs::write(
        owner::owner_path(tmp.path()),
        serde_json::to_string(&tampered).unwrap(),
    )
    .unwrap();
    // Remove the unix socket so the TCP path is the only one probed.
    let _ = std::fs::remove_file(tmp.path().join("rsntr.sock"));
    let err = owner::connect_tcp(tmp.path()).await.err().expect("refused");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    let ch = OwnerChannel::open(tmp.path(), Prefer::Auto)
        .await
        .expect("auto");
    assert!(!ch.is_socket(), "falls back to in-process");
    node.shutdown().await;
}

#[test]
fn a_stale_record_after_a_kill_is_ignored() {
    let tmp = TempDir::new("tcp-stale");
    store::init_dir(tmp.path()).expect("init");
    let mut child = Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(["serve", "--foreground", "--offline"])
        .arg(tmp.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn serve");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !owner::owner_path(tmp.path()).exists() {
        assert!(Instant::now() < deadline, "no rsntr.owner");
        std::thread::sleep(Duration::from_millis(50));
    }
    let record = owner::read_record(tmp.path()).expect("record");
    assert_eq!(record.pid, child.id());
    unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
    let _ = child.wait();
    let deadline = Instant::now() + Duration::from_secs(5);
    while lease::is_held(tmp.path()) {
        assert!(Instant::now() < deadline, "lease not released");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        owner::owner_path(tmp.path()).exists(),
        "the record outlives the process"
    );
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        assert!(
            !owner::probe(tmp.path()).await,
            "a dead port is not a server"
        );
        let ch = OwnerChannel::open(tmp.path(), Prefer::Auto)
            .await
            .expect("auto");
        assert!(!ch.is_socket());
    });
    // The next server replaces the record.
    let (code, out, _) = rsntr(&["--json", "serve", "--offline", tmp.path().to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let pid = v["pid"].as_i64().unwrap() as i32;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(r) = owner::read_record(tmp.path())
            && r.pid as i32 == pid
        {
            break;
        }
        assert!(Instant::now() < deadline, "new record not written");
        std::thread::sleep(Duration::from_millis(50));
    }
    let (code, _, _) = rsntr(&["--json", "stop", tmp.path().to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(!owner::owner_path(tmp.path()).exists());
}

/// GitHub issue #1 (Windows): with no unix socket, `chat send --file` and
/// an auto-spilled body beside a serving node go over loopback TCP, and
/// the serving process imports the attachment. A forced `--local` send
/// still meets the lock, but leaves no spill file behind.
#[tokio::test(flavor = "multi_thread")]
async fn file_and_spill_sends_ride_tcp_beside_a_serving_node() {
    let tmp = TempDir::new("tcp-file");
    store::init_dir(tmp.path()).expect("init");
    chat::chat_init(tmp.path()).expect("chat");
    let peer = store::init_dir(&tmp.path().join("peer")).expect("init peer");
    let node = serve::start_node(tmp.path(), true).await.expect("serve");
    // What Windows sees: the TCP record only.
    let _ = std::fs::remove_file(tmp.path().join("rsntr.sock"));
    assert!(owner::read_record(tmp.path()).is_ok());

    let d = tmp.path().to_str().unwrap().to_string();
    let peer = peer.to_string();
    let tiny = tmp.path().join("tiny.txt");
    std::fs::write(&tiny, "x\n").unwrap();
    let body = tmp.path().join("body.txt");
    std::fs::write(&body, "{\"hi\":1}\n").unwrap();
    let big = tmp.path().join("big.txt");
    std::fs::write(&big, "y".repeat(100 * 1024)).unwrap();
    let spills = |dir: &std::path::Path| {
        std::fs::read_dir(dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".rsntr-spill-")
            })
            .count()
    };

    let run = |args: Vec<String>| {
        tokio::task::spawn_blocking(move || {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            rsntr(&args)
        })
    };
    let send = |extra: &[&str]| {
        let mut v: Vec<String> = ["--json", "chat", "send", &peer, "-d", &d]
            .iter()
            .map(|s| s.to_string())
            .collect();
        v.extend(extra.iter().map(|s| s.to_string()));
        v
    };

    let (code, out, err) = run(send(&[
        "--body-file",
        body.to_str().unwrap(),
        "--file",
        tiny.to_str().unwrap(),
    ]))
    .await
    .unwrap();
    assert_eq!(code, 0, "--file over tcp: {out} {err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ok"], true, "{v}");

    let (code, out, err) = run(send(&["--body-file", big.to_str().unwrap()]))
        .await
        .unwrap();
    assert_eq!(code, 0, "spilled body over tcp: {out} {err}");
    assert_eq!(spills(tmp.path()), 0, "spill removed after a send");

    // Forcing in-process beside the serving node meets the blob-store
    // lock: an error, but no stray spill file.
    let (code, out, err) = run(send(&["--local", "--body-file", big.to_str().unwrap()]))
        .await
        .unwrap();
    assert_ne!(code, 0, "{out} {err}");
    assert_eq!(spills(tmp.path()), 0, "spill removed after a failed send");

    let staged = std::fs::read_dir(tmp.path().join(chat::BLOBS_INBOX_DIR))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(staged, 0, "staged copies removed after import");
    node.shutdown().await;
}
