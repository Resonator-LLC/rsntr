//! The connector to OpenAI-compatible model providers: the HTTP client
//! ([`LlmClient`]: retries, timeouts, cancellation, streaming and
//! non-streaming requests), the stream parser and the message and
//! accumulation types.

pub mod client;
pub mod sse;
pub mod types;

pub use client::LlmClient;
pub use types::{
    ChatRequest, Completion, Message, StreamAccumulator, ToolCall, ToolCallAccumulator, ToolSpec,
    Usage, parse_completion,
};

/// Failure of a model request. The messages never contain keys.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LlmError {
    #[error("the request was cancelled")]
    Cancelled,
    #[error("the provider did not answer in time ({0})")]
    Timeout(&'static str),
    #[error("the provider answered with HTTP status {status}: {message}")]
    Http { status: u16, message: String },
    #[error("could not reach the provider: {0}")]
    Connect(String),
    #[error("the provider reported an error: {0}")]
    Provider(String),
    #[error("unexpected response from the provider: {0}")]
    Protocol(String),
}
