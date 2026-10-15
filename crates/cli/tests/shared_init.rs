//! M2: several apps initializing one empty shared directory at once
//! produce exactly one identity.

use std::process::Command;

use rsntr::store;
use rsntr::testutil::TempDir;

#[test]
fn concurrent_library_initializers_agree_on_one_identity() {
    let tmp = TempDir::new("init-lib");
    let dir = tmp.path().join("node");
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let d = dir.clone();
            std::thread::spawn(move || store::ensure_dir_initialized(&d).expect("ensure"))
        })
        .collect();
    let ids: Vec<String> = handles
        .into_iter()
        .map(|h| h.join().unwrap().to_string())
        .collect();
    assert!(ids.iter().all(|i| i == &ids[0]), "{ids:?}");
    let conn = store::open_db(&dir).expect("open");
    assert_eq!(
        resonator_node::get_rsntr(&conn, "endpoint_id").as_deref(),
        Some(ids[0].as_str())
    );
    // Reopening is a no-op with the same id.
    assert_eq!(
        store::ensure_dir_initialized(&dir).unwrap().to_string(),
        ids[0]
    );
}

#[test]
fn concurrent_binary_inits_yield_one_winner() {
    let tmp = TempDir::new("init-bin");
    let dir = tmp.path().join("node");
    let children: Vec<_> = (0..4)
        .map(|_| {
            Command::new(env!("CARGO_BIN_EXE_rsntr"))
                .args(["--json", "init"])
                .arg(&dir)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn init")
        })
        .collect();
    let outcomes: Vec<(i32, String)> = children
        .into_iter()
        .map(|c| {
            let out = c.wait_with_output().unwrap();
            (
                out.status.code().unwrap_or(-1),
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                ),
            )
        })
        .collect();
    let winners = outcomes.iter().filter(|(c, _)| *c == 0).count();
    assert_eq!(winners, 1, "{outcomes:?}");
    for (code, err) in &outcomes {
        if *code != 0 {
            assert!(err.contains("already initialized"), "{err}");
        }
    }
    let id = store::node_id(&dir).expect("key").to_string();
    let conn = store::open_db(&dir).expect("open");
    assert_eq!(
        resonator_node::get_rsntr(&conn, "endpoint_id").as_deref(),
        Some(id.as_str()),
        "the key and the database belong to one node"
    );
}
