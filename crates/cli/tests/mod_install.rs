//! Remote wasm mod install/update/remove over two live offline nodes:
//! grant -> push -> hot invoke (no restart) -> remove; the caps-ceiling
//! and mod-install denials; and the `_modulations` direct-DML ban.

use std::path::PathBuf;
use std::time::Duration;

use rsntr::client::{self, QueryOutcome};
use rsntr::testutil::TempDir;
use rsntr::{modcmd, serve, store};

/// The camera-mod wasm (descriptor name "cameras", needs no caps).
fn camera_wasm_path() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/camera-mod/target/wasm32-unknown-unknown/release/camera_mod.wasm");
    p.exists().then_some(p)
}

async fn admit(node: &serve::RunningNode, peer_hex: &str) {
    let peer = peer_hex.to_string();
    node.node()
        .db()
        .call(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO _peers (endpoint_id, added_at) VALUES (?1, datetime('now'))",
                [&peer],
            )
            .expect("admit");
        })
        .await
        .expect("db call");
}

async fn grant(node: &serve::RunningNode, peer_hex: &str, table: &str, action: &str) {
    let (peer, table, action) = (peer_hex.to_string(), table.to_string(), action.to_string());
    node.node()
        .db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO _policy (peer_or_group, table_name, action, effect) \
                 VALUES (?1, ?2, ?3, 'allow')",
                [&peer, &table, &action],
            )
            .expect("grant");
        })
        .await
        .expect("db call");
}

#[tokio::test(flavor = "multi_thread")]
async fn push_install_invoke_hot_then_remove() {
    let Some(wasm) = camera_wasm_path() else {
        eprintln!("SKIPPED: camera_mod.wasm unavailable (build examples/camera-mod for wasm32)");
        return;
    };
    let ta = TempDir::new("mi-a");
    let tb = TempDir::new("mi-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    let b_id = store::init_dir(tb.path()).expect("init b");

    // B starts with zero mods — proves the first hot install works.
    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    let ticket_b = b.ready_ticket(Duration::from_secs(3)).await;
    let ticket_a = a.ready_ticket(Duration::from_secs(3)).await;
    store::peer_add(ta.path(), "b", &ticket_b, &[]).expect("a learns b");
    store::peer_add(tb.path(), "a", &ticket_a, &[]).expect("b learns a");
    // The target (B) fetches the wasm from A over iroh-blobs; in offline
    // mode its serving transport needs A's direct addresses.
    b.transport().add_peer_addrs(a_id, a.direct_addrs());
    a.transport().add_peer_addrs(b_id, b.direct_addrs());
    let a_hex = a_id.to_string();
    admit(&b, &a_hex).await; // for the blob gate + peer gate

    // Ungranted: pushing without a mod-install grant is denied (exit 2).
    let denied = modcmd::mod_push(
        ta.path(),
        "b",
        "cameras",
        &wasm,
        &[],
        None,
        None,
        None,
        true,
    )
    .await
    .expect("push completes the protocol");
    assert!(
        matches!(denied.outcome, QueryOutcome::Denied(_)),
        "expected denial, got {:?}",
        denied.outcome
    );

    // Grant install on `cameras` and invocation of the mod, then push.
    grant(&b, &a_hex, "cameras", "mod-install").await;
    grant(&b, &a_hex, "*", "mod:cameras").await;
    let installed = modcmd::mod_push(
        ta.path(),
        "b",
        "cameras",
        &wasm,
        &[],
        None,
        None,
        None,
        true,
    )
    .await
    .expect("push");
    match &installed.outcome {
        QueryOutcome::Rows { columns, rows, .. } => {
            assert!(columns.contains(&"name".to_string()));
            assert_eq!(rows.len(), 1, "install summary row");
        }
        other => panic!("expected the install summary, got {other:?}"),
    }

    // Hot invoke on B without any restart: the mod answers `hologram`
    // (config-free), and the transport hello now advertises `cameras`
    // (else the mod gate would fast-fail ModUnsupported). The hologram
    // streams as Generic frames closed by Done, i.e. an ok outcome.
    let invoked = client::run_query(ta.path(), "b", "cameras", "hologram", &[], true, None)
        .await
        .expect("invoke");
    assert!(
        matches!(
            invoked.outcome,
            QueryOutcome::Rows { .. } | QueryOutcome::Graph { .. }
        ),
        "expected the mod to answer (not mod-unsupported), got {:?}",
        invoked.outcome
    );

    // Remove it: the modulation stops being served.
    let removed = modcmd::mod_remove_remote(ta.path(), "b", "cameras", true)
        .await
        .expect("remove");
    assert!(matches!(removed.outcome, QueryOutcome::Rows { .. }));
    let after = client::run_query(ta.path(), "b", "cameras", "hologram", &[], true, None)
        .await
        .expect("invoke after removal");
    match &after.outcome {
        QueryOutcome::Failed(e) => assert_eq!(e.code, "mod-unsupported"),
        other => panic!("expected mod-unsupported after removal, got {other:?}"),
    }

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn caps_ceiling_and_modulations_ban() {
    let Some(wasm) = camera_wasm_path() else {
        eprintln!("SKIPPED: camera_mod.wasm unavailable");
        return;
    };
    let ta = TempDir::new("mi2-a");
    let tb = TempDir::new("mi2-b");
    let a_id = store::init_dir(ta.path()).expect("init a");
    store::init_dir(tb.path()).expect("init b");
    let b = serve::start_node(tb.path(), true).await.expect("serve b");
    let a = serve::start_node(ta.path(), true).await.expect("serve a");
    store::peer_add(
        ta.path(),
        "b",
        &b.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("a learns b");
    store::peer_add(
        tb.path(),
        "a",
        &a.ready_ticket(Duration::from_secs(3)).await,
        &[],
    )
    .expect("b learns a");
    let a_hex = a_id.to_string();
    admit(&b, &a_hex).await;

    // Caps ceiling: install granted, but the requested `db_read` cap has
    // no `mod-cap:db_read` allow — denied before any fetch.
    grant(&b, &a_hex, "cameras", "mod-install").await;
    let denied = modcmd::mod_push(
        ta.path(),
        "b",
        "cameras",
        &wasm,
        &["db_read".to_string()],
        None,
        None,
        None,
        true,
    )
    .await
    .expect("push completes");
    match &denied.outcome {
        QueryOutcome::Denied(d) => {
            assert!(
                d.reason.as_deref().unwrap_or_default().contains("db_read"),
                "denial names the missing cap: {:?}",
                d.reason
            );
        }
        other => panic!("expected caps-ceiling denial, got {other:?}"),
    }

    // The `_modulations` direct-DML ban: even a '*' write grant cannot
    // write the registry on the remote lane.
    grant(&b, &a_hex, "*", "write").await;
    let banned = client::run_query(
        ta.path(),
        "b",
        "sql-sqlite",
        "INSERT INTO _modulations (name, sha256) VALUES ('x', 'y')",
        &[],
        true,
        None,
    )
    .await
    .expect("insert completes the protocol");
    match &banned.outcome {
        QueryOutcome::Denied(d) => assert!(
            d.reason
                .as_deref()
                .unwrap_or_default()
                .contains("mod-install modulation"),
            "ban names the remedy: {:?}",
            d.reason
        ),
        other => panic!("expected the _modulations ban, got {other:?}"),
    }

    a.shutdown().await;
    b.shutdown().await;
}
