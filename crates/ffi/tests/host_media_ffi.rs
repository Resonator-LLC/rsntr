//! The embedded-app side of a host-published media source: publish,
//! write frames, learn when a viewer joins, close. Plain #[test], the
//! way Swift and Kotlin call it.

use std::sync::{Arc, Mutex};

use resonator_ffi::{MediaViewerListener, Node};

struct Viewers(Mutex<Vec<u32>>);

impl MediaViewerListener for Viewers {
    fn on_viewers(&self, count: u32) {
        self.0.lock().expect("viewers lock").push(count);
    }
}

#[test]
fn publishing_requires_serving_and_declares_the_source() {
    let tmp = rsntr::testutil::TempDir::new("ffi-media");
    let dir = tmp.path().to_string_lossy().into_owned();
    let node = Node::new(dir, true).expect("open");

    // Not serving: a clear refusal naming what to do, not a silent no-op.
    match node.publish_media("camera".into(), "video/mp4".into(), None) {
        Err(e) => assert!(
            format!("{e}").contains("serve()"),
            "the error must say what to do: {e}"
        ),
        Ok(_) => panic!("publishing must refuse while this process does not serve"),
    }

    node.serve().expect("serve");
    assert!(node.published_media().is_empty());

    let listener = Arc::new(Viewers(Mutex::new(Vec::new())));
    let feed = node
        .publish_media(
            "camera".into(),
            "video/mp4; codecs=\"avc1.4d401f\"".into(),
            Some(listener.clone() as Arc<dyn MediaViewerListener>),
        )
        .expect("publish");
    assert_eq!(feed.name(), "camera");
    assert_eq!(node.published_media(), vec!["camera".to_string()]);

    // The declaration landed, with an empty command marking it host-provided.
    let row = node
        .local_query(
            "SELECT command, content_type FROM _media WHERE name = 'camera'".into(),
            vec![],
        )
        .expect("row");
    assert_eq!(row.rows.len(), 1);
    assert!(
        matches!(&row.rows[0].values[0], resonator_ffi::FfiValue::Text { v } if v.is_empty()),
        "an empty command is what makes it host-provided: {:?}",
        row.rows[0].values[0]
    );
    assert!(
        matches!(&row.rows[0].values[1], resonator_ffi::FfiValue::Text { v } if v.contains("avc1")),
        "{:?}",
        row.rows[0].values[1]
    );

    // Writing with nobody watching is fine and never blocks.
    assert_eq!(feed.viewers(), 0);
    assert_eq!(feed.write(b"frame".to_vec()), 0);

    // Closing is idempotent and withdraws the feed.
    feed.close();
    assert!(node.published_media().is_empty());
    feed.close();
    assert_eq!(feed.write(b"after close".to_vec()), 0);

    node.suspend();
}
