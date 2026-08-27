//! Agent chat primitives: `chat wait` unblocks on a live send and times
//! out cleanly; `chat log --since` pages by cursor; an oversized body
//! auto-spills to a text blob and inlines back on the reader.

use std::time::Duration;

use rsntr::testutil::TempDir;
use rsntr::{chat, serve, store};

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

#[tokio::test(flavor = "multi_thread")]
async fn wait_unblocks_since_pages_and_spill_round_trips() {
    let ta = TempDir::new("wait-a");
    let tb = TempDir::new("wait-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    let _b_id = store::init_dir(tb.path()).expect("init b");
    chat::chat_init(ta.path()).expect("chat init a");
    chat::chat_init(tb.path()).expect("chat init b");

    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    let ticket = b.ready_ticket(Duration::from_secs(3)).await;
    store::peer_add(ta.path(), "b", &ticket, &[]).expect("a learns b");
    admit(&b, &a_id.to_string()).await;
    let a = serve::start_node(ta.path(), true).await.expect("serve a");

    // Timeout path first: nothing arrives in one second.
    let report = chat::chat_wait(tb.path(), None, Duration::from_secs(1))
        .await
        .expect("wait");
    assert!(report.timed_out);
    assert!(report.messages.is_empty());
    assert!(report.next_since.is_none());

    // Wait unblocks on a live send (any-scope form).
    let waiter = {
        let dir = tb.path().to_path_buf();
        tokio::spawn(async move { chat::chat_wait(&dir, None, Duration::from_secs(30)).await })
    };
    // Give the waiter time to entrain before the message lands.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let first = chat::chat_send(ta.path(), "b", "first", None)
        .await
        .expect("send first");
    a.wake_outbox();
    let report = tokio::time::timeout(Duration::from_secs(20), waiter)
        .await
        .expect("wait deadline")
        .expect("join")
        .expect("wait ok");
    assert!(!report.timed_out);
    assert_eq!(report.messages.len(), 1);
    assert_eq!(report.messages[0].body, "first");
    assert!(!report.messages[0].outgoing);
    assert_eq!(
        report.next_since.as_deref(),
        Some(first.message_id.as_str())
    );

    // A second message, then --since pages past the first.
    let second = chat::chat_send(ta.path(), "b", "second", None)
        .await
        .expect("send second");
    a.wake_outbox();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let entries = chat::chat_log(tb.path(), &a_id.to_string(), 50, None).expect("log");
        if entries.iter().any(|e| e.id == second.message_id) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "second not delivered");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let paged = chat::chat_log(
        tb.path(),
        &a_id.to_string(),
        50,
        Some(first.message_id.as_str()),
    )
    .expect("log --since");
    assert_eq!(paged.len(), 1, "only the newer message: {paged:?}");
    assert_eq!(paged[0].id, second.message_id);

    // Auto-spill: a body over the 64 KiB ceiling round-trips.
    let big = "resonate ".repeat(12_000); // ~108 KiB
    let spilled = chat::chat_send(ta.path(), "b", &big, None)
        .await
        .expect("send big");
    assert!(spilled.spilled);
    assert!(spilled.blob.is_some());
    a.wake_outbox();
    // Sender history keeps the full text locally.
    {
        let mine = chat::chat_log(ta.path(), "b", 5, None).expect("a log");
        let entry = mine
            .iter()
            .find(|e| e.id == spilled.message_id)
            .expect("own row");
        assert_eq!(entry.body, big);
    }
    // Receiver: preview + spill marker land...
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let entries = chat::chat_log(tb.path(), &a_id.to_string(), 5, None).expect("b log");
        if let Some(e) = entries.iter().find(|e| e.id == spilled.message_id) {
            assert!(e.body.len() < 2048, "wire body is a preview");
            assert!(e.body.contains("full text in the attachment"));
            assert_eq!(e.blob_name.as_deref(), Some(chat::SPILL_NAME));
            break;
        }
        assert!(std::time::Instant::now() < deadline, "spill not delivered");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // ...and inline_spilled fetches the full text back from the sender
    // and persists it.
    store::peer_add(tb.path(), "a", &a_id.to_string(), &a.direct_addrs()).expect("b learns a");
    let mut entries = chat::chat_log(tb.path(), &a_id.to_string(), 5, None).expect("b log");
    let skipped = chat::inline_spilled(tb.path(), &mut entries).await;
    assert!(skipped.is_empty(), "inline failed for {skipped:?}");
    let entry = entries
        .iter()
        .find(|e| e.id == spilled.message_id)
        .expect("row");
    assert_eq!(entry.body, big, "full text inlined");
    // Persisted: a fresh read has the full body with no marker left.
    let again = chat::chat_log(tb.path(), &a_id.to_string(), 5, None).expect("b log again");
    let entry = again
        .iter()
        .find(|e| e.id == spilled.message_id)
        .expect("row again");
    assert_eq!(entry.body, big);
    assert!(entry.blob_name.is_none());

    a.shutdown().await;
    b.shutdown().await;
}
