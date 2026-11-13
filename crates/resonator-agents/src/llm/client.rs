//! The HTTP client of the Chat Completions API.
//!
//! One [`LlmClient`] per configured provider. A request is built from a
//! [`ChatRequest`], sent to `<base_url>/chat/completions`, and read either
//! as an event stream (`stream = true`) or as one JSON document.
//!
//! The request body: `model`, `messages`, `tools` and `tool_choice` (only
//! with tools), `max_tokens` (if set), `stream` and, when streaming with
//! `include_usage`, `stream_options`. Then the provider's `extra_body` is
//! merged in, and the request's own `extra_body` over that. The extra
//! members may override or add anything except `model`, `messages`,
//! `tools` and `stream`, which always are what the client says; so
//! `tool_choice`, `max_tokens`, `stream_options` or
//! `parallel_tool_calls` can be set by the operator.
//!
//! Timeouts: the connection within `connect_timeout_ms`, the response
//! headers within `first_byte_timeout_secs`, every further read within
//! `idle_timeout_secs`, and the whole call, retries included, within
//! `request_timeout_secs`.
//!
//! Retries: a 408, 429 or 5xx status, and a failed connection or a
//! timeout before any byte of a successful response's body was received,
//! are retried up to `max_retries` times. The wait is `retry_backoff_ms`
//! doubled with each retry (plus a little jitter); a `Retry-After` header
//! in seconds takes precedence. No wait is longer than
//! `retry_max_wait_secs`, and none goes past the overall deadline. Once
//! text has been passed to the caller nothing is retried.
//!
//! Ends of streams: `[DONE]` ends a stream. A stream that ends cleanly
//! without `[DONE]` is accepted if it brought anything; with nothing it is
//! a protocol error. A connection that breaks off in the middle of the
//! response is an error ([`LlmError::Connect`]) even if text came before
//! it: the response is incomplete, and the caller has the text it was
//! given.
//!
//! The API key never appears in logs, errors or `Debug` output; provider
//! error messages that quote it are scrubbed.

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use resonator_server::config::AiProviderConfig;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use super::LlmError;
use super::sse::SseParser;
use super::types::{
    ChatRequest, Completion, StreamAccumulator, error_message, parse_completion, truncate,
};

/// The largest response body read when it is not a stream.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// The most of an error response's body that is read.
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
/// The most text a streamed response may have.
const MAX_STREAM_TEXT_BYTES: usize = 4 * 1024 * 1024;
/// The most bytes a streamed response may have in all (tool call
/// arguments, metadata and framing included).
const MAX_STREAM_BYTES: usize = 32 * 1024 * 1024;
/// Members of the body that `extra_body` cannot change.
const PROTECTED_KEYS: [&str; 4] = ["model", "messages", "tools", "stream"];

/// A client for one OpenAI-compatible provider. Cheap to clone.
#[derive(Clone)]
pub struct LlmClient {
    inner: Arc<Inner>,
}

struct Inner {
    name: String,
    url: String,
    http: reqwest::Client,
    api_key: Option<String>,
    headers: HeaderMap,
    extra_body: Map<String, Value>,
    stream: bool,
    include_usage: bool,
    first_byte_timeout: Option<Duration>,
    idle_timeout: Option<Duration>,
    request_timeout: Duration,
    max_retries: u32,
    retry_backoff: Duration,
    retry_max_wait: Duration,
}

impl fmt::Debug for LlmClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LlmClient")
            .field("name", &self.inner.name)
            .field("url", &self.inner.url)
            .field("has_api_key", &self.inner.api_key.is_some())
            .field("stream", &self.inner.stream)
            .finish_non_exhaustive()
    }
}

/// The name of the fallback variable that holds a provider's key.
fn default_key_variable(name: &str) -> String {
    format!(
        "RESONATOR_AI_{}_API_KEY",
        name.to_ascii_uppercase().replace('-', "_")
    )
}

fn nonzero_secs(secs: u64) -> Option<Duration> {
    (secs > 0).then(|| Duration::from_secs(secs))
}

impl LlmClient {
    /// `name` is the provider's name from config (for logs and the env
    /// var fallback).
    pub fn new(name: &str, config: &AiProviderConfig) -> Result<LlmClient, LlmError> {
        let api_key = if !config.api_key.is_empty() {
            Some(config.api_key.expose().to_owned())
        } else if !config.api_key_env.is_empty() {
            match std::env::var(&config.api_key_env) {
                Ok(key) if !key.is_empty() => Some(key),
                _ => {
                    return Err(LlmError::Connect(format!(
                        "provider '{name}': the environment variable {} that should hold the \
                         API key is not set",
                        config.api_key_env
                    )));
                }
            }
        } else {
            std::env::var(default_key_variable(name))
                .ok()
                .filter(|key| !key.is_empty())
        };

        let mut headers = HeaderMap::new();
        for (header, value) in &config.headers {
            let header_name = HeaderName::from_bytes(header.as_bytes()).map_err(|_| {
                LlmError::Connect(format!(
                    "provider '{name}': '{header}' is not a valid header"
                ))
            })?;
            let header_value = HeaderValue::from_str(value).map_err(|_| {
                LlmError::Connect(format!(
                    "provider '{name}': the value of header '{header}' is not valid"
                ))
            })?;
            headers.insert(header_name, header_value);
        }
        if let Some(key) = &api_key {
            let mut value = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| {
                LlmError::Connect(format!(
                    "provider '{name}': the API key has characters a header cannot have"
                ))
            })?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }

        let extra_body = match serde_json::to_value(&config.extra_body) {
            Ok(Value::Object(map)) => map,
            Ok(_) | Err(_) => {
                return Err(LlmError::Connect(format!(
                    "provider '{name}': extra_body cannot be written as JSON"
                )));
            }
        };

        let mut builder = reqwest::Client::builder()
            .use_rustls_tls()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none());
        if config.connect_timeout_ms > 0 {
            builder = builder.connect_timeout(Duration::from_millis(config.connect_timeout_ms));
        }
        let http = builder.build().map_err(|error| {
            LlmError::Connect(format!(
                "provider '{name}': cannot set up the HTTP client ({})",
                describe(&error)
            ))
        })?;

        Ok(LlmClient {
            inner: Arc::new(Inner {
                name: name.to_owned(),
                url: format!("{}/chat/completions", config.base_url.trim_end_matches('/')),
                http,
                api_key,
                headers,
                extra_body,
                stream: config.stream,
                include_usage: config.include_usage,
                first_byte_timeout: nonzero_secs(config.first_byte_timeout_secs),
                idle_timeout: nonzero_secs(config.idle_timeout_secs),
                request_timeout: Duration::from_secs(config.request_timeout_secs.max(1)),
                max_retries: config.max_retries,
                retry_backoff: Duration::from_millis(config.retry_backoff_ms),
                retry_max_wait: Duration::from_secs(config.retry_max_wait_secs),
            }),
        })
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// The JSON body of a request.
    fn body(&self, request: &ChatRequest) -> Value {
        let inner = &self.inner;
        let mut body = Map::new();
        if !request.tools.is_empty()
            && let Some(choice) = &request.tool_choice
        {
            body.insert("tool_choice".to_owned(), choice.clone());
        }
        if let Some(max_tokens) = request.max_tokens {
            body.insert("max_tokens".to_owned(), json!(max_tokens));
        }
        if inner.stream && inner.include_usage {
            body.insert("stream_options".to_owned(), json!({"include_usage": true}));
        }
        for (key, value) in inner.extra_body.iter().chain(request.extra_body.iter()) {
            if !PROTECTED_KEYS.contains(&key.as_str()) {
                body.insert(key.clone(), value.clone());
            }
        }
        body.insert("model".to_owned(), json!(request.model));
        body.insert(
            "messages".to_owned(),
            Value::Array(request.messages.iter().map(|m| m.to_json()).collect()),
        );
        if !request.tools.is_empty() {
            body.insert(
                "tools".to_owned(),
                Value::Array(request.tools.iter().map(|t| t.to_json()).collect()),
            );
        }
        body.insert("stream".to_owned(), json!(inner.stream));
        Value::Object(body)
    }

    /// Runs one completion. Streams when config.stream; calls `on_text`
    /// with each piece of visible text as it arrives (non-streaming: once
    /// with the whole text). Returns the completion (content, tool_calls,
    /// finish_reason, usage).
    pub async fn complete(
        &self,
        request: &ChatRequest,
        cancel: &tokio_util::sync::CancellationToken,
        on_text: &mut (dyn FnMut(&str) + Send),
    ) -> Result<Completion, LlmError> {
        let inner = &self.inner;
        let started = Instant::now();
        let deadline = started + inner.request_timeout;
        let body = serde_json::to_vec(&self.body(request))
            .map_err(|error| LlmError::Protocol(format!("cannot write the request ({error})")))?;
        let mut state = AttemptState::default();
        let mut retries = 0u32;
        loop {
            let result = self
                .attempt(&body, deadline, cancel, on_text, &mut state)
                .await;
            let failure = match result {
                Ok(completion) => {
                    let usage = completion.usage.unwrap_or_default();
                    tracing::info!(
                        provider = %inner.name,
                        model = %request.model,
                        status = state.status.unwrap_or(0),
                        prompt_tokens = usage.prompt_tokens,
                        completion_tokens = usage.completion_tokens,
                        duration_ms = started.elapsed().as_millis() as u64,
                        retries,
                        "model request finished"
                    );
                    return Ok(completion);
                }
                Err(failure) => failure,
            };
            let error = self.scrub(failure.error);
            let can_retry = failure.retryable
                && !state.delivered
                && retries < inner.max_retries
                && error != LlmError::Cancelled;
            if can_retry {
                let wait = self.retry_wait(retries, failure.retry_after);
                if Instant::now() + wait < deadline {
                    tracing::debug!(
                        provider = %inner.name,
                        model = %request.model,
                        error = %error,
                        wait_ms = wait.as_millis() as u64,
                        "retrying a model request"
                    );
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            return Err(self.log_failure(request, LlmError::Cancelled, &state, started, retries));
                        }
                        _ = tokio::time::sleep(wait) => {}
                    }
                    retries += 1;
                    continue;
                }
            }
            return Err(self.log_failure(request, error, &state, started, retries));
        }
    }

    fn log_failure(
        &self,
        request: &ChatRequest,
        error: LlmError,
        state: &AttemptState,
        started: Instant,
        retries: u32,
    ) -> LlmError {
        tracing::info!(
            provider = %self.inner.name,
            model = %request.model,
            status = state.status.unwrap_or(0),
            duration_ms = started.elapsed().as_millis() as u64,
            retries,
            error = %error,
            "model request failed"
        );
        error
    }

    /// Removes the key from a message that might quote it.
    fn scrub(&self, error: LlmError) -> LlmError {
        let Some(key) = self.inner.api_key.as_deref().filter(|key| key.len() >= 4) else {
            return error;
        };
        let clean = |text: String| {
            if text.contains(key) {
                text.replace(key, "[redacted]")
            } else {
                text
            }
        };
        match error {
            LlmError::Http { status, message } => LlmError::Http {
                status,
                message: clean(message),
            },
            LlmError::Connect(message) => LlmError::Connect(clean(message)),
            LlmError::Provider(message) => LlmError::Provider(clean(message)),
            LlmError::Protocol(message) => LlmError::Protocol(clean(message)),
            other => other,
        }
    }

    /// The wait before retry number `retry` (from 0).
    fn retry_wait(&self, retry: u32, retry_after: Option<Duration>) -> Duration {
        let inner = &self.inner;
        let wait = match retry_after {
            Some(wait) => wait,
            None => {
                let base = inner
                    .retry_backoff
                    .saturating_mul(1u32.checked_shl(retry).unwrap_or(u32::MAX));
                let mut random = [0u8; 2];
                let jitter = if getrandom::fill(&mut random).is_ok() {
                    // Up to a tenth more.
                    base.mul_f64(f64::from(u16::from_le_bytes(random)) / f64::from(u16::MAX) / 10.0)
                } else {
                    Duration::ZERO
                };
                base.saturating_add(jitter)
            }
        };
        wait.min(inner.retry_max_wait)
    }

    /// One try.
    async fn attempt(
        &self,
        body: &[u8],
        deadline: Instant,
        cancel: &CancellationToken,
        on_text: &mut (dyn FnMut(&str) + Send),
        state: &mut AttemptState,
    ) -> Result<Completion, Failure> {
        let inner = &self.inner;
        state.status = None;
        let request = inner
            .http
            .post(&inner.url)
            .headers(inner.headers.clone())
            .header(CONTENT_TYPE, "application/json")
            .header(
                ACCEPT,
                if inner.stream {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            )
            .body(body.to_vec());
        let mut response = race(
            request.send(),
            inner.first_byte_timeout,
            "first byte",
            deadline,
            cancel,
        )
        .await
        .map_err(Failure::retryable)?
        .map_err(|error| Failure::retryable(send_error(&error)))?;

        let status = response.status();
        state.status = Some(status.as_u16());
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let retryable = matches!(status.as_u16(), 408 | 429) || status.is_server_error();
            let text = match self
                .read_body(&mut response, MAX_ERROR_BODY_BYTES, true, deadline, cancel)
                .await
            {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(LlmError::Cancelled) => return Err(Failure::fatal(LlmError::Cancelled)),
                Err(_) => String::new(),
            };
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|value| {
                    value
                        .get("error")
                        .filter(|error| !error.is_null())
                        .map(error_message)
                })
                .unwrap_or_else(|| {
                    let text = text.trim();
                    if text.is_empty() {
                        status.canonical_reason().unwrap_or("error").to_owned()
                    } else {
                        truncate(text, 300)
                    }
                });
            let error = LlmError::Http {
                status: status.as_u16(),
                message,
            };
            return Err(Failure {
                error,
                retryable,
                retry_after,
            });
        }

        let is_json = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains("json"));
        if !inner.stream || is_json {
            let bytes = self
                .read_body(&mut response, MAX_BODY_BYTES, false, deadline, cancel)
                .await
                .map_err(|error| Failure {
                    // Nothing of the body yet: worth another try.
                    retryable: matches!(error, LlmError::Timeout(_) | LlmError::Connect(_)),
                    error,
                    retry_after: None,
                })?;
            let text = std::str::from_utf8(&bytes).map_err(|_| {
                Failure::fatal(LlmError::Protocol("the response is not UTF-8".to_owned()))
            })?;
            let completion = parse_completion(text).map_err(Failure::fatal)?;
            if !completion.content.is_empty() {
                state.delivered = true;
                on_text(&completion.content);
            }
            return Ok(completion);
        }
        self.read_stream(response, deadline, cancel, on_text, state)
            .await
    }

    /// Reads a whole body, up to `max` bytes; with `cut`, a longer body is
    /// cut off instead of being an error.
    async fn read_body(
        &self,
        response: &mut reqwest::Response,
        max: usize,
        cut: bool,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, LlmError> {
        let mut bytes = Vec::new();
        loop {
            let chunk = race(
                response.chunk(),
                self.inner.idle_timeout,
                "idle",
                deadline,
                cancel,
            )
            .await?
            .map_err(|error| body_error(&error))?;
            let Some(chunk) = chunk else {
                return Ok(bytes);
            };
            if bytes.len() + chunk.len() > max {
                if cut {
                    bytes.extend_from_slice(&chunk[..max - bytes.len()]);
                    return Ok(bytes);
                }
                return Err(LlmError::Protocol(format!(
                    "the response is larger than {max} bytes"
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
    }

    async fn read_stream(
        &self,
        mut response: reqwest::Response,
        deadline: Instant,
        cancel: &CancellationToken,
        on_text: &mut (dyn FnMut(&str) + Send),
        state: &mut AttemptState,
    ) -> Result<Completion, Failure> {
        let mut parser = SseParser::new();
        let mut accumulator = StreamAccumulator::default();
        let mut received = 0usize;
        loop {
            let chunk = race(
                response.chunk(),
                self.inner.idle_timeout,
                "idle",
                deadline,
                cancel,
            )
            .await
            .and_then(|chunk| chunk.map_err(|error| body_error(&error)))
            .map_err(|error| Failure {
                // Before any byte of the body a broken connection or a
                // timeout is worth another try.
                retryable: received == 0
                    && matches!(error, LlmError::Timeout(_) | LlmError::Connect(_)),
                error,
                retry_after: None,
            })?;
            let (events, end) = match chunk {
                Some(chunk) => {
                    received += chunk.len();
                    if received > MAX_STREAM_BYTES {
                        return Err(Failure::fatal(LlmError::Protocol(format!(
                            "the stream is larger than {MAX_STREAM_BYTES} bytes"
                        ))));
                    }
                    let events = parser.feed(&chunk).map_err(|overflow| {
                        Failure::fatal(LlmError::Protocol(overflow.to_string()))
                    })?;
                    (events, false)
                }
                None => (parser.finish(), true),
            };
            for event in events {
                let step = accumulator.on_data(&event.data).map_err(Failure::fatal)?;
                if !step.text.is_empty() {
                    if accumulator.content().len() > MAX_STREAM_TEXT_BYTES {
                        return Err(Failure::fatal(LlmError::Protocol(format!(
                            "the response has more than {MAX_STREAM_TEXT_BYTES} bytes of text"
                        ))));
                    }
                    state.delivered = true;
                    on_text(&step.text);
                }
                if step.done {
                    // Dropping the response closes the connection, should
                    // the server have more to say.
                    return Ok(accumulator.finish());
                }
            }
            if end {
                if accumulator.has_output() {
                    return Ok(accumulator.finish());
                }
                return Err(Failure::fatal(LlmError::Protocol(
                    "the stream ended without a response".to_owned(),
                )));
            }
        }
    }
}

#[derive(Debug, Default)]
struct AttemptState {
    /// The status of the last response.
    status: Option<u16>,
    /// Whether any text was passed to the caller.
    delivered: bool,
}

/// A failed try.
struct Failure {
    error: LlmError,
    retryable: bool,
    retry_after: Option<Duration>,
}

impl Failure {
    fn retryable(error: LlmError) -> Failure {
        Failure {
            retryable: !matches!(error, LlmError::Cancelled),
            error,
            retry_after: None,
        }
    }

    fn fatal(error: LlmError) -> Failure {
        Failure {
            error,
            retryable: false,
            retry_after: None,
        }
    }
}

/// Awaits `future` unless the token is cancelled, the phase's own
/// timeout passes, or the deadline does.
async fn race<T>(
    future: impl Future<Output = T>,
    timeout: Option<Duration>,
    label: &'static str,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<T, LlmError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    let (limit, label) = match timeout {
        Some(timeout) if timeout < remaining => (timeout, label),
        _ => (remaining, "total"),
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(LlmError::Cancelled),
        result = tokio::time::timeout(limit, future) => result.map_err(|_| LlmError::Timeout(label)),
    }
}

/// An error and its causes, without the URL.
fn describe(error: &reqwest::Error) -> String {
    let mut text = String::new();
    let mut source: Option<&dyn std::error::Error> = std::error::Error::source(error);
    while let Some(cause) = source {
        if !text.is_empty() {
            text.push_str(": ");
        }
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    if text.is_empty() {
        text = error.to_string();
    }
    truncate(&text, 300)
}

fn send_error(error: &reqwest::Error) -> LlmError {
    if error.is_timeout() {
        LlmError::Timeout("connect")
    } else {
        LlmError::Connect(describe(error))
    }
}

fn body_error(error: &reqwest::Error) -> LlmError {
    LlmError::Connect(format!(
        "the connection broke off in the middle of the response ({})",
        describe(error)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::{Message, ToolSpec};

    fn config() -> AiProviderConfig {
        AiProviderConfig {
            base_url: "http://127.0.0.1:9/v1/".to_owned(),
            ..AiProviderConfig::default()
        }
    }

    #[test]
    fn the_body_is_built_and_extras_cannot_change_the_core() {
        let mut config = config();
        config.extra_body =
            serde_json::from_value(json!({"max_tokens": 6, "temperature": 0.5, "tools": []}))
                .unwrap();
        let client = LlmClient::new("t", &config).unwrap();
        assert_eq!(client.inner.url, "http://127.0.0.1:9/v1/chat/completions");
        let mut request = ChatRequest {
            model: "m".to_owned(),
            messages: vec![Message::User("hi".to_owned())],
            tools: vec![ToolSpec {
                name: "t".to_owned(),
                description: "d".to_owned(),
                parameters: json!({"type": "object"}),
            }],
            tool_choice: Some(json!("auto")),
            max_tokens: Some(5),
            ..ChatRequest::default()
        };
        request.extra_body.insert("model".to_owned(), json!("evil"));
        request.extra_body.insert("stream".to_owned(), json!(false));
        request.extra_body.insert("max_tokens".to_owned(), json!(7));
        let body = client.body(&request);
        assert_eq!(body["model"], "m");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], 7);
        assert_eq!(body["temperature"], 0.5);
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["stream_options"], json!({"include_usage": true}));
        assert_eq!(body["tools"][0]["function"]["name"], "t");
        assert!(body.get("parallel_tool_calls").is_none());
    }

    #[test]
    fn retry_waits_double_and_are_capped() {
        let mut config = config();
        config.retry_backoff_ms = 100;
        config.retry_max_wait_secs = 1;
        let client = LlmClient::new("t", &config).unwrap();
        let first = client.retry_wait(0, None);
        assert!(first >= Duration::from_millis(100) && first <= Duration::from_millis(110));
        let third = client.retry_wait(2, None);
        assert!(third >= Duration::from_millis(400) && third <= Duration::from_millis(440));
        assert_eq!(client.retry_wait(10, None), Duration::from_secs(1));
        assert_eq!(client.retry_wait(40, None), Duration::from_secs(1));
        assert_eq!(
            client.retry_wait(0, Some(Duration::from_secs(30))),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn complete_is_send() {
        fn assert_send<T: Send>(_: T) {}
        let client = LlmClient::new("t", &config()).unwrap();
        let request = ChatRequest::default();
        let cancel = CancellationToken::new();
        let mut on_text = |_: &str| {};
        assert_send(client.complete(&request, &cancel, &mut on_text));
    }
}
