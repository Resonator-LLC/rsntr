//! M7: `rsntr install-service` prints a unit and a recipe without
//! writing anything; `chat init` creates the `chat_log_v` view on fresh
//! and legacy scaffolds alike.

use rsntr::testutil::TempDir;
use rsntr::{chat, store};

#[test]
fn install_service_prints_only() {
    let tmp = TempDir::new("svc");
    store::init_dir(tmp.path()).expect("init");
    let before: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(["--json", "install-service", "--user"])
        .arg(tmp.path())
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json");
    let unit = v["unit"].as_str().expect("unit");
    assert!(
        unit.contains("[Service]") && unit.contains("--foreground"),
        "{unit}"
    );
    assert!(unit.contains("WantedBy=default.target"));
    assert!(
        v["recipe"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.as_str().unwrap().contains("enable-linger")),
        "{v}"
    );
    assert!(
        v["unit_path"]
            .as_str()
            .unwrap()
            .contains(".config/systemd/user")
    );
    // Plain mode: the unit on stdout, the recipe on stderr.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(["install-service"])
        .arg(tmp.path())
        .output()
        .expect("run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("[Unit]"), "{stdout}");
    assert!(stdout.contains("WantedBy=multi-user.target"));
    assert!(String::from_utf8_lossy(&out.stderr).contains("systemctl daemon-reload"));
    let after: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(before, after, "nothing written into the node directory");
}

#[test]
fn chat_log_v_on_fresh_and_legacy_scaffolds() {
    // Fresh.
    let fresh = TempDir::new("view-fresh");
    store::init_dir(fresh.path()).expect("init");
    chat::chat_init(fresh.path()).expect("chat init");
    let conn = store::open_db(fresh.path()).expect("open");
    let cols: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('chat_log_v')")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for want in [
        "id",
        "body",
        "re_id",
        "tags",
        "sender_name",
        "delivery_status",
        "delivered_at",
    ] {
        assert!(
            cols.iter().any(|c| c == want),
            "{want} missing from {cols:?}"
        );
    }
    drop(conn);

    // Legacy: a pre-threading chat_messages, then chat init again.
    let legacy = TempDir::new("view-legacy");
    store::init_dir(legacy.path()).expect("init");
    {
        let conn = store::open_db(legacy.path()).expect("open");
        conn.execute_batch(
            "CREATE TABLE chat_messages (id TEXT PRIMARY KEY, scope TEXT NOT NULL, \
             sender TEXT NOT NULL, at TEXT NOT NULL, \
             received_at TEXT NOT NULL DEFAULT (datetime('now')), body TEXT NOT NULL, \
             blob_hash TEXT, blob_bytes INTEGER, blob_name TEXT, blob_type TEXT, \
             outgoing INTEGER NOT NULL DEFAULT 0); \
             INSERT INTO chat_messages (id, scope, sender, at, body) \
             VALUES ('01OLD', 'x', 'y', '2026-01-01T00:00:00Z', 'old row');",
        )
        .expect("legacy schema");
    }
    chat::chat_init(legacy.path()).expect("re-scaffold");
    let conn = store::open_db(legacy.path()).expect("open");
    let (body, re, name): (String, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT body, re_id, sender_name FROM chat_log_v WHERE id = '01OLD'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("view reads the legacy row with the new columns");
    assert_eq!(body, "old row");
    assert!(re.is_none() && name.is_none());
    let comment: String = conn
        .query_row(
            "SELECT comment FROM _comments WHERE name = 'chat_log_v'",
            [],
            |r| r.get(0),
        )
        .expect("described");
    assert!(comment.contains("view"));
    // Idempotent.
    chat::chat_init(legacy.path()).expect("third run");
}
