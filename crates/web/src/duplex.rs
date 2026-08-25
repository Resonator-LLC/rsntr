//! The upstream half of the audio-duplex lane: `POST /duplex/{id}`
//! (web-api.md, "Duplex upstream").
//!
//! A browser cannot stream a request body on HTTP/1.1, so the upstream
//! is a sequence of ordinary POSTs, each carrying one span of raw bytes;
//! the server appends them to the open exchange in POST-completion
//! order. The client MUST serialize its POSTs; `Rsntr-Fin: 1` on the
//! last one half-closes the wire (the source's stdin sees EOF). The
//! exchange itself was opened by `POST /request` with an `audio-duplex`
//! query; its response carries the `rsntr:AudioDuplex` header whose
//! `rsntr:id` names this endpoint.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

use crate::WebState;
use crate::frames::frames_error;

/// One upstream item: a span of raw bytes, or the client's Fin.
#[derive(Debug)]
pub(crate) enum UpMsg {
    Data(bytes::Bytes),
    Fin,
}

/// Open duplex exchanges by request ULID. Registered by `/request`
/// before its response starts, removed by the exchange's relay task when
/// it ends, so entries never outlive their stream.
#[derive(Default)]
pub(crate) struct DuplexRegistry {
    inner: Mutex<HashMap<String, mpsc::Sender<UpMsg>>>,
}

impl DuplexRegistry {
    pub(crate) fn insert(&self, id: &str, tx: mpsc::Sender<UpMsg>) {
        self.inner.lock().unwrap().insert(id.to_string(), tx);
    }

    pub(crate) fn sender(&self, id: &str) -> Option<mpsc::Sender<UpMsg>> {
        self.inner.lock().unwrap().get(id).cloned()
    }

    pub(crate) fn remove(&self, id: &str) {
        self.inner.lock().unwrap().remove(id);
    }
}

/// `POST /duplex/{id}`: append this body's bytes to the open exchange.
/// The body is read incrementally (never buffered whole), and the
/// bounded channel toward the wire is the backpressure. 204 on success,
/// 404 for an unknown or already-ended exchange, 409 when the exchange
/// dies mid-body.
pub(crate) async fn duplex_post(
    State(state): State<Arc<WebState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(tx) = state.duplex.sender(&id) else {
        return frames_error(
            StatusCode::NOT_FOUND,
            "not-found",
            format!("no open audio-duplex exchange {id:?}"),
            Some(id),
        );
    };
    let fin = headers
        .get("Rsntr-Fin")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim() == "1");

    let mut chunks = body.into_data_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                return frames_error(
                    StatusCode::BAD_REQUEST,
                    "protocol-error",
                    format!("upstream body failed: {e}"),
                    Some(id),
                );
            }
        };
        if chunk.is_empty() {
            continue;
        }
        if tx.send(UpMsg::Data(chunk)).await.is_err() {
            state.duplex.remove(&id);
            return frames_error(
                StatusCode::CONFLICT,
                "conflict",
                "the audio-duplex exchange has ended",
                Some(id),
            );
        }
    }
    if fin && tx.send(UpMsg::Fin).await.is_err() {
        state.duplex.remove(&id);
        return frames_error(
            StatusCode::CONFLICT,
            "conflict",
            "the audio-duplex exchange has ended",
            Some(id),
        );
    }
    StatusCode::NO_CONTENT.into_response()
}
