//! Host-published media feeds: a media source whose bytes come from the
//! embedding application rather than from a shell command.
//!
//! A `_media` row is the declaration of a source (its name, its content
//! type, and the thing `_policy` grants access to). Normally its
//! `command` is a shell pipeline the node runs. An **empty command**
//! means the bytes come from the host instead: an app registers a feed
//! here and writes encoded frames into it, and every viewer of that
//! source receives them. This is what lets a node embedded in a phone
//! serve its camera, where no shell command could: iOS cannot exec a
//! binary at all, and a camera is an in-process API rather than
//! something spawnable.
//!
//! Writes never block and never queue without bound. A viewer that
//! cannot keep up misses chunks and continues from the newest ones,
//! because this is a live feed: the alternative is the failure mode the
//! transport otherwise has, where a slow viewer stalls the encoder and
//! latency grows without a ceiling (measured at roughly half a second of
//! lag per second of running, with no recovery). Dropping is the point.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

/// Chunks buffered per feed before the slowest viewer starts missing
/// them. Small on purpose: a live viewer wants the newest frame, not a
/// backlog.
pub const FEED_CAPACITY: usize = 64;

/// Called when the number of viewers of a feed changes. An encoder uses
/// it to force a keyframe when someone joins, without which a late
/// viewer waits for the next one.
pub type ViewerObserver = Arc<dyn Fn(usize) + Send + Sync>;

struct Feed {
    content_type: String,
    tx: broadcast::Sender<Arc<[u8]>>,
    /// Viewer bookkeeping, shared with every viewer so theirs outlives
    /// the feed. Deliberately does NOT hold the sender: a viewer that
    /// kept the sender alive would never see the feed end.
    shared: Arc<FeedShared>,
}

#[derive(Default)]
struct FeedShared {
    viewers: AtomicUsize,
    observer: Mutex<Option<ViewerObserver>>,
}

/// The registry of feeds this process publishes. One per node.
#[derive(Default)]
pub struct MediaFeeds {
    feeds: Mutex<HashMap<String, Arc<Feed>>>,
}

/// The writing end of a published feed. Dropping it does not end the
/// feed; [`MediaFeeds::withdraw`] does.
#[derive(Clone)]
pub struct FeedWriter {
    name: String,
    feed: Arc<Feed>,
}

impl FeedWriter {
    /// The source name this feed serves.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Hands one chunk to every viewer. Never blocks. Returns how many
    /// viewers it went to (0 means nobody is watching, which is not an
    /// error: a camera may run before anyone looks).
    pub fn write(&self, chunk: &[u8]) -> usize {
        let bytes: Arc<[u8]> = Arc::from(chunk);
        self.feed.tx.send(bytes).unwrap_or(0)
    }

    /// How many viewers are attached right now.
    pub fn viewers(&self) -> usize {
        self.feed.shared.viewers.load(Ordering::SeqCst)
    }

    /// Registers (or clears) the viewer-count observer.
    pub fn set_viewer_observer(&self, observer: Option<ViewerObserver>) {
        *self.feed.shared.observer.lock().expect("observer lock") = observer;
    }

    /// The content type viewers are told about.
    pub fn content_type(&self) -> String {
        self.feed.content_type.clone()
    }
}

/// One viewer's end of a feed, handed to the serving path.
pub struct FeedReader {
    rx: broadcast::Receiver<Arc<[u8]>>,
    shared: Arc<FeedShared>,
}

/// What a viewer's next read produced.
pub enum FeedChunk {
    /// Bytes to forward. `skipped` counts chunks this viewer missed
    /// because it could not keep up; they are gone by design.
    Bytes { bytes: Arc<[u8]>, skipped: u64 },
    /// The host withdrew the feed (or dropped every writer): the stream
    /// ends cleanly.
    Ended,
}

impl FeedReader {
    /// Waits for the next chunk. Never yields a backlog: a viewer that
    /// fell behind resumes at the newest chunk and is told how many it
    /// missed.
    pub async fn next(&mut self) -> FeedChunk {
        let mut skipped = 0u64;
        loop {
            match self.rx.recv().await {
                Ok(bytes) => return FeedChunk::Bytes { bytes, skipped },
                Err(broadcast::error::RecvError::Lagged(n)) => skipped += n,
                Err(broadcast::error::RecvError::Closed) => return FeedChunk::Ended,
            }
        }
    }
}

impl Drop for FeedReader {
    fn drop(&mut self) {
        let left = self
            .shared
            .viewers
            .fetch_sub(1, Ordering::SeqCst)
            .saturating_sub(1);
        notify(&self.shared, left);
    }
}

fn notify(shared: &FeedShared, count: usize) {
    let observer = shared.observer.lock().expect("observer lock").clone();
    if let Some(f) = observer {
        f(count);
    }
}

impl MediaFeeds {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publishes `name`, replacing any feed already published under it
    /// (its viewers see the stream end and can re-open).
    pub fn publish(&self, name: &str, content_type: &str) -> FeedWriter {
        let (tx, _rx) = broadcast::channel(FEED_CAPACITY);
        let feed = Arc::new(Feed {
            content_type: content_type.to_string(),
            tx,
            shared: Arc::new(FeedShared::default()),
        });
        self.feeds
            .lock()
            .expect("media feeds lock")
            .insert(name.to_string(), feed.clone());
        FeedWriter {
            name: name.to_string(),
            feed,
        }
    }

    /// Ends the feed published under `name`; its viewers' streams end.
    pub fn withdraw(&self, name: &str) -> bool {
        self.feeds
            .lock()
            .expect("media feeds lock")
            .remove(name)
            .is_some()
    }

    /// The content type of a published feed, when one is published.
    pub fn content_type(&self, name: &str) -> Option<String> {
        self.feeds
            .lock()
            .expect("media feeds lock")
            .get(name)
            .map(|f| f.content_type.clone())
    }

    /// Attaches a viewer to `name`, or `None` when the host publishes no
    /// such feed.
    pub fn subscribe(&self, name: &str) -> Option<FeedReader> {
        let feed = self
            .feeds
            .lock()
            .expect("media feeds lock")
            .get(name)?
            .clone();
        let rx = feed.tx.subscribe();
        let shared = feed.shared.clone();
        let count = shared.viewers.fetch_add(1, Ordering::SeqCst) + 1;
        notify(&shared, count);
        Some(FeedReader { rx, shared })
    }

    /// Every published source name.
    pub fn published(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .feeds
            .lock()
            .expect("media feeds lock")
            .keys()
            .cloned()
            .collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_viewer_receives_what_the_host_writes() {
        let feeds = MediaFeeds::new();
        let w = feeds.publish("camera", "video/mp4");
        assert_eq!(w.viewers(), 0, "writing before anyone watches is fine");
        assert_eq!(w.write(b"ignored"), 0);

        let mut viewer = feeds.subscribe("camera").expect("published");
        assert_eq!(w.viewers(), 1);
        assert_eq!(w.write(b"frame-1"), 1);
        match viewer.next().await {
            FeedChunk::Bytes { bytes, skipped } => {
                assert_eq!(&*bytes, b"frame-1");
                assert_eq!(skipped, 0);
            }
            FeedChunk::Ended => panic!("ended early"),
        }
        assert!(feeds.subscribe("nothing-here").is_none());
        assert_eq!(feeds.published(), vec!["camera".to_string()]);
    }

    #[tokio::test]
    async fn a_slow_viewer_misses_chunks_instead_of_stalling_the_source() {
        let feeds = MediaFeeds::new();
        let w = feeds.publish("camera", "video/mp4");
        let mut viewer = feeds.subscribe("camera").expect("published");

        // Far more than the buffer holds, written without anyone reading:
        // the writes must all return immediately.
        let started = std::time::Instant::now();
        for i in 0..(FEED_CAPACITY * 4) {
            w.write(format!("frame-{i}").as_bytes());
        }
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "writing must never block on a slow viewer"
        );

        // The viewer resumes at the newest chunks and is told what it lost.
        match viewer.next().await {
            FeedChunk::Bytes { bytes, skipped } => {
                assert!(skipped > 0, "a lagging viewer is told it missed chunks");
                let text = String::from_utf8_lossy(&bytes).to_string();
                let n: usize = text.trim_start_matches("frame-").parse().expect("index");
                assert!(
                    n >= FEED_CAPACITY * 3,
                    "resumed near the live edge, got {text}"
                );
            }
            FeedChunk::Ended => panic!("ended early"),
        }
    }

    #[tokio::test]
    async fn viewer_count_is_observed_and_withdrawing_ends_the_stream() {
        let feeds = MediaFeeds::new();
        let w = feeds.publish("camera", "video/mp4");
        let seen: Arc<Mutex<Vec<usize>>> = Arc::default();
        let sink = seen.clone();
        w.set_viewer_observer(Some(Arc::new(move |n| {
            sink.lock().expect("seen lock").push(n)
        })));

        let a = feeds.subscribe("camera").expect("published");
        let mut b = feeds.subscribe("camera").expect("published");
        assert_eq!(w.viewers(), 2);
        drop(a);
        assert_eq!(w.viewers(), 1);
        assert_eq!(*seen.lock().expect("seen lock"), vec![1, 2, 1]);

        // The stream ends when no sender is left: the registry entry and
        // every writer. A viewer must never keep it alive by itself.
        assert!(feeds.withdraw("camera"), "withdraw reports it removed one");
        drop(w);
        assert!(
            matches!(
                tokio::time::timeout(Duration::from_secs(5), b.next()).await,
                Ok(FeedChunk::Ended)
            ),
            "a withdrawn feed must end its viewers streams, not hang them"
        );
        assert!(feeds.published().is_empty());
    }
}
