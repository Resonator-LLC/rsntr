//! M5 threading + tags: `--re`/`--tag` ride the wire as rsntr:re /
//! rsntr:tag, land in the peer's chat_messages, filter `chat log`, and
//! show up in events (hooks/await) and `--json`.

use std::time::Duration;

use rsntr::chat::{LogFilter, SendOptions};
use rsntr::testutil::TempDir;
use rsntr::{chat, events, serve, store};

async fn wait_for_message(dir: &std::path::Path, scope: &str, id: &str) -> chat::LogEntry {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(log) = chat::chat_log(dir, scope, 10, None)
            && let Some(e) = log.into_iter().find(|e| e.id == id)
        {
            return e;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "message {id} never arrived"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn re_and_tags_travel_filter_and_surface() {
    let ta = TempDir::new("thr-a");
    let tb = TempDir::new("thr-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    let b_id = store::init_dir(tb.path()).expect("init b");
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

    // The opener, then a tagged reply threading on it.
    let opener = chat::chat_send(ta.path(), "b", "lunch?", None)
        .await
        .expect("send opener");
    a.wake_outbox();
    let reply = chat::chat_send_opts(
        ta.path(),
        rsntr::Prefer::Auto,
        "b",
        "noon works",
        None,
        &SendOptions {
            re: Some(opener.message_id.clone()),
            tags: vec!["topic:lunch".into(), "ack".into(), "topic:lunch".into()],
        },
    )
    .await
    .expect("send reply");
    a.wake_outbox();

    // Sender side: stored normalized.
    let own = chat::chat_log(ta.path(), "b", 10, None).expect("log a");
    let own_reply = own
        .iter()
        .find(|e| e.id == reply.message_id)
        .expect("own row");
    assert_eq!(own_reply.re.as_deref(), Some(opener.message_id.as_str()));
    assert_eq!(
        own_reply.tags,
        vec!["ack".to_string(), "topic:lunch".to_string()]
    );

    // Receiver side: the wire carried both.
    let got = wait_for_message(tb.path(), &a_id.to_string(), &reply.message_id).await;
    assert_eq!(got.re.as_deref(), Some(opener.message_id.as_str()));
    assert_eq!(got.tags, vec!["ack".to_string(), "topic:lunch".to_string()]);

    // Filters: by thread, by tag, and a tag nobody used.
    let a_hex = a_id.to_string();
    let by_re = chat::chat_log_filtered(
        tb.path(),
        &a_hex,
        10,
        None,
        &LogFilter {
            re: Some(opener.message_id.clone()),
            tag: None,
        },
    )
    .expect("by re");
    assert_eq!(by_re.len(), 1);
    assert_eq!(by_re[0].id, reply.message_id);
    let by_tag = chat::chat_log_filtered(
        tb.path(),
        &a_hex,
        10,
        None,
        &LogFilter {
            re: None,
            tag: Some("ack".into()),
        },
    )
    .expect("by tag");
    assert_eq!(by_tag.len(), 1);
    let none = chat::chat_log_filtered(
        tb.path(),
        &a_hex,
        10,
        None,
        &LogFilter {
            re: None,
            tag: Some("nope".into()),
        },
    )
    .expect("no such tag");
    assert!(none.is_empty());

    // Events (hooks/await vocabulary) carry them.
    let (_cursor, evs) =
        events::new_message_events(tb.path(), 0, &b_id.to_string()).expect("events");
    let ev = evs
        .iter()
        .find(|e| e.id == reply.message_id)
        .expect("event");
    assert_eq!(ev.re.as_deref(), Some(opener.message_id.as_str()));
    let j = events::message_json(ev);
    assert_eq!(j["tags"], serde_json::json!(["ack", "topic:lunch"]));

    // The CLI: --json chat log --tag on b, and --re on send from b.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args([
            "--json",
            "chat",
            "log",
            &a_hex,
            "--tag",
            "topic:lunch",
            "-d",
        ])
        .arg(tb.path())
        .output()
        .expect("run log");
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json");
    assert_eq!(v["messages"].as_array().map(Vec::len), Some(1), "{v}");
    assert_eq!(v["messages"][0]["re"], serde_json::json!(opener.message_id));

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args([
            "--json",
            "chat",
            "send",
            "a",
            "see you there",
            "--re",
            &reply.message_id,
            "--tag",
            "ack",
            "-d",
        ])
        .arg(tb.path())
        .output()
        .expect("run send");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json");
    assert_eq!(v["re"], serde_json::json!(reply.message_id));
    b.wake_outbox();
    let back = wait_for_message(ta.path(), "b", v["message_id"].as_str().unwrap()).await;
    assert_eq!(back.re.as_deref(), Some(reply.message_id.as_str()));
    assert_eq!(back.tags, vec!["ack".to_string()]);

    // Caps are enforced before anything is written.
    let err = chat::chat_send_opts(
        ta.path(),
        rsntr::Prefer::Auto,
        "b",
        "x",
        None,
        &SendOptions {
            re: Some("".into()),
            tags: vec![],
        },
    )
    .await
    .expect_err("empty re");
    assert!(err.to_string().contains("rsntr:re"), "{err}");

    a.shutdown().await;
    b.shutdown().await;
}
