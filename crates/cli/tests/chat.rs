//! Chat modulation integration (docs/chat-protocol.md): scaffold, offline
//! two-node direct send with vibration, idempotent resend, queue drain on
//! reconnection, watch, room fan-out, and file transfer over iroh-blobs.

use std::path::Path;
use std::time::{Duration, Instant};

use rsntr::testutil::TempDir;
use rsntr::{chat, client, serve, store};

/// Polls `f` on the node dir's database until it returns true.
fn wait_db(dir: &Path, what: &str, dur: Duration, f: impl Fn(&rusqlite::Connection) -> bool) {
    let deadline = Instant::now() + dur;
    loop {
        let conn = store::open_db(dir).expect("open db");
        if f(&conn) {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Async wrapper: same poll without blocking the runtime worker.
async fn wait_db_async(
    dir: &Path,
    what: &str,
    dur: Duration,
    f: impl Fn(&rusqlite::Connection) -> bool + Send + 'static,
) {
    let dir = dir.to_path_buf();
    let what = what.to_string();
    tokio::task::spawn_blocking(move || wait_db(&dir, &what, dur, f))
        .await
        .expect("wait task");
}

fn count(conn: &rusqlite::Connection, sql: &str, params: &[&str]) -> i64 {
    conn.query_row(sql, rusqlite::params_from_iter(params.iter()), |r| r.get(0))
        .unwrap_or(-1)
}

/// Admits `peer_hex` on a running node's db (a `_peers` row).
async fn admit(node: &serve::RunningNode, peer_hex: &str) {
    let peer = peer_hex.to_string();
    node.node()
        .db()
        .call(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO _peers (endpoint_id, added_at) \
                 VALUES (?1, datetime('now'))",
                [&peer],
            )
            .expect("admit peer");
        })
        .await
        .expect("db call");
}

#[test]
fn chat_init_scaffolds_schema_points_and_policy() {
    let tmp = TempDir::new("chat-init");
    store::init_dir(tmp.path()).expect("init");
    chat::chat_init(tmp.path()).expect("chat init");
    chat::chat_init(tmp.path()).expect("chat init is idempotent");

    let conn = store::open_db(tmp.path()).expect("open");
    for table in [
        "chat_messages",
        "chat_rooms",
        "chat_members",
        "_outbox",
        "_results",
    ] {
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                &[table],
            ),
            1,
            "missing table {table}"
        );
    }

    // The three projection points, once each.
    for (point, kind) in [
        ("urn:rsntr:chat:send", "excitable"),
        ("urn:rsntr:chat-inbox", "sympathetic"),
        ("urn:rsntr:chat:history", "radiant"),
    ] {
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM _projection WHERE point_iri = ?1 AND kind = ?2",
                &[point, kind],
            ),
            1,
            "missing point {point}"
        );
    }
    // The inbox point's resource doubles as vibration source and gate.
    let resource: String = conn
        .query_row(
            "SELECT resource FROM _projection WHERE point_iri = 'urn:rsntr:chat-inbox'",
            [],
            |r| r.get(0),
        )
        .expect("inbox resource");
    assert_eq!(resource, "chat_messages");

    // Policy: DMs open to admitted peers; self may read and entrain.
    let self_hex = store::node_id(tmp.path()).expect("id").to_string();
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM _policy WHERE peer_or_group = '*' \
             AND table_name = 'chat:direct' AND action = 'chat' AND effect = 'allow'",
            &[],
        ),
        1
    );
    for action in ["read", "entrain"] {
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM _policy WHERE peer_or_group = ?1 \
                 AND table_name = 'chat_messages' AND action = ?2 AND effect = 'allow'",
                &[self_hex.as_str(), action],
            ),
            1,
            "missing self {action} row"
        );
    }

    // The modulation is advertised.
    let mods = resonator_node::get_rsntr(&conn, "modulations").expect("mods");
    assert!(mods.contains("chat"), "got {mods}");
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_send_delivers_vibrates_watches_and_is_idempotent() {
    let ta = TempDir::new("chat-a");
    let tb = TempDir::new("chat-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    let b_id = store::init_dir(tb.path()).expect("init b");
    chat::chat_init(ta.path()).expect("chat init a");
    chat::chat_init(tb.path()).expect("chat init b");

    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    let ticket = b.ready_ticket(Duration::from_secs(3)).await;
    store::peer_add(ta.path(), "b", &ticket, &[]).expect("a learns b");
    admit(&b, &a_id.to_string()).await;

    // A serves too: its outbox worker is the sender.
    let a = serve::start_node(ta.path(), true).await.expect("serve a");

    // Subscribe to B's inbox point before anything is sent.
    let (sub, mut vibrations) = b.node().entrainments().subscribe(
        "urn:rsntr:chat-inbox".to_string(),
        Some("chat_messages".to_string()),
    );

    let report = chat::chat_send(ta.path(), "b", "lunch at noon?", None)
        .await
        .expect("send");
    assert_eq!(report.queued_to, vec![b_id.to_string()]);
    a.wake_outbox();

    // Delivery status joins by message id.
    let rid = report.message_id.clone();
    wait_db_async(
        ta.path(),
        "outbox done",
        Duration::from_secs(15),
        move |conn| {
            count(
                conn,
                "SELECT count(*) FROM _outbox WHERE request_id = ?1 AND status = 'done'",
                &[rid.as_str()],
            ) == 1
        },
    )
    .await;

    // The recipient's table has the message, sender assigned from the
    // transport-proven identity, scope = the sender.
    let (a_hex, rid) = (a_id.to_string(), report.message_id.clone());
    let (a_hex2, rid2) = (a_hex.clone(), rid.clone());
    wait_db_async(
        tb.path(),
        "b's chat row",
        Duration::from_secs(10),
        move |conn| {
            count(
                conn,
                "SELECT count(*) FROM chat_messages WHERE id = ?1 AND scope = ?2 \
             AND sender = ?2 AND body = 'lunch at noon?' AND outgoing = 0",
                &[rid2.as_str(), a_hex2.as_str()],
            ) == 1
        },
    )
    .await;

    // The append vibrated the inbox point.
    tokio::time::timeout(Duration::from_secs(5), vibrations.recv())
        .await
        .expect("vibration within deadline")
        .expect("subscription alive");
    b.node().entrainments().unsubscribe(sub);

    // Watch on B for the conversation with A: entrain over the wire,
    // then a second message lands and is emitted.
    let (wtx, mut wrx) = tokio::sync::mpsc::channel(16);
    let (damp_tx, damp_rx) = tokio::sync::oneshot::channel();
    let watch = tokio::spawn(chat::chat_watch(
        tb.path().to_path_buf(),
        a_hex.clone(),
        wtx,
        damp_rx,
    ));
    match tokio::time::timeout(Duration::from_secs(10), wrx.recv())
        .await
        .expect("watch event deadline")
    {
        Some(chat::WatchEvent::Entrained) => {}
        other => panic!("expected Entrained, got {other:?}"),
    }
    let second = chat::chat_send(ta.path(), "b", "second one", None)
        .await
        .expect("second send");
    a.wake_outbox();
    match tokio::time::timeout(Duration::from_secs(15), wrx.recv())
        .await
        .expect("watch message deadline")
    {
        Some(chat::WatchEvent::Message(entry)) => {
            assert_eq!(entry.id, second.message_id);
            assert_eq!(entry.body, "second one");
            assert_eq!(entry.sender, a_hex);
        }
        Some(other) => panic!("unexpected watch event {other:?}"),
        None => panic!("watch ended early"),
    }
    let _ = damp_tx.send(());
    match tokio::time::timeout(Duration::from_secs(5), wrx.recv())
        .await
        .expect("damp deadline")
    {
        Some(chat::WatchEvent::Damped) | None => {}
        other => panic!("expected Damped, got {other:?}"),
    }
    watch.await.expect("watch join").expect("watch clean exit");
    // The ephemeral watch admission is cleaned up.
    let conn = store::open_db(tb.path()).expect("open b");
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM _peers WHERE name = '_watch'",
            &[]
        ),
        0
    );
    drop(conn);

    // Idempotent resend: re-shipping the first request does not apply
    // twice (answered from _applied, still exactly one row at B).
    {
        let conn = store::open_db(ta.path()).expect("open a");
        conn.execute(
            "UPDATE _outbox SET status = 'sent' WHERE request_id = ?1",
            [&report.message_id],
        )
        .expect("reset row");
    }
    a.wake_outbox();
    let rid = report.message_id.clone();
    wait_db_async(
        ta.path(),
        "resend done",
        Duration::from_secs(15),
        move |conn| {
            count(
                conn,
                "SELECT count(*) FROM _outbox WHERE request_id = ?1 AND status = 'done'",
                &[rid.as_str()],
            ) == 1
        },
    )
    .await;
    let conn = store::open_db(tb.path()).expect("open b");
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM chat_messages WHERE id = ?1",
            &[report.message_id.as_str()],
        ),
        1,
        "resend must not duplicate"
    );
    // And the wire request is recorded as applied exactly once.
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM _applied WHERE request_id = ?1",
            &[report.message_id.as_str()],
        ),
        1
    );
    drop(conn);

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_queue_drains_when_peer_comes_up() {
    let ta = TempDir::new("drain-a");
    let tc = TempDir::new("drain-c");
    let a_id = store::init_dir(ta.path()).expect("init a");
    let c_id = store::init_dir(tc.path()).expect("init c");
    chat::chat_init(ta.path()).expect("chat init a");
    chat::chat_init(tc.path()).expect("chat init c");

    // A knows C by id only; C is down.
    store::peer_add(ta.path(), "c", &c_id.to_string(), &[]).expect("a learns c");
    let a = serve::start_node(ta.path(), true).await.expect("serve a");

    let report = chat::chat_send(ta.path(), "c", "are you there?", None)
        .await
        .expect("send queues");
    a.wake_outbox();

    // While C is down the row stays queued (dial failures back off).
    tokio::time::sleep(Duration::from_millis(1200)).await;
    {
        let conn = store::open_db(ta.path()).expect("open a");
        let status: String = conn
            .query_row(
                "SELECT status FROM _outbox WHERE request_id = ?1",
                [&report.message_id],
                |r| r.get(0),
            )
            .expect("row");
        assert_eq!(
            status, "queued",
            "undialable peer must leave the row queued"
        );
    }

    // C comes up and admits A; A learns the live addresses.
    let c = serve::start_node(tc.path(), true).await.expect("serve c");
    admit(&c, &a_id.to_string()).await;
    a.transport().add_peer_addrs(c_id, c.direct_addrs());
    a.wake_outbox();

    let rid = report.message_id.clone();
    wait_db_async(
        ta.path(),
        "drain done",
        Duration::from_secs(20),
        move |conn| {
            count(
                conn,
                "SELECT count(*) FROM _outbox WHERE request_id = ?1 AND status = 'done'",
                &[rid.as_str()],
            ) == 1
        },
    )
    .await;
    let rid = report.message_id.clone();
    wait_db_async(
        tc.path(),
        "c's chat row",
        Duration::from_secs(10),
        move |conn| {
            count(
                conn,
                "SELECT count(*) FROM chat_messages WHERE id = ?1 AND body = 'are you there?'",
                &[rid.as_str()],
            ) == 1
        },
    )
    .await;

    a.shutdown().await;
    c.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn room_fan_out_host_to_two_members() {
    let th = TempDir::new("room-h");
    let t1 = TempDir::new("room-m1");
    let t2 = TempDir::new("room-m2");
    let h_id = store::init_dir(th.path()).expect("init h");
    let m1_id = store::init_dir(t1.path()).expect("init m1");
    let m2_id = store::init_dir(t2.path()).expect("init m2");
    for dir in [th.path(), t1.path(), t2.path()] {
        chat::chat_init(dir).expect("chat init");
    }

    let h = serve::start_node(th.path(), true).await.expect("serve h");
    let m1 = serve::start_node(t1.path(), true).await.expect("serve m1");
    let m2 = serve::start_node(t2.path(), true).await.expect("serve m2");

    // Admissions and dial hints: members know the host, the host knows
    // the members.
    admit(&h, &m1_id.to_string()).await;
    admit(&h, &m2_id.to_string()).await;
    admit(&m1, &h_id.to_string()).await;
    admit(&m2, &h_id.to_string()).await;
    store::peer_add(t1.path(), "h", &h_id.to_string(), &[]).expect("m1 names h");
    m1.transport().add_peer_addrs(h_id, h.direct_addrs());
    h.transport().add_peer_addrs(m1_id, m1.direct_addrs());
    h.transport().add_peer_addrs(m2_id, m2.direct_addrs());

    // Host: create the room, admit both members.
    let room = chat::room_create(th.path(), "gardening").expect("room create");
    assert!(room.starts_with("urn:rsntr:room:"));
    chat::room_add(th.path(), "gardening", &m1_id.to_string()).expect("add m1");
    chat::room_add(th.path(), &room, &m2_id.to_string()).expect("add m2");

    // Member 1 binds the room and sends into it.
    chat::room_join(t1.path(), "h", &room, Some("gardening")).expect("join");
    let report = chat::chat_send(t1.path(), "gardening", "the seedlings came up", None)
        .await
        .expect("room send");
    assert_eq!(report.queued_to, vec![h_id.to_string()]);
    assert_eq!(report.scope, room);
    m1.wake_outbox();

    // The host's transcript is authoritative: scope = room, author = m1.
    let (rid, room_c, m1_hex) = (report.message_id.clone(), room.clone(), m1_id.to_string());
    wait_db_async(
        th.path(),
        "host row",
        Duration::from_secs(15),
        move |conn| {
            count(
                conn,
                "SELECT count(*) FROM chat_messages WHERE id = ?1 AND scope = ?2 AND sender = ?3",
                &[rid.as_str(), room_c.as_str(), m1_hex.as_str()],
            ) == 1
        },
    )
    .await;

    // Fan-out reaches member 2 with authorship preserved and the room
    // bound to the host by trust-on-first-use, labeled from the frame.
    let (rid, room_c, m1_hex) = (report.message_id.clone(), room.clone(), m1_id.to_string());
    wait_db_async(t2.path(), "m2 row", Duration::from_secs(20), move |conn| {
        count(
            conn,
            "SELECT count(*) FROM chat_messages WHERE id = ?1 AND scope = ?2 AND sender = ?3 \
             AND body = 'the seedlings came up' AND outgoing = 0",
            &[rid.as_str(), room_c.as_str(), m1_hex.as_str()],
        ) == 1
    })
    .await;
    {
        let conn = store::open_db(t2.path()).expect("open m2");
        let (host, name): (String, String) = conn
            .query_row(
                "SELECT host, name FROM chat_rooms WHERE room_id = ?1",
                [&room],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("m2 room binding");
        assert_eq!(host, h_id.to_string());
        assert_eq!(name, "gardening");
    }

    // The author is not echoed: m1 holds only its own outgoing copy.
    tokio::time::sleep(Duration::from_millis(500)).await;
    {
        let conn = store::open_db(t1.path()).expect("open m1");
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM chat_messages WHERE scope = ?1",
                &[room.as_str()],
            ),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM chat_messages WHERE scope = ?1 AND outgoing = 1",
                &[room.as_str()],
            ),
            1
        );
    }

    // chat log on the host joins nothing for a received message but
    // shows it; on m1 the outgoing row carries its delivery status.
    let log = chat::chat_log(t1.path(), &room, 10).expect("m1 log");
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].status.as_deref(), Some("done"));

    h.shutdown().await;
    m1.shutdown().await;
    m2.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn file_send_and_fetch_round_trip() {
    let ta = TempDir::new("file-a");
    let tb = TempDir::new("file-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    let b_id = store::init_dir(tb.path()).expect("init b");
    chat::chat_init(ta.path()).expect("chat init a");
    chat::chat_init(tb.path()).expect("chat init b");

    // ~300 KiB of patterned bytes.
    let data: Vec<u8> = (0u32..300 * 1024)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    let src = ta.path().join("seedlings.jpg");
    std::fs::write(&src, &data).expect("write source");

    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    let ticket = b.ready_ticket(Duration::from_secs(3)).await;
    store::peer_add(ta.path(), "b", &ticket, &[]).expect("a learns b");
    admit(&b, &a_id.to_string()).await;

    // Import + queue while A's node is down (the blob store is
    // exclusive; the serving node takes it over next).
    let report = chat::chat_send(ta.path(), "b", "here", Some(&src))
        .await
        .expect("file send");
    let (hash, bytes) = report.blob.clone().expect("blob attached");
    assert!(hash.starts_with("blake3:"));
    assert_eq!(bytes, data.len() as u64);

    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    a.wake_outbox();

    // The message lands with its BlobRef metadata; no bytes rode along.
    let (rid, hash_c) = (report.message_id.clone(), hash.clone());
    wait_db_async(
        tb.path(),
        "blob message",
        Duration::from_secs(15),
        move |conn| {
            count(
                conn,
                "SELECT count(*) FROM chat_messages WHERE id = ?1 AND blob_hash = ?2 \
             AND blob_name = 'seedlings.jpg' AND blob_type = 'image/jpeg'",
                &[rid.as_str(), hash_c.as_str()],
            ) == 1
        },
    )
    .await;
    {
        let conn = store::open_db(tb.path()).expect("open b");
        let stored_bytes: i64 = conn
            .query_row(
                "SELECT blob_bytes FROM chat_messages WHERE id = ?1",
                [&report.message_id],
                |r| r.get(0),
            )
            .expect("blob bytes");
        assert_eq!(stored_bytes, data.len() as i64);
    }

    // B fetches the bytes from A over iroh-blobs, hash-verified.
    store::peer_add(tb.path(), "a", &a_id.to_string(), &a.direct_addrs()).expect("b learns a");
    let out = tb.path().join("fetched.jpg");
    let outcome = client::run_fetch(
        tb.path(),
        "a",
        &hash,
        Some(&out),
        true,
        Duration::from_secs(20),
    )
    .await
    .expect("fetch");
    match outcome {
        client::FetchOutcome::Written { bytes, .. } => assert_eq!(bytes, data.len() as u64),
        other => panic!("expected Written, got {other:?}"),
    }
    assert_eq!(std::fs::read(&out).expect("read fetched"), data);

    // A stranger (not in A's _peers) is refused by the provider gate.
    let stranger = TempDir::new("file-stranger");
    store::init_dir(stranger.path()).expect("init stranger");
    store::peer_add(stranger.path(), "a", &a_id.to_string(), &a.direct_addrs())
        .expect("stranger learns a");
    let refused = client::run_fetch(
        stranger.path(),
        "a",
        &hash,
        None,
        true,
        Duration::from_secs(10),
    )
    .await;
    match refused {
        Ok(client::FetchOutcome::Unreachable(_)) | Err(_) => {}
        Ok(other) => panic!("stranger fetch must fail, got {other:?}"),
    }

    let _ = b_id;
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn peer_add_while_serving_is_dialable_without_restart() {
    let ta = TempDir::new("live-add-a");
    let tc = TempDir::new("live-add-c");
    let a_id = store::init_dir(ta.path()).expect("init a");
    let c_id = store::init_dir(tc.path()).expect("init c");
    chat::chat_init(ta.path()).expect("chat init a");
    chat::chat_init(tc.path()).expect("chat init c");

    let c = serve::start_node(tc.path(), true).await.expect("serve c");
    admit(&c, &a_id.to_string()).await;

    // A starts serving knowing C by id only: undialable offline.
    store::peer_add(ta.path(), "c", &c_id.to_string(), &[]).expect("a names c");
    let a = serve::start_node(ta.path(), true).await.expect("serve a");

    let report = chat::chat_send(ta.path(), "c", "ping", None)
        .await
        .expect("send queues");
    a.wake_outbox();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    {
        let conn = store::open_db(ta.path()).expect("open a");
        let status: String = conn
            .query_row(
                "SELECT status FROM _outbox WHERE request_id = ?1",
                [&report.message_id],
                |r| r.get(0),
            )
            .expect("row");
        assert_eq!(status, "queued", "no addresses yet: the row stays queued");
    }

    // `rsntr peer add` from outside the serving process, while A keeps
    // running: dial addresses resolve from _peers at dial time, so no
    // restart and no in-process add_peer_addrs crutch is needed.
    let ticket = c.ready_ticket(Duration::from_secs(3)).await;
    store::peer_add(ta.path(), "c", &ticket, &[]).expect("a learns c's addrs");
    a.wake_outbox();

    let rid = report.message_id.clone();
    wait_db_async(
        ta.path(),
        "delivery after live peer add",
        Duration::from_secs(20),
        move |conn| {
            count(
                conn,
                "SELECT count(*) FROM _outbox WHERE request_id = ?1 AND status = 'done'",
                &[rid.as_str()],
            ) == 1
        },
    )
    .await;
    let rid = report.message_id.clone();
    wait_db_async(tc.path(), "c's row", Duration::from_secs(10), move |conn| {
        count(
            conn,
            "SELECT count(*) FROM chat_messages WHERE id = ?1 AND body = 'ping'",
            &[rid.as_str()],
        ) == 1
    })
    .await;

    let _ = c_id;
    a.shutdown().await;
    c.shutdown().await;
}
