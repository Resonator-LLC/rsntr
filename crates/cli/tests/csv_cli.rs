//! `rsntr csv export` / `rsntr csv import` against a local node
//! directory, exercising the codec shared with the web interface.
#![cfg(feature = "web")]

use rsntr::testutil::TempDir;
use rsntr::{csvcmd, store};

#[tokio::test(flavor = "multi_thread")]
async fn csv_export_import_round_trip() {
    let tmp = TempDir::new("csv-cli");
    store::init_dir(tmp.path()).expect("init");

    {
        let conn = store::open_db(tmp.path()).expect("open");
        conn.execute_batch(
            "CREATE TABLE pets (name TEXT, note TEXT, data BLOB);
             INSERT INTO pets VALUES ('rex', 'a,b', x'0a0b');
             INSERT INTO pets VALUES ('fido', NULL, NULL);",
        )
        .expect("seed");
    }

    let doc = csvcmd::csv_export(tmp.path(), "pets")
        .await
        .expect("export runs")
        .expect("export allowed");
    assert_eq!(doc, "name,note,data\r\nrex,\"a,b\",x'0a0b'\r\nfido,,\r\n");

    // Import into a fresh table with --create, then export it back:
    // identical bytes.
    let report = csvcmd::csv_import(tmp.path(), "pets2", &doc, true)
        .await
        .expect("import runs")
        .expect("import allowed");
    assert!(report.created);
    assert_eq!(report.rows_inserted, 2);
    let doc2 = csvcmd::csv_export(tmp.path(), "pets2")
        .await
        .expect("re-export runs")
        .expect("re-export allowed");
    assert_eq!(doc2, doc);

    // Appending without --create works on an existing table; a missing
    // table without --create is a not-found outcome, and an unknown
    // export table likewise.
    let report = csvcmd::csv_import(tmp.path(), "pets2", &doc, false)
        .await
        .expect("append runs")
        .expect("append allowed");
    assert!(!report.created);
    assert_eq!(report.rows_inserted, 2);
    let missing = csvcmd::csv_import(tmp.path(), "nope", &doc, false)
        .await
        .expect("runs");
    assert!(
        matches!(&missing, Err(csvcmd::Failure::Error { code, .. }) if code == "not-found"),
        "got {missing:?}"
    );
    let missing = csvcmd::csv_export(tmp.path(), "nope").await.expect("runs");
    assert!(matches!(
        &missing,
        Err(csvcmd::Failure::Error { code, .. }) if code == "not-found"
    ));
}
