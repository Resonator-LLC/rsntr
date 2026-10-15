//! M7 chaos: one serving process, four foreign writer processes hammering
//! an app table, a peer sending messages, a knocker, an `rsntr await`
//! consumer, and an entrainment on the app table, all on one directory.
//! Nothing may report busy/locked/snapshot; every message and knock must
//! reach the awaiter at least once; the app-table entrainment must see
//! the writers; the database must pass integrity_check.

#![cfg(unix)]

use std::process::Command;
use std::time::{Duration, Instant};

use resonator_protocol::{Entrain, EnvelopeObject};
use resonator_transport::RequestStream;
use rsntr::testutil::TempDir;
use rsntr::{chat, client, owner, serve, store};

fn rsntr_json(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .arg("--json")
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
async fn many_processes_one_directory() {
    let ta = TempDir::new("chaos-a");
    let tb = TempDir::new("chaos-b");
    let tk = TempDir::new("chaos-k");
    let a_id = store::init_dir(ta.path()).expect("init a");
    let b_id = store::init_dir(tb.path()).expect("init b");
    store::init_dir(tk.path()).expect("init k");
    chat::chat_init(ta.path()).expect("chat a");
    chat::chat_init(tb.path()).expect("chat b");
    {
        let conn = store::open_db(ta.path()).unwrap();
        conn.execute_batch(
            "CREATE TABLE chaos_events (id INTEGER PRIMARY KEY, writer TEXT, n INTEGER); \
             INSERT INTO _projection (point_iri, kind, label, resource) \
             VALUES ('urn:chaos:events', 'sympathetic', 'events', 'chaos_events');",
        )
        .unwrap();
    }
    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    let ticket = a.ready_ticket(Duration::from_secs(3)).await;
    store::peer_add(ta.path(), "b", &b_id.to_string(), &[]).expect("a admits b");
    store::peer_add(tb.path(), "a", &ticket, &[]).expect("b names a");
    store::peer_add(tk.path(), "a", &ticket, &[]).expect("k names a");
    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    let a_dir = ta.path().to_str().unwrap().to_string();

    // The entrainment on the app table, over the owner channel.
    let mut watcher = owner::connect(ta.path()).await.expect("connect");
    watcher
        .send(&EnvelopeObject::Entrain(Entrain {
            id: "01CHAOSENTRAIN000000000001".into(),
            point: "urn:chaos:events".into(),
        }))
        .await
        .unwrap();
    assert!(matches!(
        watcher.recv().await.unwrap(),
        Some(EnvelopeObject::Done(_))
    ));

    // The awaiter: one process per event, re-armed until quiet.
    let cursor = ta.path().join("agent.cursor");
    let awaiter = {
        let (d, c) = (a_dir.clone(), cursor.to_str().unwrap().to_string());
        std::thread::spawn(move || {
            let mut events: Vec<serde_json::Value> = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(60);
            let mut quiet = 0;
            while Instant::now() < deadline && quiet < 3 {
                let (code, out, err) =
                    rsntr_json(&["await", "-d", &d, "--cursor-file", &c, "--timeout", "3"]);
                match code {
                    0 => {
                        quiet = 0;
                        events.push(serde_json::from_str(&out).unwrap());
                    }
                    3 => quiet += 1,
                    other => panic!("await exit {other}: {err}"),
                }
            }
            events
        })
    };
    tokio::time::sleep(Duration::from_millis(800)).await;

    // A knock parks in a's inbox.
    let knock = client::knock(tk.path(), "a", "chaos knock", true)
        .await
        .expect("knock");
    let knock_id = knock.decision.and_then(|d| d.id).expect("pending id");

    // Four foreign writer processes (in-process pipelines, never the
    // channel), 60 inserts each from a seed file.
    let writers: Vec<_> = (0..4)
        .map(|w| {
            let d = a_dir.clone();
            let seed = ta.path().join(format!("seed-{w}.sql"));
            let sql: String = (0..60)
                .map(|i| format!("INSERT INTO chaos_events (writer, n) VALUES ('w{w}', {i});\n"))
                .collect();
            std::fs::write(&seed, sql).unwrap();
            std::thread::spawn(move || {
                let out = Command::new(env!("CARGO_BIN_EXE_rsntr"))
                    .args(["--json", "--local", "sql", &d, "--file"])
                    .arg(&seed)
                    .output()
                    .expect("writer");
                (
                    out.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&out.stderr).to_string(),
                )
            })
        })
        .collect();

    // B sends messages to a while the writers run.
    let mut sent = Vec::new();
    for i in 0..15 {
        let r = chat::chat_send(tb.path(), "a", &format!("chaos {i}"), None)
            .await
            .expect("send");
        sent.push(r.message_id);
        b.wake_outbox();
    }

    for w in writers {
        let (code, err) = w.join().unwrap();
        assert_eq!(code, 0, "writer failed: {err}");
        let lower = err.to_lowercase();
        assert!(
            !(lower.contains("locked") || lower.contains("busy") || lower.contains("snapshot")),
            "{err}"
        );
    }

    // The entrainment saw the writers (coarse wakes are fine; zero is not).
    let mut vibrations = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while let Ok(Ok(Some(EnvelopeObject::Vibration(_)))) =
        tokio::time::timeout_at(deadline, watcher.recv()).await
    {
        vibrations += 1;
        if vibrations >= 3 {
            break;
        }
    }
    assert!(
        vibrations >= 1,
        "no vibration reached the app-table entrainment"
    );

    let events = awaiter.join().unwrap();
    let ids: Vec<String> = events
        .iter()
        .filter_map(|e| e["id"].as_str().map(str::to_string))
        .collect();
    for id in &sent {
        assert!(
            ids.contains(id),
            "message {id} never reached the awaiter: {ids:?}"
        );
    }
    assert!(
        ids.contains(&knock_id),
        "the knock never reached the awaiter"
    );

    let conn = store::open_db(ta.path()).unwrap();
    let n: i64 = conn
        .query_row("SELECT count(*) FROM chaos_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 240);
    let ok: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
    let _ = a_id;
    a.shutdown().await;
    b.shutdown().await;
}
