//! Two-node offline test of the extism mods host: register + enable the
//! time mod on the serving node, then `rsntr query --mod time` from the
//! client node over the real transport (which also proves the hello
//! advertises the wasm mod, or the transport gate would fast-fail).
//!
//! Skips when the time-mod wasm cannot be built and no prior artifact
//! exists.

#![cfg(feature = "mods")]

use std::path::PathBuf;

use resonator_protocol::Value;
use rsntr::client::{self, QueryOutcome};
use rsntr::testutil::TempDir;
use rsntr::{serve, store};

fn time_wasm() -> Option<Vec<u8>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/time-mod");
    let artifact = root.join("target/wasm32-unknown-unknown/release/time_mod.wasm");
    let build = std::process::Command::new("cargo")
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
        .current_dir(&root)
        .output();
    match build {
        Ok(out) if out.status.success() => {}
        Ok(out) => eprintln!(
            "time-mod build failed (falling back to any existing artifact):\n{}",
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(e) => eprintln!("time-mod build could not run: {e}"),
    }
    std::fs::read(&artifact).ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn time_mod_over_the_wire() {
    let Some(wasm) = time_wasm() else {
        eprintln!("SKIPPED: time_mod.wasm unavailable (wasm32-unknown-unknown target missing?)");
        return;
    };

    let ta = TempDir::new("mod-time-a");
    let tb = TempDir::new("mod-time-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    store::init_dir(tb.path()).expect("init b");

    // Register + enable the time mod on B before serving (the registry
    // loads at start).
    {
        let conn = store::open_db(tb.path()).expect("open b");
        resonator_mods::mod_add(&conn, "time", &wasm, &["clock".to_string()], None)
            .expect("mod add");
        assert!(resonator_mods::mod_set_enabled(&conn, "time", true).expect("enable"));
    }

    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    let hello = b.node().hello().await.expect("hello");
    assert!(hello.mods.iter().any(|m| m == "time"), "{:?}", hello.mods);

    let ticket = b.ready_ticket(std::time::Duration::from_secs(3)).await;
    store::peer_add(ta.path(), "b", &ticket, &[]).expect("peer add");

    // B admits A and allows the mod:time action.
    let a_hex = a_id.to_string();
    b.node()
        .db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO _peers (endpoint_id, added_at) VALUES (?1, datetime('now'))",
                [&a_hex],
            )
            .expect("admit a");
            conn.execute(
                "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
                 VALUES (?1, '*', 'mod:time', 'allow')",
                [&a_hex],
            )
            .expect("allow mod:time");
        })
        .await
        .expect("db call");

    let report = client::run_query(ta.path(), "b", "time", "now", &[], true, None)
        .await
        .expect("query");
    match &report.outcome {
        QueryOutcome::Rows {
            columns,
            rows,
            done,
        } => {
            assert_eq!(columns, &["now"]);
            assert_eq!(rows.len(), 1);
            assert_eq!(done.row_count, Some(1));
            let Some((_, Value::Text(ts))) = rows[0].cells.iter().find(|(n, _)| n == "now") else {
                panic!("no text 'now' cell in {:?}", rows[0]);
            };
            assert_eq!(ts.len(), 30, "{ts}");
            assert!(ts.ends_with('Z'), "{ts}");
        }
        other => panic!("expected rows, got {other:?}"),
    }

    // An unrelated mod still fast-fails at the transport gate.
    let missing = client::run_query(ta.path(), "b", "weather", "now", &[], true, None)
        .await
        .expect("query");
    match &missing.outcome {
        QueryOutcome::Failed(e) => assert_eq!(e.code, "mod-unsupported"),
        other => panic!("expected mod-unsupported, got {other:?}"),
    }

    b.shutdown().await;
}
