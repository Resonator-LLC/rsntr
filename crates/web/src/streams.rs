//! Server-initiated streams (`GET`/`POST /stream/{id}`, web-api.md
//! section 5). The registry is fully wired in v1 but no builtin
//! modulation opens streams yet; first real traffic is chat (M7).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::BytesMut;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use resonator_protocol::{EnvelopeObject, MAX_FRAME_LEN, encode_envelope};

use crate::WebState;
use crate::feed::FeedEvent;
use crate::frames::{decode_frames_body, frames_error};

struct Entry {
    mod_name: String,
    /// Server-to-browser frames; taken by the single reader.
    frames: Option<mpsc::Receiver<EnvelopeObject>>,
    reader_taken: bool,
    /// Browser-to-server write half; `None` = no write half or FIN seen.
    write: Option<mpsc::Sender<EnvelopeObject>>,
    had_write_half: bool,
}

/// Registry of announced streams, shared between the handlers and the
/// modulations that open streams.
#[derive(Default)]
pub struct StreamRegistry {
    inner: Mutex<HashMap<String, Entry>>,
}

/// The modulation-side handle of one server-initiated stream.
pub struct ServerStream {
    /// The announced ULID.
    pub id: String,
    /// Frames pushed here reach the browser's `GET /stream/{id}` reader.
    pub frames: mpsc::Sender<EnvelopeObject>,
    /// Frames the browser POSTs, when the stream was opened with a
    /// write half.
    pub incoming: Option<mpsc::Receiver<EnvelopeObject>>,
}

impl StreamRegistry {
    /// `(id, mod)` of every stream not yet consumed to its end, for the
    /// feed's reconnect re-announcement.
    pub fn open_streams(&self) -> Vec<(String, String)> {
        self.inner
            .lock()
            .expect("stream registry lock")
            .iter()
            .map(|(id, e)| (id.clone(), e.mod_name.clone()))
            .collect()
    }

    fn remove(&self, id: &str) {
        self.inner.lock().expect("stream registry lock").remove(id);
    }
}

impl WebState {
    /// Opens and announces a server-initiated stream (for modulations;
    /// no v1 builtin uses it yet). The stream ends when the returned
    /// `frames` sender is dropped and the reader has drained it.
    pub fn open_server_stream(&self, mod_name: &str, with_write_half: bool) -> ServerStream {
        let id = ulid::Ulid::new().to_string();
        let (ftx, frx) = mpsc::channel::<EnvelopeObject>(8);
        let (incoming, write) = if with_write_half {
            let (wtx, wrx) = mpsc::channel::<EnvelopeObject>(8);
            (Some(wrx), Some(wtx))
        } else {
            (None, None)
        };
        self.streams
            .inner
            .lock()
            .expect("stream registry lock")
            .insert(
                id.clone(),
                Entry {
                    mod_name: mod_name.to_string(),
                    frames: Some(frx),
                    reader_taken: false,
                    write,
                    had_write_half: with_write_half,
                },
            );
        self.publish(FeedEvent::Stream {
            id: id.clone(),
            mod_name: mod_name.to_string(),
        });
        ServerStream {
            id,
            frames: ftx,
            incoming,
        }
    }
}

/// `GET /stream/{id}`: the read half, one reader only.
pub(crate) async fn stream_get(
    State(state): State<Arc<WebState>>,
    Path(id): Path<String>,
) -> Response {
    let frames = {
        let mut reg = state.streams.inner.lock().expect("stream registry lock");
        match reg.get_mut(&id) {
            None => {
                return frames_error(
                    StatusCode::NOT_FOUND,
                    "point-unknown",
                    format!("no stream {id:?}"),
                    None,
                );
            }
            Some(e) if e.reader_taken || e.frames.is_none() => {
                return frames_error(
                    StatusCode::CONFLICT,
                    "protocol-error",
                    format!("stream {id:?} already has a reader"),
                    None,
                );
            }
            Some(e) => {
                e.reader_taken = true;
                e.frames.take().expect("checked above")
            }
        }
    };

    let (btx, brx) = mpsc::channel::<Result<bytes::Bytes, std::convert::Infallible>>(8);
    let st = state.clone();
    tokio::spawn(async move {
        let mut frames = frames;
        while let Some(obj) = frames.recv().await {
            let mut buf = BytesMut::new();
            if encode_envelope(&obj, &mut buf).is_err() {
                break;
            }
            if btx.send(Ok(buf.freeze())).await.is_err() {
                // Reader aborted: reset the stream, release everything.
                break;
            }
        }
        st.streams.remove(&id);
        st.publish(FeedEvent::Closed { id });
    });

    (
        StatusCode::OK,
        [
            ("Content-Type", "application/rsntr-frames"),
            ("Cache-Control", "no-store"),
            ("X-Accel-Buffering", "no"),
        ],
        Body::from_stream(ReceiverStream::new(brx)),
    )
        .into_response()
}

/// `POST /stream/{id}`: the write half; `Rsntr-Fin: 1` closes it.
pub(crate) async fn stream_post(
    State(state): State<Arc<WebState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let fin = headers
        .get("Rsntr-Fin")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim() == "1");

    let tx = {
        let reg = state.streams.inner.lock().expect("stream registry lock");
        match reg.get(&id) {
            None => {
                return frames_error(
                    StatusCode::NOT_FOUND,
                    "point-unknown",
                    format!("no stream {id:?}"),
                    None,
                );
            }
            Some(e) => match &e.write {
                Some(tx) => tx.clone(),
                None => {
                    let why = if e.had_write_half {
                        "write half already finished"
                    } else {
                        "stream has no write half"
                    };
                    return frames_error(StatusCode::CONFLICT, "protocol-error", why, None);
                }
            },
        }
    };

    let frames = match decode_frames_body(&headers, body, 16 * MAX_FRAME_LEN).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    for obj in frames {
        if tx.send(obj).await.is_err() {
            return frames_error(
                StatusCode::CONFLICT,
                "protocol-error",
                "the stream's modulation went away",
                None,
            );
        }
    }
    if fin {
        let mut reg = state.streams.inner.lock().expect("stream registry lock");
        if let Some(e) = reg.get_mut(&id) {
            e.write = None;
        }
    }
    StatusCode::NO_CONTENT.into_response()
}
