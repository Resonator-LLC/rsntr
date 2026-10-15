//! M6: the directory `rsntr` uses when -d is omitted: $RSNTR_NODE, the
//! current directory when it holds rsntr.db, else the per-user default.

use std::process::Command;

use rsntr::store;
use rsntr::testutil::TempDir;

fn node_dir(
    cwd: &std::path::Path,
    env: Option<&std::path::Path>,
    home: &std::path::Path,
) -> serde_json::Value {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rsntr"));
    cmd.args(["--json", "node", "dir"])
        .current_dir(cwd)
        .env_remove("RSNTR_NODE")
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("xdg"));
    if let Some(e) = env {
        cmd.env("RSNTR_NODE", e);
    }
    let out = cmd.output().expect("run");
    serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json")
}

#[test]
fn resolution_order_env_cwd_default() {
    let tmp = TempDir::new("defdir");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let plain = tmp.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();

    // Nothing around: the per-user default under XDG_DATA_HOME.
    let v = node_dir(&plain, None, &home);
    assert_eq!(v["source"], "default", "{v}");
    assert_eq!(v["dir"], home.join("xdg/rsntr/node").display().to_string());
    assert_eq!(v["initialized"], false);

    // A directory holding rsntr.db wins over the default.
    let here = tmp.path().join("here");
    store::init_dir(&here).unwrap();
    let v = node_dir(&here, None, &home);
    assert_eq!(v["source"], "cwd", "{v}");
    assert_eq!(v["initialized"], true);

    // The env var wins over both.
    let elsewhere = tmp.path().join("elsewhere");
    let v = node_dir(&here, Some(&elsewhere), &home);
    assert_eq!(v["source"], "env", "{v}");
    assert_eq!(v["dir"], elsewhere.display().to_string());
    assert_eq!(v["initialized"], false);

    // The library agrees with the binary.
    assert!(
        store::default_node_dir().ends_with("rsntr/node")
            || store::default_node_dir() == std::path::Path::new(".")
    );
}

#[test]
fn bare_serve_initializes_the_default_node() {
    let tmp = TempDir::new("defserve");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(["--json", "serve", "--offline"])
        .current_dir(tmp.path())
        .env_remove("RSNTR_NODE")
        .env("HOME", &home)
        .env("XDG_DATA_HOME", home.join("xdg"))
        .output()
        .expect("serve");
    let v: serde_json::Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stderr)));
    let pid = v["pid"].as_i64().expect("pid") as i32;
    let dir = home.join("xdg/rsntr/node");
    assert!(
        store::db_path(&dir).exists(),
        "initialized at the default: {v}"
    );
    let _ = Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(["--json", "stop"])
        .arg(&dir)
        .output();
    unsafe { libc::kill(pid, libc::SIGKILL) };
}
