//! Daemon event hooks: a configured command fires with the JSON event on
//! stdin when a chat message arrives or an `_inbox` row parks; own sends
//! stay silent; a hanging hook is killed at the timeout.

#![cfg(unix)]

use std::path::Path;
use std::time::{Duration, Instant};

use rsntr::testutil::TempDir;
use rsntr::{Prefer, chat, hooks, serve, store};

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

async fn wait_file(path: &Path, what: &str, dur: Duration, f: impl Fn(&str) -> bool) {
    let deadline = Instant::now() + dur;
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if f(&text) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; file so far: {text:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn hooks_fire_on_message_and_inbox_and_time_out() {
    let ta = TempDir::new("hooks-a");
    let tb = TempDir::new("hooks-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    let _b_id = store::init_dir(tb.path()).expect("init b");
    chat::chat_init(ta.path()).expect("chat init a");
    chat::chat_init(tb.path()).expect("chat init b");

    // Hooks are configured before the daemon starts (the runner loads at
    // start and reloads on _hooks changes; both paths are exercised).
    let events = tb.path().join("events.jsonl");
    let id = hooks::hook_add(
        tb.path(),
        Prefer::Local,
        "*",
        &format!("cat >> '{}'", events.display()),
    )
    .await
    .expect("hook add");
    assert!(id > 0);
    let listed = hooks::hook_list(tb.path(), Prefer::Local)
        .await
        .expect("list");
    assert_eq!(listed.len(), 1);
    assert!(listed[0].enabled);

    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    let ticket = b.ready_ticket(Duration::from_secs(3)).await;
    store::peer_add(ta.path(), "b", &ticket, &[]).expect("a learns b");
    admit(&b, &a_id.to_string()).await;
    let a = serve::start_node(ta.path(), true).await.expect("serve a");

    // An incoming message fires the hook with the event JSON on stdin.
    chat::chat_send(ta.path(), "b", "wake up", None)
        .await
        .expect("send");
    a.wake_outbox();
    wait_file(&events, "message event", Duration::from_secs(20), |text| {
        text.contains("\"event\":\"message\"") && text.contains("wake up")
    })
    .await;
    {
        let text = std::fs::read_to_string(&events).expect("events");
        let event: serde_json::Value =
            serde_json::from_str(text.lines().next().expect("one line")).expect("json event");
        assert_eq!(event["from"], a_id.to_string());
        assert_eq!(event["body"], "wake up");
    }

    // A parked inbox row fires the inbox event (committed on the serving
    // connection, as a real knock escalation would be).
    b.node()
        .db()
        .call(|conn| {
            conn.execute(
                "INSERT INTO _inbox (request_id, peer, sql, params, received_at) \
                 VALUES ('01HOOKTEST', ?1, '', 'let me in', datetime('now'))",
                [&"cd".repeat(32)],
            )
            .expect("park a knock");
        })
        .await
        .expect("db call");
    wait_file(&events, "inbox event", Duration::from_secs(10), |text| {
        text.contains("\"event\":\"inbox\"") && text.contains("let me in")
    })
    .await;

    // Own sends never fire hooks: B messages A, nothing new lands in the
    // event file (the next assertion counts lines before and after).
    let lines_before = std::fs::read_to_string(&events)
        .expect("events")
        .lines()
        .count();
    store::peer_add(tb.path(), "a", &a_id.to_string(), &a.direct_addrs()).expect("b learns a");
    admit(&a, &b.peer_id().to_string()).await;
    chat::chat_send(tb.path(), "a", "outgoing is silent", None)
        .await
        .expect("b sends");
    b.wake_outbox();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let lines_after = std::fs::read_to_string(&events)
        .expect("events")
        .lines()
        .count();
    assert_eq!(
        lines_before, lines_after,
        "an outgoing send must not fire the message hook"
    );

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn hanging_hook_is_killed_at_the_timeout() {
    // A short timeout via the env override; the runner must reap the
    // hook and stay alive for the next event.
    // SAFETY: test-process env, set before any hook runs.
    unsafe { std::env::set_var("RSNTR_HOOK_TIMEOUT_MS", "500") };

    let tb = TempDir::new("hooks-hang");
    store::init_dir(tb.path()).expect("init");
    chat::chat_init(tb.path()).expect("chat init");
    let marker = tb.path().join("ran");
    hooks::hook_add(
        tb.path(),
        Prefer::Local,
        "inbox",
        &format!("echo x >> '{}'; sleep 600", marker.display()),
    )
    .await
    .expect("hook add");

    let b = serve::start_node(tb.path(), true).await.expect("serve");
    b.node()
        .db()
        .call(|conn| {
            conn.execute(
                "INSERT INTO _inbox (request_id, peer, sql, params, received_at) \
                 VALUES ('01HANG', ?1, '', 'x', datetime('now'))",
                [&"ee".repeat(32)],
            )
            .expect("park");
        })
        .await
        .expect("db call");
    wait_file(&marker, "first hook run", Duration::from_secs(10), |text| {
        text.lines().count() == 1
    })
    .await;

    // A second event goes through promptly, proving the runner killed
    // the sleeping hook at the timeout instead of wedging for 600s.
    b.node()
        .db()
        .call(|conn| {
            conn.execute(
                "INSERT INTO _inbox (request_id, peer, sql, params, received_at) \
                 VALUES ('01HANG2', ?1, '', 'y', datetime('now'))",
                [&"ef".repeat(32)],
            )
            .expect("park 2");
        })
        .await
        .expect("db call");
    wait_file(
        &marker,
        "second hook run",
        Duration::from_secs(15),
        |text| text.lines().count() == 2,
    )
    .await;
    // Let the second run hit its timeout so the runner sweeps its group
    // before the test (and the runtime) goes away.
    tokio::time::sleep(Duration::from_millis(800)).await;

    b.shutdown().await;
}
