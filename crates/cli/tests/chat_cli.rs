//! CLI-boundary chat checks over the real binary: `--body-file` carries
//! bodies past the OS argv cap (auto-spilling on the way), and `sql`
//! reports SELECT rows in its JSON.

use std::process::{Command, Stdio};

use rsntr::store;
use rsntr::testutil::TempDir;

fn rsntr_json(args: &[&str], stdin: Option<&[u8]>) -> (i32, serde_json::Value) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rsntr"));
    cmd.arg("--json").args(args);
    let out = match stdin {
        Some(bytes) => {
            use std::io::Write;
            let mut child = cmd
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn rsntr");
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(bytes)
                .expect("write stdin");
            child.wait_with_output().expect("output")
        }
        None => cmd.output().expect("running rsntr"),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "not one JSON object ({e}); stdout {stdout:?} stderr {:?}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out.status.code().unwrap_or(-1), json)
}

#[test]
fn body_file_and_stdin_carry_big_bodies_and_sql_reports_rows() {
    let tmp = TempDir::new("chat-cli");
    let dir = tmp.path().to_str().expect("utf8");
    let (code, _) = rsntr_json(&["init", dir], None);
    assert_eq!(code, 0);
    // A queueable target: any admitted peer id works, no daemon needed.
    let peer_hex = "ab".repeat(32);
    let (code, _) = rsntr_json(&["peer", "add", "buddy", &peer_hex, "-d", dir], None);
    assert_eq!(code, 0);

    // 300 KiB body — far past MAX_ARG_STRLEN — via --body-file.
    let big = "wfbody ".repeat(44_000);
    let body_path = tmp.path().join("body.txt");
    std::fs::write(&body_path, &big).expect("write body");
    let (code, sent) = rsntr_json(
        &[
            "chat",
            "send",
            "buddy",
            "--body-file",
            body_path.to_str().expect("utf8"),
            "-d",
            dir,
        ],
        None,
    );
    assert_eq!(code, 0, "{sent}");
    assert_eq!(sent["ok"], true);
    assert_eq!(sent["spilled"], true, "{sent}");

    // The local log holds the full body.
    let entries = rsntr::chat::chat_log(tmp.path(), "buddy", 5, None).expect("log");
    let entry = entries
        .iter()
        .find(|e| e.id == sent["message_id"].as_str().unwrap())
        .expect("own row");
    assert_eq!(entry.body, big);

    // `--body-file -` reads stdin.
    let (code, sent) = rsntr_json(
        &["chat", "send", "buddy", "--body-file", "-", "-d", dir],
        Some(b"wfstdin hello"),
    );
    assert_eq!(code, 0, "{sent}");
    assert_eq!(sent["spilled"], false);

    // Neither text nor --body-file is an error (exit 1, not a panic).
    let out = Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(["--json", "chat", "send", "buddy", "-d", dir])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(1));

    // `sql` reports SELECT rows in JSON.
    let (code, got) = rsntr_json(
        &[
            "sql",
            dir,
            "SELECT endpoint_id, name FROM _peers ORDER BY name",
        ],
        None,
    );
    assert_eq!(code, 0, "{got}");
    assert_eq!(got["columns"], serde_json::json!(["endpoint_id", "name"]));
    assert_eq!(got["rows"][0][1], "buddy");
    assert_eq!(got["row_count"].as_i64(), Some(1));

    let _ = store::node_id(tmp.path()).expect("id");
}
