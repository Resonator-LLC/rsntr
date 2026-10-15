//! M4 rooms surfacing: `chat room list` / `chat room members` on the host
//! and on a member, plus the JSON shapes.

use std::time::Duration;

use rsntr::testutil::TempDir;
use rsntr::{chat, serve, store};

#[tokio::test(flavor = "multi_thread")]
async fn room_list_and_members_on_both_sides() {
    let th = TempDir::new("room-h");
    let tm = TempDir::new("room-m");
    let h_id = store::init_dir(th.path()).expect("init host");
    let m_id = store::init_dir(tm.path()).expect("init member");
    chat::chat_init(th.path()).expect("chat host");
    chat::chat_init(tm.path()).expect("chat member");

    // Rooms exist without any node serving (create/add/join are local).
    store::peer_add(th.path(), "mem", &m_id.to_string(), &[]).expect("host admits member");
    store::peer_add(tm.path(), "host", &h_id.to_string(), &[]).expect("member names host");
    let room = chat::room_create(th.path(), "garden").expect("create");
    chat::room_add(th.path(), "garden", "mem").expect("add member");
    chat::room_join(tm.path(), "host", &room, Some("garden")).expect("join");

    let hosted = chat::room_list(th.path()).expect("host list");
    assert_eq!(hosted.len(), 1);
    assert!(hosted[0].hosting);
    assert_eq!(hosted[0].member_count, 2, "host + member");
    assert_eq!(hosted[0].name, "garden");

    let (info, members) = chat::room_members(th.path(), "garden").expect("host members");
    assert!(info.hosting);
    let names: Vec<Option<String>> = members.iter().map(|m| m.name.clone()).collect();
    assert!(
        names.contains(&Some("mem".to_string())),
        "petname resolved: {names:?}"
    );
    assert!(
        members.iter().any(|m| m.member == h_id.to_string()),
        "host is a member"
    );

    let joined = chat::room_list(tm.path()).expect("member list");
    assert_eq!(joined.len(), 1);
    assert!(!joined[0].hosting);
    assert_eq!(joined[0].host, h_id.to_string());
    assert_eq!(joined[0].member_count, 0, "a member keeps no roster");
    let (info, members) = chat::room_members(tm.path(), &room).expect("member side");
    assert!(!info.hosting);
    assert!(members.is_empty());

    assert!(chat::room_members(th.path(), "no-such-room").is_err());

    // JSON shapes through the binary.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(["--json", "chat", "room", "list", "-d"])
        .arg(th.path())
        .output()
        .expect("run list");
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json");
    assert_eq!(v["rooms"][0]["hosting"], true, "{v}");
    assert_eq!(v["rooms"][0]["member_count"], 2);
    assert_eq!(v["rooms"][0]["room"], serde_json::json!(room));
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(["--json", "chat", "room", "members", "garden", "-d"])
        .arg(th.path())
        .output()
        .expect("run members");
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json");
    assert_eq!(v["members"].as_array().map(Vec::len), Some(2), "{v}");

    // And a serving host still answers the same (no surprises from
    // the socket path).
    let h = serve::start_node(th.path(), true)
        .await
        .expect("serve host");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        chat::room_list(th.path())
            .expect("list while serving")
            .len(),
        1
    );
    h.shutdown().await;
}
