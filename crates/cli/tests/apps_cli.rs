//! M4: `rsntr apps declare` / `apps list`, the serving daemon
//! registering itself, and the system graph naming apps and tables.

use std::process::Command;

use rsntr::testutil::TempDir;
use rsntr::{serve, store};

fn rsntr(args: &[&str]) -> (i32, serde_json::Value, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rsntr"))
        .arg("--json")
        .args(args)
        .output()
        .expect("run rsntr");
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let json = serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Null);
    (
        out.status.code().unwrap_or(-1),
        json,
        format!("{stdout}{}", String::from_utf8_lossy(&out.stderr)),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn declare_list_and_graph() {
    let tmp = TempDir::new("apps");
    store::init_dir(tmp.path()).expect("init");
    let d = tmp.path().to_str().unwrap();
    let (code, _, err) = rsntr(&[
        "sql",
        d,
        "CREATE TABLE notebook_pages (id INTEGER PRIMARY KEY, body TEXT)",
    ]);
    assert_eq!(code, 0, "{err}");

    // A missing table is refused; a reserved one too.
    let (code, _, err) = rsntr(&[
        "apps",
        "declare",
        "org.example.notebook",
        "notebook_missing",
        "x",
        "-d",
        d,
    ]);
    assert_ne!(code, 0);
    assert!(err.contains("does not exist"), "{err}");
    let (code, _, err) = rsntr(&[
        "apps",
        "declare",
        "org.example.notebook",
        "_peers",
        "x",
        "-d",
        d,
    ]);
    assert_ne!(code, 0);
    assert!(err.contains("reserved"), "{err}");

    let (code, v, err) = rsntr(&[
        "apps",
        "declare",
        "org.example.notebook",
        "notebook_pages",
        "pages of the notebook",
        "--name",
        "Notebook",
        "--version",
        "1.0",
        "-d",
        d,
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["tables"], serde_json::json!(["notebook_pages"]));
    assert_eq!(v["name"], "Notebook");

    let (code, v, _) = rsntr(&["apps", "list", "-d", d]);
    assert_eq!(code, 0);
    assert_eq!(v["apps"].as_array().map(Vec::len), Some(1), "{v}");

    // The comment reached _comments; the daemon registers itself.
    let node = serve::start_node(tmp.path(), true).await.expect("serve");
    let (code, v, _) = rsntr(&["apps", "list", "-d", d]);
    assert_eq!(code, 0);
    let ids: Vec<&str> = v["apps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["app_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"network.resonator.rsntr"), "{ids:?}");
    assert!(ids.contains(&"org.example.notebook"));

    // The system graph names the app, its table, and the comment.
    let (code, v, err) = rsntr(&["system", "graph", "-d", d]);
    assert_eq!(code, 0, "{err}");
    let lines: Vec<String> = v["triples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("urn:rsntr:app:org.example.notebook") && l.ends_with("#App> .")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("urn:rsntr:table:notebook_pages") && l.contains("#declares>"))
    );
    assert!(lines.iter().any(
        |l| l.contains("urn:rsntr:table:notebook_pages") && l.contains("pages of the notebook")
    ));
    // Declaring again merges (idempotent) and adds a second table.
    let (code, _, err) = rsntr(&[
        "sql",
        d,
        "CREATE TABLE notebook_tags (id INTEGER PRIMARY KEY)",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, v, err) = rsntr(&[
        "apps",
        "declare",
        "org.example.notebook",
        "notebook_tags",
        "tags",
        "-d",
        d,
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        v["tables"],
        serde_json::json!(["notebook_pages", "notebook_tags"])
    );
    node.shutdown().await;
}

/// `apps declare` describes an app that is usually not this process,
/// and it used to restate the whole `_apps` row from this one.
///
/// So the platform column said where the CLI ran, not where the app
/// runs, and name and version were reset whenever their flags were
/// omitted - which is exactly how you declare an app's second table.
/// Every value written was true about the process that wrote it, and
/// the column headings say they are about the app; `apps list` and the
/// system graph then answered "who last touched this id" to a reader
/// asking "what is this app".
#[tokio::test(flavor = "multi_thread")]
async fn declaring_for_an_app_does_not_restate_it_from_this_process() {
    let tmp = TempDir::new("apps-identity");
    store::init_dir(tmp.path()).expect("init");
    let d = tmp.path().to_str().unwrap();
    let (code, _, err) = rsntr(&[
        "sql",
        d,
        "CREATE TABLE notebook_a (x TEXT); CREATE TABLE notebook_b (x TEXT); \
         CREATE TABLE kontrolator_notes (x TEXT)",
    ]);
    assert_eq!(code, 0, "{err}");

    let app_row = |id: &str| -> serde_json::Value {
        let (code, v, err) = rsntr(&["apps", "list", "-d", d]);
        assert_eq!(code, 0, "{err}");
        v["apps"]
            .as_array()
            .expect("apps")
            .iter()
            .find(|a| a["app_id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("no row for {id} in {v}"))
    };

    // Declared with a name and a version, then a second table without.
    let (code, _, err) = rsntr(&[
        "apps",
        "declare",
        "org.example.notebook",
        "notebook_a",
        "first",
        "--name",
        "Notebook",
        "--version",
        "1.0",
        "-d",
        d,
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = rsntr(&[
        "apps",
        "declare",
        "org.example.notebook",
        "notebook_b",
        "second",
        "-d",
        d,
    ]);
    assert_eq!(code, 0, "{err}");
    let row = app_row("org.example.notebook");
    assert_eq!(row["name"], "Notebook", "the name survives: {row}");
    assert_eq!(row["version"], "1.0", "the version survives: {row}");
    assert_eq!(
        row["tables"],
        serde_json::json!(["notebook_a", "notebook_b"]),
        "{row}"
    );

    // An app that registered itself from another device: the CLI has
    // nothing truer to say about where it runs, so it says nothing.
    let (code, _, err) = rsntr(&[
        "sql",
        d,
        "INSERT INTO _apps (app_id, name, version, lib_version, platform, first_seen, last_seen) \
         VALUES ('am.resonator.kontrolator', 'Kontrolator', '3.1', '0.2.1', 'ios', \
                 '2026-01-01T00:00:00Z', '2026-09-01T00:00:00Z')",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = rsntr(&[
        "apps",
        "declare",
        "am.resonator.kontrolator",
        "kontrolator_notes",
        "notes",
        "-d",
        d,
    ]);
    assert_eq!(code, 0, "{err}");
    let row = app_row("am.resonator.kontrolator");
    assert_eq!(row["platform"], "ios", "the app's platform stands: {row}");
    assert_eq!(row["lib_version"], "0.2.1", "and what it links: {row}");
    assert_eq!(row["name"], "Kontrolator", "{row}");
    assert_eq!(row["version"], "3.1", "{row}");
    assert_eq!(
        row["tables"],
        serde_json::json!(["kontrolator_notes"]),
        "the declaration itself still lands: {row}"
    );
    // last_seen moves, because something did just touch the row.
    assert_ne!(row["last_seen"], "2026-09-01T00:00:00Z", "{row}");
}
