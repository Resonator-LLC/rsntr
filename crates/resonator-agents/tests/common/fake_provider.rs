//! A fake OpenAI-compatible provider for tests: an axum server on
//! 127.0.0.1 that answers `POST /v1/chat/completions` with scripted
//! replies and `GET /v1/models` with a trivial list.
#![allow(dead_code)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// What the fake answers to one request.
#[derive(Debug, Clone)]
pub enum Reply {
    /// Streamed text in these pieces, `delay` between chunks, then usage
    /// + [DONE].
    Text {
        pieces: Vec<String>,
        delay: Duration,
    },
    /// Streamed tool calls (several = parallel tool calls, interleaved
    /// fragments by index), then finish_reason tool_calls.
    ToolCalls(Vec<(String, String, Value)>),
    /// Plain JSON completion (non-streaming), regardless of `stream`.
    Json(Value),
    /// HTTP error status with optional Retry-After seconds and an
    /// OpenAI-style error body.
    Status {
        status: u16,
        retry_after: Option<u64>,
        message: String,
    },
    /// Sends `pieces` then drops the connection without [DONE], in the
    /// middle of an event.
    Disconnect { pieces: Vec<String> },
    /// Never answers until the client goes away.
    Hang,
    /// A stream that says `error` midway (OpenRouter style error chunk).
    StreamError {
        pieces: Vec<String>,
        message: String,
    },
}

impl Reply {
    /// Streamed text without delays.
    pub fn text(pieces: &[&str]) -> Reply {
        Reply::Text {
            pieces: pieces.iter().map(|p| (*p).to_owned()).collect(),
            delay: Duration::ZERO,
        }
    }
}

/// A request the fake received.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub path: String,
    /// The JSON body (`Null` if it was not JSON).
    pub body: Value,
    /// Headers by lower-case name.
    pub headers: BTreeMap<String, String>,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

#[derive(Default)]
struct Shared {
    replies: VecDeque<Reply>,
    requests: Vec<RecordedRequest>,
    cancelled: usize,
}

type SharedState = Arc<Mutex<Shared>>;

pub struct FakeProvider {
    address: std::net::SocketAddr,
    shared: SharedState,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for FakeProvider {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl FakeProvider {
    pub async fn start() -> FakeProvider {
        let shared = SharedState::default();
        let app = Router::new()
            .route("/v1/chat/completions", post(completions))
            .route("/v1/models", get(models))
            .with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the fake provider");
        let address = listener.local_addr().expect("local address");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        FakeProvider {
            address,
            shared,
            server,
        }
    }

    /// `http://127.0.0.1:PORT/v1`
    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    /// Queues a reply; replies are used in order, and with none queued
    /// the fake streams the text "ok".
    pub fn push(&self, reply: Reply) {
        self.shared.lock().unwrap().replies.push_back(reply);
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.shared.lock().unwrap().requests.clone()
    }

    pub fn request_count(&self) -> usize {
        self.shared.lock().unwrap().requests.len()
    }

    pub async fn wait_for_requests(&self, n: usize, timeout: Duration) -> bool {
        self.wait(timeout, |shared| shared.requests.len() >= n)
            .await
    }

    /// Responses whose client went away before the fake finished sending.
    pub fn cancelled_streams(&self) -> usize {
        self.shared.lock().unwrap().cancelled
    }

    pub async fn wait_for_cancelled(&self, n: usize, timeout: Duration) -> bool {
        self.wait(timeout, |shared| shared.cancelled >= n).await
    }

    async fn wait(&self, timeout: Duration, done: impl Fn(&Shared) -> bool) -> bool {
        let end = tokio::time::Instant::now() + timeout;
        loop {
            if done(&self.shared.lock().unwrap()) {
                return true;
            }
            if tokio::time::Instant::now() >= end {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// Counts a response as cancelled if dropped before it was finished.
struct CancelGuard {
    shared: SharedState,
    finished: bool,
}

impl CancelGuard {
    fn finish(&mut self) {
        self.finished = true;
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if !self.finished
            && let Ok(mut shared) = self.shared.lock()
        {
            shared.cancelled += 1;
        }
    }
}

async fn models() -> Response {
    axum::Json(json!({
        "object": "list",
        "data": [{"id": "fake-model", "object": "model", "owned_by": "fake"}],
    }))
    .into_response()
}

async fn completions(
    State(shared): State<SharedState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let recorded = RecordedRequest {
        path: "/v1/chat/completions".to_owned(),
        body: body.clone(),
        headers: headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect(),
    };
    let reply = {
        let mut state = shared.lock().unwrap();
        state.requests.push(recorded);
        state.replies.pop_front()
    }
    .unwrap_or_else(|| Reply::text(&["ok"]));
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let include_usage = body
        .pointer("/stream_options/include_usage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("fake-model")
        .to_owned();

    match reply {
        Reply::Text { pieces, delay } => {
            if !stream {
                return json_response(completion_json(&model, &pieces.concat(), &[]));
            }
            let mut events = vec![(
                chunk(&model, json!({"role": "assistant", "content": ""}), None),
                Duration::ZERO,
            )];
            for piece in &pieces {
                events.push((chunk(&model, json!({"content": piece}), None), delay));
            }
            events.push((chunk(&model, json!({}), Some("stop")), Duration::ZERO));
            if include_usage {
                events.push((usage_chunk(&model), Duration::ZERO));
            }
            events.push(("data: [DONE]\n\n".to_owned(), Duration::ZERO));
            stream_response(shared, events, false)
        }
        Reply::ToolCalls(calls) => {
            if !stream {
                return json_response(completion_json(&model, "", &calls));
            }
            let mut events = Vec::new();
            let mut first = Vec::new();
            let mut halves = Vec::new();
            for (index, (id, name, arguments)) in calls.iter().enumerate() {
                let arguments = arguments.to_string();
                let mut middle = arguments.len() / 2;
                while !arguments.is_char_boundary(middle) {
                    middle += 1;
                }
                first.push(json!({
                    "index": index, "id": id, "type": "function",
                    "function": {"name": name, "arguments": arguments[..middle]},
                }));
                halves
                    .push(json!({"index": index, "function": {"arguments": arguments[middle..]}}));
            }
            // The starts of all calls, then the rest of each, interleaved.
            for fragment in first.into_iter().chain(halves) {
                events.push((
                    chunk(&model, json!({"tool_calls": [fragment]}), None),
                    Duration::ZERO,
                ));
            }
            events.push((chunk(&model, json!({}), Some("tool_calls")), Duration::ZERO));
            if include_usage {
                events.push((usage_chunk(&model), Duration::ZERO));
            }
            events.push(("data: [DONE]\n\n".to_owned(), Duration::ZERO));
            stream_response(shared, events, false)
        }
        Reply::Json(value) => json_response(value),
        Reply::Status {
            status,
            retry_after,
            message,
        } => {
            let mut response = (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                axum::Json(json!({
                    "error": {"message": message, "type": "fake_error", "code": status},
                })),
            )
                .into_response();
            if let Some(seconds) = retry_after {
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, seconds.to_string().parse().unwrap());
            }
            response
        }
        Reply::Disconnect { pieces } => {
            let mut events = vec![(
                chunk(&model, json!({"role": "assistant", "content": ""}), None),
                Duration::ZERO,
            )];
            for piece in &pieces {
                events.push((
                    chunk(&model, json!({"content": piece}), None),
                    Duration::ZERO,
                ));
            }
            events.push((
                "data: {\"choices\":[{\"delta\":{\"cont".to_owned(),
                Duration::ZERO,
            ));
            stream_response(shared, events, true)
        }
        Reply::Hang => {
            let _guard = CancelGuard {
                shared,
                finished: false,
            };
            std::future::pending::<Response>().await
        }
        Reply::StreamError { pieces, message } => {
            let mut events = vec![(
                chunk(&model, json!({"role": "assistant", "content": ""}), None),
                Duration::ZERO,
            )];
            for piece in &pieces {
                events.push((
                    chunk(&model, json!({"content": piece}), None),
                    Duration::ZERO,
                ));
            }
            let error = json!({
                "id": "chatcmpl-fake", "object": "chat.completion.chunk", "model": model,
                "error": {"message": message, "code": 502},
                "choices": [{"index": 0, "delta": {"content": ""}, "finish_reason": "error"}],
            });
            events.push((format!("data: {error}\n\n"), Duration::ZERO));
            events.push(("data: [DONE]\n\n".to_owned(), Duration::ZERO));
            stream_response(shared, events, false)
        }
    }
}

fn chunk(model: &str, delta: Value, finish_reason: Option<&str>) -> String {
    let value = json!({
        "id": "chatcmpl-fake",
        "object": "chat.completion.chunk",
        "model": model,
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
    });
    format!("data: {value}\n\n")
}

fn usage_chunk(model: &str) -> String {
    let value = json!({
        "id": "chatcmpl-fake",
        "object": "chat.completion.chunk",
        "model": model,
        "choices": [],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
    });
    format!("data: {value}\n\n")
}

fn completion_json(model: &str, content: &str, calls: &[(String, String, Value)]) -> Value {
    let mut message = json!({"role": "assistant", "content": content});
    let mut finish_reason = "stop";
    if !calls.is_empty() {
        message["content"] = Value::Null;
        message["tool_calls"] = calls
            .iter()
            .map(|(id, name, arguments)| {
                json!({"id": id, "type": "function",
                       "function": {"name": name, "arguments": arguments.to_string()}})
            })
            .collect();
        finish_reason = "tool_calls";
    }
    json!({
        "id": "chatcmpl-fake",
        "object": "chat.completion",
        "model": model,
        "choices": [{"index": 0, "message": message, "finish_reason": finish_reason}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
    })
}

fn json_response(value: Value) -> Response {
    axum::Json(value).into_response()
}

/// A streamed response: each event after its delay. With `abort`, the
/// body fails after the last event, which makes the server drop the
/// connection without ending the response properly.
fn stream_response(shared: SharedState, events: Vec<(String, Duration)>, abort: bool) -> Response {
    let (sender, receiver) = mpsc::channel::<Result<Bytes, std::io::Error>>(1);
    tokio::spawn(async move {
        for (event, delay) in events {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if sender.send(Ok(Bytes::from(event))).await.is_err() {
                return;
            }
        }
        if abort {
            let _ = sender
                .send(Err(std::io::Error::other("fake disconnect")))
                .await;
        }
    });
    let guard = CancelGuard {
        shared,
        finished: false,
    };
    let body =
        futures_util::stream::unfold((receiver, guard), |(mut receiver, mut guard)| async move {
            match receiver.recv().await {
                Some(item) => {
                    // A deliberate failure is the fake's own doing.
                    if item.is_err() {
                        guard.finish();
                    }
                    Some((item, (receiver, guard)))
                }
                None => {
                    guard.finish();
                    None
                }
            }
        });
    let mut response = Response::new(Body::from_stream(body));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, "text/event-stream".parse().unwrap());
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-cache".parse().unwrap());
    response
}
