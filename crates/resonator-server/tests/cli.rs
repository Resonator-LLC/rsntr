//! Tests of the `resonator` binary and of server start-up and shutdown.

mod common;

use std::process::Command;

use common::{TestDb, add_participant, delta, participant, wavelet_name};
use resonator_server::app::{self, AppError};
use resonator_server::config::Config;
use resonator_server::storage::{Database, DatabaseOptions, Synchronous};
use resonator_server::waveserver::WaveletProvider;

fn resonator() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_resonator-server"));
    // The tests must not pick up settings of the environment they run in.
    for (key, _) in std::env::vars() {
        if key.starts_with("RESONATOR_") {
            command.env_remove(key);
        }
    }
    command
}

fn stdout(command: &mut Command) -> String {
    let output = command.output().expect("run resonator");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn config_precedence_is_defaults_file_environment_flags() {
    let dir = tempfile::tempdir().unwrap();
    // No file: the defaults.
    let defaults =
        Config::from_toml(&stdout(resonator().current_dir(dir.path()).arg("config"))).unwrap();
    assert_eq!(defaults, Config::default());
    assert_eq!(defaults.listen, ["0.0.0.0:9898"]);

    let file = dir.path().join("server.toml");
    std::fs::write(
        &file,
        "domain = \"file.example\"\nsnapshot_interval = 7\nwave_cache_size = 5\n",
    )
    .unwrap();
    let effective = Config::from_toml(&stdout(
        resonator()
            .current_dir(dir.path())
            .env("RESONATOR_DOMAIN", "env.example")
            .env("RESONATOR_SNAPSHOT_INTERVAL", "9")
            .env("RESONATOR_LISTEN", "0.0.0.0:1,0.0.0.0:2")
            .env("RESONATOR_SYNCHRONOUS", "full")
            .args([
                "--config",
                "server.toml",
                "--domain",
                "flag.example",
                "config",
            ]),
    ))
    .unwrap();
    assert_eq!(effective.domain, "flag.example");
    assert_eq!(effective.snapshot_interval, 9);
    assert_eq!(effective.wave_cache_size, 5);
    assert_eq!(effective.listen, ["0.0.0.0:1", "0.0.0.0:2"]);
    assert_eq!(effective.synchronous, Synchronous::Full);
    assert_eq!(effective.database_path, Config::default().database_path);

    // resonator.toml in the working directory is read by default, and the
    // config file can be named in the environment.
    std::fs::write(
        dir.path().join("resonator.toml"),
        "domain = \"cwd.example\"\n",
    )
    .unwrap();
    let from_cwd =
        Config::from_toml(&stdout(resonator().current_dir(dir.path()).arg("config"))).unwrap();
    assert_eq!(from_cwd.domain, "cwd.example");
    let from_env = Config::from_toml(&stdout(
        resonator()
            .current_dir(dir.path())
            .env("RESONATOR_CONFIG", &file)
            .arg("config"),
    ))
    .unwrap();
    assert_eq!(from_env.domain, "file.example");
}

#[test]
fn invalid_configuration_fails_with_a_message() {
    let dir = tempfile::tempdir().unwrap();
    let output = resonator()
        .current_dir(dir.path())
        .args(["--domain", "Not A Domain", "config"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not a valid domain name"), "{stderr}");
}

#[tokio::test]
async fn backup_subcommand_copies_a_database_in_use() {
    let test_db = TestDb::new();
    let config = Config {
        domain: common::DOMAIN.to_owned(),
        database_path: test_db.path.clone(),
        ..Config::default()
    };
    let server = app::start(&config).await.unwrap();
    let user = participant("user@example.com");
    let name = wavelet_name("w", "conv+root");
    let v0 = server.version_zero(&name);
    let result = server
        .submit_request_durably(
            &name,
            delta(&user, &v0, vec![add_participant(&user, &user)]),
        )
        .await
        .unwrap();

    // The server still has the database open.
    let destination = test_db.dir.path().join("copy.db");
    let printed = stdout(
        resonator()
            .arg("--database-path")
            .arg(&test_db.path)
            .arg("backup")
            .arg(&destination),
    );
    assert_eq!(printed.trim(), destination.display().to_string());
    // An existing destination is refused.
    let again = resonator()
        .arg("--database-path")
        .arg(&test_db.path)
        .arg("backup")
        .arg(&destination)
        .output()
        .unwrap();
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("already exists"));
    app::shutdown(server).await;

    let copy = Database::open(&destination, DatabaseOptions::default()).unwrap();
    assert_eq!(
        copy.delta_store().open(name).end_version().await.unwrap(),
        Some(result.hashed_version_after_application)
    );
    assert_eq!(
        copy.meta(app::META_DOMAIN).await.unwrap().as_deref(),
        Some(common::DOMAIN)
    );
    copy.close().await;
}

#[test]
fn backup_of_a_missing_database_fails() {
    let dir = tempfile::tempdir().unwrap();
    let output = resonator()
        .current_dir(dir.path())
        .args(["backup", "copy.db"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not exist"));
    // Asking for a backup must not create a database.
    assert!(!dir.path().join("resonator.db").exists());
}

#[tokio::test]
async fn serve_starts_and_shuts_down_cleanly() {
    let test_db = TestDb::new();
    let config = Config {
        domain: common::DOMAIN.to_owned(),
        database_path: test_db.path.clone(),
        // An ephemeral port on the loopback interface.
        listen: vec!["127.0.0.1:0".to_owned()],
        ..Config::default()
    };
    // Stops as soon as it is ready.
    app::serve_until(config.clone(), async {}).await.unwrap();
    assert!(test_db.path.exists());
    // The WAL was checkpointed and removed or emptied on close.
    let wal = test_db.dir.path().join("resonator.db-wal");
    assert!(!wal.exists() || std::fs::metadata(&wal).unwrap().len() == 0);

    // The database remembers its domain.
    let other = Config {
        domain: "other.example".to_owned(),
        ..config.clone()
    };
    match app::start(&other).await {
        Err(AppError::DomainMismatch {
            stored, configured, ..
        }) => {
            assert_eq!(stored, common::DOMAIN);
            assert_eq!(configured, "other.example");
        }
        other => panic!("unexpected result {:?}", other.map(|_| ())),
    }
    let server = app::start(&config).await.unwrap();
    assert!(server.get_wave_ids().await.unwrap().is_empty());
    app::shutdown(server).await;
}

#[tokio::test]
async fn serve_fails_cleanly_when_an_address_is_taken() {
    let test_db = TestDb::new();
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let config = Config {
        domain: common::DOMAIN.to_owned(),
        database_path: test_db.path.clone(),
        listen: vec![
            "127.0.0.1:0".to_owned(),
            taken.local_addr().unwrap().to_string(),
        ],
        ..Config::default()
    };
    match app::serve_until(config.clone(), async {}).await {
        Err(AppError::Io(error)) => {
            assert!(error.to_string().contains("cannot listen on 127.0.0.1:"));
        }
        other => panic!("unexpected result {other:?}"),
    }
    // The database was closed again: it can be reopened right away.
    drop(taken);
    app::serve_until(config, async {}).await.unwrap();
}
