//! A consumer that stops reading (`| head`, a pager quit) must not turn
//! `rsntr` into a panic with exit 101: the write fails with EPIPE and the
//! process exits 0 quietly.

use std::process::{Command, Stdio};

#[test]
fn closed_stdout_exits_zero_quietly() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .arg("guide")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    // Close the read end without reading: the first write past the pipe
    // buffer fails with EPIPE.
    drop(child.stdout.take());
    let out = child.wait_with_output().expect("wait");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}
