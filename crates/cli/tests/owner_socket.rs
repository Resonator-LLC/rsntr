//! Control socket integration (docs/owner-channel.md sec 3.2): a serving
//! node listens on `<dir>/rsntr.sock`; framed owner envelopes execute on
//! the serving connection, so its update hook fires and an entrained
//! Sympathetic point receives Vibrations for socket-delivered writes.
#![cfg(unix)]

use std::path::Path;
use std::time::Duration;

use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use resonator_protocol::{EnvelopeObject, Request, RequestKind, decode_envelope, encode_envelope};
use rsntr::testutil::TempDir;
use rsntr::{serve, store};

const IO_TIMEOUT: Duration = Duration::from_secs(20);

async fn send_frame(conn: &mut UnixStream, obj: &EnvelopeObject) {
    let mut out = BytesMut::new();
    encode_envelope(obj, &mut out).expect("encode frame");
    tokio::time::timeout(IO_TIMEOUT, conn.write_all(&out))
        .await
        .expect("write timed out")
        .expect("write frame");
}

async fn read_frame(conn: &mut UnixStream, buf: &mut BytesMut) -> Option<EnvelopeObject> {
    loop {
        if let Some(obj) = decode_envelope(buf).expect("decode frame") {
            return Some(obj);
        }
        let n = tokio::time::timeout(IO_TIMEOUT, conn.read_buf(buf))
            .await
            .expect("read timed out")
            .expect("read");
        if n == 0 {
            assert!(buf.is_empty(), "connection closed mid-frame");
            return None;
        }
    }
}

fn sock_mode(path: &Path) -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).expect("socket metadata").mode() & 0o777
}

#[tokio::test]
async fn socket_owner_execute_vibrates_entrained_point() {
    let tmp = TempDir::new("owner-sock");
    store::init_dir(tmp.path()).expect("init");
    {
        let conn = store::open_db(tmp.path()).expect("open");
        conn.execute_batch(
            "CREATE TABLE plants (id INTEGER PRIMARY KEY, name TEXT);
             INSERT INTO _projection (point_iri, kind, path, ord, resource)
             VALUES ('urn:x:plants-changed', 'sympathetic', '', 0, 'plants');",
        )
        .expect("fixture");
    }

    let node = serve::start_node(tmp.path(), true).await.expect("serve");
    let sock = tmp.path().join("rsntr.sock");
    assert!(
        sock.exists(),
        "the control socket is created at serve start"
    );
    assert_eq!(sock_mode(&sock), 0o600, "the socket is owner-only");

    // Connection A: entrain the Sympathetic point; ack is a bare Done.
    let mut watcher = rsntr::owner_socket::connect(tmp.path())
        .await
        .expect("connect watcher");
    let mut watcher_buf = BytesMut::new();
    send_frame(
        &mut watcher,
        &EnvelopeObject::Entrain(resonator_protocol::Entrain {
            id: "01JSOCKENTRAIN000000000001".into(),
            point: "urn:x:plants-changed".into(),
        }),
    )
    .await;
    match read_frame(&mut watcher, &mut watcher_buf).await {
        Some(EnvelopeObject::Done(_)) => {}
        other => panic!("expected the entrain ack Done, got {other:?}"),
    }

    // Connection B: an owner Execute that writes the entrained table.
    let mut writer = rsntr::owner_socket::connect(tmp.path())
        .await
        .expect("connect writer");
    let mut writer_buf = BytesMut::new();
    let req = Request::new(
        RequestKind::Execute,
        "sql-sqlite",
        "INSERT INTO plants (name) VALUES ('fern')",
    );
    send_frame(&mut writer, &req.to_envelope()).await;
    match read_frame(&mut writer, &mut writer_buf).await {
        Some(EnvelopeObject::Result(_)) => {}
        other => panic!("expected Result, got {other:?}"),
    }
    match read_frame(&mut writer, &mut writer_buf).await {
        Some(EnvelopeObject::Done(d)) => assert_eq!(d.affected_rows, Some(1)),
        other => panic!("expected Done, got {other:?}"),
    }

    // The write committed on the serving connection, so the update hook
    // fired and the watcher receives its Vibration (recorded finding 1).
    match read_frame(&mut watcher, &mut watcher_buf).await {
        Some(EnvelopeObject::Vibration(v)) => {
            assert_eq!(v.point, "urn:x:plants-changed");
        }
        other => panic!("expected a Vibration, got {other:?}"),
    }

    // Damping is closing the connection on this transport.
    drop(watcher);

    // The owner op is on the ledger: direction local, decided_by owner.
    let (direction, peer, decision, decided_by): (String, String, String, String) = node
        .node()
        .db()
        .call(|conn| {
            conn.query_row(
                "SELECT direction, peer, decision, decided_by FROM _audit \
                 WHERE signal LIKE 'INSERT INTO plants%' ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
        })
        .await
        .unwrap();
    assert_eq!(direction, "local");
    assert_eq!(peer, node.peer_id().to_string());
    assert_eq!(decision, "allow");
    assert_eq!(decided_by, "owner");

    node.shutdown().await;
    assert!(!sock.exists(), "clean shutdown removes the socket");
}

#[tokio::test]
async fn socket_owner_ddl_and_media_exclusion() {
    let tmp = TempDir::new("owner-sock-ddl");
    store::init_dir(tmp.path()).expect("init");
    let node = serve::start_node(tmp.path(), true).await.expect("serve");

    // DDL through the socket succeeds (banned on the gated path).
    let mut conn = rsntr::owner_socket::connect(tmp.path())
        .await
        .expect("connect");
    let mut buf = BytesMut::new();
    let req = Request::new(
        RequestKind::Execute,
        "sql-sqlite",
        "CREATE TABLE seedlings (id INTEGER PRIMARY KEY, name TEXT)",
    );
    send_frame(&mut conn, &req.to_envelope()).await;
    loop {
        match read_frame(&mut conn, &mut buf).await {
            Some(EnvelopeObject::Result(_)) => {}
            Some(EnvelopeObject::Done(_)) => break,
            other => panic!("expected Result/Done, got {other:?}"),
        }
    }

    // A media query answers mod-unsupported on the socket (v1).
    let mut conn = rsntr::owner_socket::connect(tmp.path())
        .await
        .expect("connect");
    let mut buf = BytesMut::new();
    let req = Request::new(RequestKind::Query, "media", "cam");
    send_frame(&mut conn, &req.to_envelope()).await;
    match read_frame(&mut conn, &mut buf).await {
        Some(EnvelopeObject::Error(e)) => assert_eq!(e.code, "mod-unsupported"),
        other => panic!("expected Error, got {other:?}"),
    }

    // A non-request first frame answers protocol-error.
    let mut conn = rsntr::owner_socket::connect(tmp.path())
        .await
        .expect("connect");
    let mut buf = BytesMut::new();
    send_frame(
        &mut conn,
        &EnvelopeObject::Knock(resonator_protocol::Knock {
            id: None,
            message: "hello me".into(),
        }),
    )
    .await;
    match read_frame(&mut conn, &mut buf).await {
        Some(EnvelopeObject::Error(e)) => assert_eq!(e.code, "protocol-error"),
        other => panic!("expected Error, got {other:?}"),
    }

    node.shutdown().await;
}

#[tokio::test]
async fn stale_socket_is_replaced_on_restart() {
    let tmp = TempDir::new("owner-sock-stale");
    store::init_dir(tmp.path()).expect("init");
    let sock = tmp.path().join("rsntr.sock");

    // Simulate a crashed serve: a socket inode with nobody listening.
    // Bound through a short /tmp symlink to stay inside the sockaddr
    // path cap; the inode itself lands at <dir>/rsntr.sock.
    {
        let link = std::path::PathBuf::from(format!("/tmp/rsntr-staletest-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(tmp.path(), &link).expect("alias");
        let stale =
            std::os::unix::net::UnixListener::bind(link.join("rsntr.sock")).expect("stale bind");
        drop(stale);
        let _ = std::fs::remove_file(&link);
    }
    assert!(sock.exists(), "the stale inode persists");

    let node = serve::start_node(tmp.path(), true).await.expect("serve");
    assert!(sock.exists());

    // The rebound socket serves.
    let mut conn = rsntr::owner_socket::connect(tmp.path())
        .await
        .expect("connect");
    let mut buf = BytesMut::new();
    let req = Request::new(RequestKind::Query, "sql-sqlite", "SELECT 1 AS one");
    send_frame(&mut conn, &req.to_envelope()).await;
    let mut saw_done = false;
    while let Some(frame) = read_frame(&mut conn, &mut buf).await {
        if let EnvelopeObject::Done(_) = frame {
            saw_done = true;
            break;
        }
    }
    assert!(saw_done, "the replaced socket answers owner requests");

    node.shutdown().await;
}
