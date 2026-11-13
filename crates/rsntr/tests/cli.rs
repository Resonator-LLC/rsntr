//! `rsntr` against a real server: sign in, start a wave, list, read,
//! reply, rename, and the errors people see.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use resonator_agents::Integrations;
use resonator_server::app::App;
use resonator_server::config::Config;
use serde_json::{Value, json};

fn rsntr(config: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .args(args)
        .env("RSNTR_CONFIG", config)
        .env_remove("RSNTR_SERVER")
        .env_remove("RSNTR_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run rsntr");
    let mut input = child.stdin.take().unwrap();
    if let Some(text) = stdin {
        input.write_all(text.as_bytes()).unwrap();
    }
    drop(input);
    child.wait_with_output().unwrap()
}

fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "rsntr failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_person_uses_waves_from_the_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        domain: "example.com".to_owned(),
        listen: vec!["127.0.0.1:0".to_owned()],
        database_path: dir.path().join("resonator.db"),
        register_max_per_hour: 0,
        ..Config::default()
    };
    let mut prepared = App::prepare(&config).await.unwrap();
    let (integrations, routes) = Integrations::mount(&mut prepared).await.unwrap();
    let app = prepared.serve(routes).await.unwrap();
    let server = format!("http://{}", app.local_addrs()[0]);
    let http = reqwest::Client::new();
    for name in ["alice", "bob"] {
        let response = http
            .post(format!("{server}/auth/register"))
            .json(&json!({"address": name, "password": format!("{name}-password-long")}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }
    let file = dir.path().join("rsntr").join("config.toml");

    let run = |args: Vec<String>, stdin: Option<String>| {
        let file = file.clone();
        tokio::task::spawn_blocking(move || {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            rsntr(&file, &args, stdin.as_deref())
        })
    };
    let args = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();

    // Not signed in yet.
    let output = run(args(&["inbox"]), None).await.unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("rsntr login"));

    // A wrong password, then the right one.
    let login = args(&[
        "login",
        "alice@example.com",
        "--server",
        &server,
        "--password-stdin",
    ]);
    let output = run(login.clone(), Some("nope\n".into())).await.unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("wrong address or password"));
    let out = ok(&run(login, Some("alice-password-long\n".into()))
        .await
        .unwrap());
    assert!(out.contains("Signed in"), "{out}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert_eq!(
        ok(&run(args(&["whoami"]), None).await.unwrap()).trim(),
        format!("alice@example.com on {server}")
    );

    // A new wave with bob, its text from stdin.
    let out = ok(&run(
        args(&["--json", "new", "Plans", "--with", "bob@example.com"]),
        Some("Shall we meet?\n".into()),
    )
    .await
    .unwrap());
    let created: Value = serde_json::from_str(&out).unwrap();
    let wave = created["wave_id"].as_str().unwrap().to_owned();
    let root = created["root_blip_id"].as_str().unwrap().to_owned();
    let short = wave.split("/w+").nth(1).unwrap().to_owned();

    let out = ok(&run(args(&["inbox"]), None).await.unwrap());
    assert!(out.contains(&wave), "{out}");
    let out = ok(&run(args(&["search", "with:bob@example.com"]), None)
        .await
        .unwrap());
    assert!(out.contains(&wave), "{out}");

    // Replies by short ids, at the end and to the first blip.
    ok(&run(args(&["reply", &short, "-m", "At noon."]), None)
        .await
        .unwrap());
    let root_short = root.strip_prefix("b+").unwrap();
    ok(
        &run(args(&["reply", &short, root_short, "-m", "Yes!"]), None)
            .await
            .unwrap(),
    );
    ok(&run(args(&["title", &short, "Lunch"]), None).await.unwrap());
    let out = ok(&run(args(&["read", &short, "--mark-read"]), None)
        .await
        .unwrap());
    for text in [
        "Title: Lunch",
        "Shall we meet?",
        "At noon.",
        "Yes!",
        "bob@example.com",
    ] {
        assert!(out.contains(text), "{text} in {out}");
    }

    // Errors of the tools reach people, with a failing exit code.
    let output = run(args(&["read", "example.com/w+nothere"]), None)
        .await
        .unwrap();
    assert!(!output.status.success());
    let output = run(
        args(&["tool", "read_wave", "{\"wave_id\": \"nope\"}"]),
        None,
    )
    .await
    .unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("not a wave id"));

    // Logging out forgets the token.
    ok(&run(args(&["logout"]), None).await.unwrap());
    assert!(!file.exists());

    integrations.shutdown().await;
    app.shutdown().await;
}
