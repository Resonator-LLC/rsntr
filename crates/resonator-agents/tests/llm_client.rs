//! The model client against a fake provider.

#[path = "common/fake_provider.rs"]
mod fake_provider;

use std::time::{Duration, Instant};

use fake_provider::{FakeProvider, Reply};
use resonator_agents::llm::{ChatRequest, Completion, LlmClient, LlmError, Message, ToolSpec};
use resonator_server::config::{AiProviderConfig, Secret};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const KEY: &str = "sk-test-very-secret-key-1234";

fn config(fake: &FakeProvider) -> AiProviderConfig {
    AiProviderConfig {
        base_url: format!("{}/", fake.base_url()),
        first_byte_timeout_secs: 5,
        idle_timeout_secs: 5,
        request_timeout_secs: 20,
        max_retries: 2,
        retry_backoff_ms: 10,
        retry_max_wait_secs: 1,
        ..AiProviderConfig::default()
    }
}

fn request() -> ChatRequest {
    ChatRequest {
        model: "fake-model".to_owned(),
        messages: vec![
            Message::System("be brief".to_owned()),
            Message::User("hello".to_owned()),
        ],
        ..ChatRequest::default()
    }
}

async fn run(
    client: &LlmClient,
    request: &ChatRequest,
) -> (Result<Completion, LlmError>, Vec<String>) {
    let cancel = CancellationToken::new();
    let mut pieces = Vec::new();
    let result = client
        .complete(request, &cancel, &mut |text: &str| {
            pieces.push(text.to_owned())
        })
        .await;
    (result, pieces)
}

fn strings(pieces: &[&str]) -> Vec<String> {
    pieces.iter().map(|p| (*p).to_owned()).collect()
}

#[tokio::test]
async fn streamed_text_arrives_in_pieces() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::text(&["Hel", "lo", " world"]));
    let client = LlmClient::new("fake", &config(&fake)).unwrap();
    assert_eq!(client.name(), "fake");
    let (result, pieces) = run(&client, &request()).await;
    let completion = result.unwrap();
    assert_eq!(pieces, strings(&["Hel", "lo", " world"]));
    assert_eq!(completion.content, "Hello world");
    assert_eq!(completion.finish_reason.as_deref(), Some("stop"));
    let usage = completion.usage.unwrap();
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (10, 5));
    let recorded = &fake.requests()[0];
    assert_eq!(recorded.path, "/v1/chat/completions");
    assert_eq!(recorded.header("accept"), Some("text/event-stream"));
    assert_eq!(recorded.body["stream"], true);
}

#[tokio::test]
async fn non_streaming_requests_give_the_text_once() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::text(&["Hel", "lo"]));
    let mut config = config(&fake);
    config.stream = false;
    let client = LlmClient::new("fake", &config).unwrap();
    let (result, pieces) = run(&client, &request()).await;
    let completion = result.unwrap();
    assert_eq!(pieces, strings(&["Hello"]));
    assert_eq!(completion.content, "Hello");
    assert_eq!(completion.usage.unwrap().total_tokens, 15);
    let body = &fake.requests()[0].body;
    assert_eq!(body["stream"], false);
    assert!(body.get("stream_options").is_none());
}

#[tokio::test]
async fn a_json_answer_to_a_stream_request_is_read() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::Json(json!({
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "whole"},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1},
    })));
    let client = LlmClient::new("fake", &config(&fake)).unwrap();
    let (result, pieces) = run(&client, &request()).await;
    assert_eq!(result.unwrap().content, "whole");
    assert_eq!(pieces, strings(&["whole"]));
}

#[tokio::test]
async fn parallel_tool_calls_are_put_together() {
    let fake = FakeProvider::start().await;
    let calls = vec![
        (
            "call_a".to_owned(),
            "read_blip".to_owned(),
            json!({"blip_id": "b+1", "note": "ünïcode"}),
        ),
        (
            "call_b".to_owned(),
            "search".to_owned(),
            json!({"query": "in:inbox", "limit": 5}),
        ),
    ];
    fake.push(Reply::ToolCalls(calls.clone()));
    fake.push(Reply::ToolCalls(calls.clone()));
    let mut config = config(&fake);
    for stream in [true, false] {
        config.stream = stream;
        let client = LlmClient::new("fake", &config).unwrap();
        let (result, pieces) = run(&client, &request()).await;
        let completion = result.unwrap();
        assert!(pieces.is_empty());
        assert_eq!(completion.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(completion.tool_calls.len(), 2);
        for (call, (id, name, arguments)) in completion.tool_calls.iter().zip(&calls) {
            assert_eq!(&call.id, id);
            assert_eq!(&call.name, name);
            let parsed: Value = serde_json::from_str(&call.arguments).unwrap();
            assert_eq!(&parsed, arguments);
        }
    }
}

#[tokio::test]
async fn the_request_has_everything_and_the_extras_merge() {
    let fake = FakeProvider::start().await;
    let mut config = config(&fake);
    config.api_key = Secret::new(KEY);
    config
        .headers
        .insert("HTTP-Referer".to_owned(), "https://wave.example".to_owned());
    config
        .headers
        .insert("X-Title".to_owned(), "Resonator".to_owned());
    config.extra_body = serde_json::from_value(json!({
        "temperature": 0.7, "top_p": 0.9, "model": "not-this", "stream": false,
    }))
    .unwrap();
    let client = LlmClient::new("fake", &config).unwrap();
    let mut request = request();
    request.tools = vec![ToolSpec {
        name: "search".to_owned(),
        description: "Searches".to_owned(),
        parameters: json!({"type": "object", "properties": {}}),
    }];
    request.tool_choice = Some(json!("auto"));
    request.max_tokens = Some(321);
    request
        .extra_body
        .insert("temperature".to_owned(), json!(0.1));
    request
        .extra_body
        .insert("parallel_tool_calls".to_owned(), json!(false));
    run(&client, &request).await.0.unwrap();

    let recorded = &fake.requests()[0];
    let body = &recorded.body;
    assert_eq!(body["model"], "fake-model");
    assert_eq!(
        body["messages"],
        json!([{"role": "system", "content": "be brief"}, {"role": "user", "content": "hello"}])
    );
    assert_eq!(body["tools"][0]["function"]["name"], "search");
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["max_tokens"], 321);
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"], json!({"include_usage": true}));
    assert_eq!(body["temperature"], 0.1);
    assert_eq!(body["top_p"], 0.9);
    assert_eq!(body["parallel_tool_calls"], false);
    assert_eq!(
        recorded.header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    assert_eq!(
        recorded.header("http-referer"),
        Some("https://wave.example")
    );
    assert_eq!(recorded.header("x-title"), Some("Resonator"));
    assert_eq!(recorded.header("content-type"), Some("application/json"));

    // Without tools there is no tools, tool_choice or parallel_tool_calls.
    let client = LlmClient::new("fake", &self::config(&fake)).unwrap();
    let mut plain = self::request();
    plain.tool_choice = Some(json!("auto"));
    run(&client, &plain).await.0.unwrap();
    let body = &fake.requests()[1].body;
    for key in ["tools", "tool_choice", "parallel_tool_calls", "max_tokens"] {
        assert!(body.get(key).is_none(), "{key}");
    }
}

#[tokio::test]
async fn keys_come_from_the_environment_or_not_at_all() {
    let fake = FakeProvider::start().await;
    // No key at all: no header.
    let client = LlmClient::new("fake-keyless-7c1e", &config(&fake)).unwrap();
    run(&client, &request()).await.0.unwrap();
    assert_eq!(fake.requests()[0].header("authorization"), None);

    // A named variable.
    // SAFETY: the variable names are unique to this test.
    unsafe { std::env::set_var("LLM_CLIENT_TEST_KEY_93F1", "sk-from-named-var") };
    let mut named = config(&fake);
    named.api_key_env = "LLM_CLIENT_TEST_KEY_93F1".to_owned();
    let client = LlmClient::new("fake", &named).unwrap();
    run(&client, &request()).await.0.unwrap();
    assert_eq!(
        fake.requests()[1].header("authorization"),
        Some("Bearer sk-from-named-var")
    );

    // The fallback variable.
    unsafe { std::env::set_var("RESONATOR_AI_LLM_TEST_5D2A_API_KEY", "sk-from-fallback") };
    let client = LlmClient::new("llm-test-5d2a", &config(&fake)).unwrap();
    run(&client, &request()).await.0.unwrap();
    assert_eq!(
        fake.requests()[2].header("authorization"),
        Some("Bearer sk-from-fallback")
    );

    // A named variable that is not set is an error naming it.
    let mut missing = config(&fake);
    missing.api_key_env = "LLM_CLIENT_TEST_UNSET_0B7E".to_owned();
    let error = LlmClient::new("fake", &missing).unwrap_err();
    assert!(
        error.to_string().contains("LLM_CLIENT_TEST_UNSET_0B7E"),
        "{error}"
    );
}

#[tokio::test]
async fn rate_limits_are_retried_after_a_capped_wait() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::Status {
        status: 429,
        retry_after: Some(30),
        message: "slow down".to_owned(),
    });
    fake.push(Reply::text(&["fine"]));
    let client = LlmClient::new("fake", &config(&fake)).unwrap();
    let started = Instant::now();
    let (result, pieces) = run(&client, &request()).await;
    let elapsed = started.elapsed();
    assert_eq!(result.unwrap().content, "fine");
    assert_eq!(pieces, strings(&["fine"]));
    assert_eq!(fake.request_count(), 2);
    // Retry-After 30 is capped at retry_max_wait_secs = 1.
    assert!(elapsed >= Duration::from_millis(950), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
}

#[tokio::test]
async fn server_errors_are_retried() {
    let fake = FakeProvider::start().await;
    for _ in 0..2 {
        fake.push(Reply::Status {
            status: 500,
            retry_after: None,
            message: "oops".to_owned(),
        });
    }
    fake.push(Reply::text(&["third time"]));
    let client = LlmClient::new("fake", &config(&fake)).unwrap();
    let (result, _) = run(&client, &request()).await;
    assert_eq!(result.unwrap().content, "third time");
    assert_eq!(fake.request_count(), 3);
}

#[tokio::test]
async fn client_errors_are_not_retried() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::Status {
        status: 400,
        retry_after: None,
        message: "bad request: messages is invalid".to_owned(),
    });
    let client = LlmClient::new("fake", &config(&fake)).unwrap();
    let (result, pieces) = run(&client, &request()).await;
    assert_eq!(
        result.unwrap_err(),
        LlmError::Http {
            status: 400,
            message: "bad request: messages is invalid (400)".to_owned()
        }
    );
    assert!(pieces.is_empty());
    assert_eq!(fake.request_count(), 1);
}

#[tokio::test]
async fn retries_run_out() {
    let fake = FakeProvider::start().await;
    for _ in 0..3 {
        fake.push(Reply::Status {
            status: 503,
            retry_after: None,
            message: "overloaded".to_owned(),
        });
    }
    let client = LlmClient::new("fake", &config(&fake)).unwrap();
    let (result, _) = run(&client, &request()).await;
    assert!(
        matches!(result, Err(LlmError::Http { status: 503, .. })),
        "{result:?}"
    );
    assert_eq!(fake.request_count(), 3);
}

#[tokio::test]
async fn a_silent_server_times_out_before_the_first_byte() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::Hang);
    let mut config = config(&fake);
    config.first_byte_timeout_secs = 1;
    config.max_retries = 0;
    let client = LlmClient::new("fake", &config).unwrap();
    let started = Instant::now();
    let (result, _) = run(&client, &request()).await;
    assert_eq!(result.unwrap_err(), LlmError::Timeout("first byte"));
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(950) && elapsed < Duration::from_secs(3));
    // The fake sees the client go away.
    assert!(fake.wait_for_cancelled(1, Duration::from_secs(3)).await);
}

#[tokio::test]
async fn a_stream_that_stalls_times_out() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::Text {
        pieces: strings(&["first", "never"]),
        delay: Duration::from_millis(2500),
    });
    let mut config = config(&fake);
    config.idle_timeout_secs = 1;
    let client = LlmClient::new("fake", &config).unwrap();
    let started = Instant::now();
    let (result, pieces) = run(&client, &request()).await;
    assert_eq!(result.unwrap_err(), LlmError::Timeout("idle"));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(pieces.is_empty());
    // Part of the body had come (the role chunk): not retried.
    assert_eq!(fake.request_count(), 1);
}

#[tokio::test]
async fn a_broken_connection_is_an_error_and_not_retried_after_text() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::Disconnect {
        pieces: strings(&["partial ", "answer"]),
    });
    let client = LlmClient::new("fake", &config(&fake)).unwrap();
    let (result, pieces) = run(&client, &request()).await;
    assert!(matches!(result, Err(LlmError::Connect(_))), "{result:?}");
    assert_eq!(pieces, strings(&["partial ", "answer"]));
    assert_eq!(fake.request_count(), 1);
}

#[tokio::test]
async fn an_error_in_the_stream_is_a_provider_error() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::StreamError {
        pieces: strings(&["some"]),
        message: "upstream went away".to_owned(),
    });
    let client = LlmClient::new("fake", &config(&fake)).unwrap();
    let (result, pieces) = run(&client, &request()).await;
    assert_eq!(
        result.unwrap_err(),
        LlmError::Provider("upstream went away (502)".to_owned())
    );
    assert_eq!(pieces, strings(&["some"]));
    assert_eq!(fake.request_count(), 1);
}

#[tokio::test]
async fn an_empty_or_odd_success_is_a_protocol_error() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::Json(json!({"unexpected": true})));
    let client = LlmClient::new("fake", &config(&fake)).unwrap();
    let (result, _) = run(&client, &request()).await;
    assert!(matches!(result, Err(LlmError::Protocol(_))), "{result:?}");
    assert_eq!(fake.request_count(), 1);
}

#[tokio::test]
async fn cancellation_stops_the_stream_promptly() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::Text {
        pieces: (0..50).map(|n| format!("piece {n} ")).collect(),
        delay: Duration::from_millis(100),
    });
    let client = LlmClient::new("fake", &config(&fake)).unwrap();
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let mut pieces = Vec::new();
    let started = Instant::now();
    let result = client
        .complete(&request(), &cancel, &mut |text: &str| {
            pieces.push(text.to_owned());
            if pieces.len() == 2 {
                trigger.cancel();
            }
        })
        .await;
    assert_eq!(result.unwrap_err(), LlmError::Cancelled);
    assert_eq!(pieces.len(), 2);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(fake.wait_for_cancelled(1, Duration::from_secs(3)).await);
    assert_eq!(fake.request_count(), 1);

    // Cancelled before anything: no request is retried either.
    fake.push(Reply::Hang);
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        trigger.cancel();
    });
    let result = client
        .complete(&request(), &cancel, &mut |_: &str| {})
        .await;
    assert_eq!(result.unwrap_err(), LlmError::Cancelled);
    assert_eq!(fake.request_count(), 2);
}

#[tokio::test]
async fn the_key_never_shows() {
    let fake = FakeProvider::start().await;
    fake.push(Reply::Status {
        status: 401,
        retry_after: None,
        message: format!("Incorrect API key provided: {KEY}"),
    });
    fake.push(Reply::StreamError {
        pieces: vec![],
        message: format!("key {KEY} is blocked"),
    });
    let mut config = config(&fake);
    config.api_key = Secret::new(KEY);
    let client = LlmClient::new("fake", &config).unwrap();
    let debug = format!("{client:?}");
    assert!(!debug.contains(KEY), "{debug}");
    assert!(debug.contains("fake"));
    for _ in 0..2 {
        let (result, _) = run(&client, &request()).await;
        let error = result.unwrap_err();
        let text = format!("{error} {error:?}");
        assert!(!text.contains(KEY), "{text}");
        assert!(text.contains("[redacted]"), "{text}");
    }

    // Nor when the connection fails.
    let mut unreachable = config.clone();
    unreachable.base_url = "http://127.0.0.1:1/v1".to_owned();
    unreachable.max_retries = 0;
    let client = LlmClient::new("fake", &unreachable).unwrap();
    let (result, _) = run(&client, &request()).await;
    let error = result.unwrap_err();
    assert!(matches!(error, LlmError::Connect(_)), "{error:?}");
    assert!(!format!("{error} {error:?}").contains(KEY));
}

#[tokio::test]
async fn the_fake_lists_models() {
    let fake = FakeProvider::start().await;
    let body: Value = reqwest::get(format!("{}/models", fake.base_url()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["data"][0]["id"], "fake-model");
}
