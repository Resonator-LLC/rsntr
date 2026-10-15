//! M2: after `shutdown()` a process holds nothing on the node directory:
//! no database handle (WAL locks), no blob store lock, no lease, and the
//! published serving keys are gone. What a suspended iOS app needs.

#![cfg(unix)]

use rsntr::testutil::TempDir;
use rsntr::{chat, lease, serve, store};

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_lets_go_of_every_handle() {
    let tmp = TempDir::new("release");
    store::init_dir(tmp.path()).expect("init");
    chat::chat_init(tmp.path()).expect("chat");
    let node = serve::start_node(tmp.path(), true).await.expect("serve");
    chat::chat_send_with(
        tmp.path(),
        rsntr::Prefer::Socket,
        "self-is-not-a-peer",
        "x",
        None,
    )
    .await
    .err();
    {
        let conn = store::open_db(tmp.path()).unwrap();
        assert!(resonator_node::get_rsntr(&conn, "serving_ticket").is_some());
    }
    node.shutdown().await;

    assert!(!lease::is_held(tmp.path()));
    let conn = store::open_db(tmp.path()).unwrap();
    assert!(resonator_node::get_rsntr(&conn, "serving_ticket").is_none());
    assert!(resonator_node::get_rsntr(&conn, "serving_addrs").is_none());
    drop(conn);

    // Linux: no fd of this process names the directory any more.
    #[cfg(target_os = "linux")]
    {
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let mut open: Vec<String> = Vec::new();
        for entry in std::fs::read_dir("/proc/self/fd").unwrap().flatten() {
            if let Ok(target) = std::fs::read_link(entry.path())
                && target.starts_with(&dir)
            {
                open.push(target.display().to_string());
            }
        }
        assert!(
            open.is_empty(),
            "handles still open after shutdown: {open:?}"
        );
    }

    // Switching the journal mode needs exclusive access: proof that no
    // connection anywhere in this process still has the file open.
    let conn = rusqlite::Connection::open(store::db_path(tmp.path())).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "delete");
    let mode: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
}
