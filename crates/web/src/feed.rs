//! The SSE control feed (`GET /feed`, web-api.md section 4): stream
//! announcements, `closed` notices, and session-level envelope objects.
//! No replay; on reconnect still-open server streams are re-announced.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use serde_json::json;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::WebState;

/// One feed event. `Envelope` carries the object pre-serialized to
/// Turtle so fan-out is a cheap clone.
#[derive(Debug, Clone)]
pub enum FeedEvent {
    /// A server-initiated stream exists; fetch `GET /stream/{id}`.
    Stream { id: String, mod_name: String },
    /// The announced stream ended or was abandoned.
    Closed { id: String },
    /// A session-level envelope object as a Turtle document.
    Envelope { turtle: String },
}

fn to_event(ev: FeedEvent) -> Event {
    match ev {
        FeedEvent::Stream { id, mod_name } => Event::default()
            .event("stream")
            .data(json!({ "id": id, "mod": mod_name }).to_string()),
        FeedEvent::Closed { id } => Event::default()
            .event("closed")
            .data(json!({ "id": id }).to_string()),
        FeedEvent::Envelope { turtle } => Event::default().event("envelope").data(turtle),
    }
}

pub(crate) async fn feed(State(state): State<Arc<WebState>>) -> impl IntoResponse {
    let rx = state.feed.subscribe();

    // retry: 2000 first, then the re-announcement of every still-open
    // server-initiated stream (the reconnect contract).
    let mut initial: Vec<Result<Event, Infallible>> =
        vec![Ok(Event::default().retry(Duration::from_millis(2000)))];
    for (id, mod_name) in state.streams.open_streams() {
        initial.push(Ok(to_event(FeedEvent::Stream { id, mod_name })));
    }

    // A lagged receiver skips what it missed: the feed has no replay,
    // pages recover by re-reading.
    let live = BroadcastStream::new(rx)
        .filter_map(|r| r.ok())
        .map(|ev| Ok::<Event, Infallible>(to_event(ev)));
    let stream = tokio_stream::iter(initial).chain(live);

    let sse = Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(20))
            .text("ping"),
    );
    (
        [("Cache-Control", "no-store"), ("X-Accel-Buffering", "no")],
        sse,
    )
}
