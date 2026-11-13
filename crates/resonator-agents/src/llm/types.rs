//! The messages of the Chat Completions API, as far as the agents use
//! them, and the accumulation of streamed responses.

use serde_json::{Map, Value, json};

use super::LlmError;

/// A tool call the model made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// The arguments as the model wrote them: JSON, if the model got it
    /// right.
    pub arguments: String,
}

/// One message of a conversation with a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    System(String),
    User(String),
    Assistant {
        content: String,
        tool_calls: Vec<ToolCall>,
    },
    /// The result of a tool call.
    Tool {
        tool_call_id: String,
        content: String,
    },
}

impl Message {
    pub fn to_json(&self) -> Value {
        match self {
            Message::System(content) => json!({"role": "system", "content": content}),
            Message::User(content) => json!({"role": "user", "content": content}),
            Message::Assistant {
                content,
                tool_calls,
            } => {
                let mut message = Map::new();
                message.insert("role".to_owned(), json!("assistant"));
                // With tool calls, an empty content is written as null,
                // which every server accepts; some reject "".
                message.insert(
                    "content".to_owned(),
                    if content.is_empty() && !tool_calls.is_empty() {
                        Value::Null
                    } else {
                        json!(content)
                    },
                );
                if !tool_calls.is_empty() {
                    let calls: Vec<Value> = tool_calls
                        .iter()
                        .map(|call| {
                            json!({
                                "id": call.id,
                                "type": "function",
                                "function": {"name": call.name, "arguments": call.arguments},
                            })
                        })
                        .collect();
                    message.insert("tool_calls".to_owned(), Value::Array(calls));
                }
                Value::Object(message)
            }
            Message::Tool {
                tool_call_id,
                content,
            } => json!({"role": "tool", "tool_call_id": tool_call_id, "content": content}),
        }
    }
}

/// A tool offered to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// The JSON Schema of the arguments.
    pub parameters: Value,
}

impl ToolSpec {
    pub fn to_json(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            },
        })
    }
}

/// A request for a completion.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    /// `tool_choice`, if not left to the provider's default (`auto`).
    pub tool_choice: Option<Value>,
    /// `max_tokens`; `None` leaves it to the provider.
    pub max_tokens: Option<u32>,
    /// Further members of the request body; they override the
    /// provider's own extra members.
    pub extra_body: Map<String, Value>,
}

/// Token counts, where the provider reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl Usage {
    fn from_json(value: &Value) -> Option<Usage> {
        let object = value.as_object()?;
        let count = |key: &str| object.get(key).and_then(Value::as_u64);
        let prompt = count("prompt_tokens").or_else(|| count("input_tokens"));
        let completion = count("completion_tokens").or_else(|| count("output_tokens"));
        if prompt.is_none() && completion.is_none() && count("total_tokens").is_none() {
            return None;
        }
        let (prompt, completion) = (prompt.unwrap_or(0), completion.unwrap_or(0));
        Some(Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: count("total_tokens").unwrap_or(prompt + completion),
        })
    }
}

/// A finished completion.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Completion {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    /// `stop`, `length`, `tool_calls`, ... as the provider says.
    pub finish_reason: Option<String>,
    /// `None` if the provider did not report usage.
    pub usage: Option<Usage>,
}

/// The text of a `content` value: a string, or an array of parts of which
/// the text parts count.
fn content_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(text) => Some(text.as_str()),
                Value::Object(part) => part.get("text").and_then(Value::as_str),
                _ => None,
            })
            .collect(),
        _ => String::new(),
    }
}

/// The message of an `error` member, which providers write as an object
/// with a `message` or as a string.
pub(crate) fn error_message(error: &Value) -> String {
    let text = match error {
        Value::String(message) => message.clone(),
        Value::Object(object) => {
            let message = object
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            match object.get("code") {
                Some(Value::String(code)) => format!("{message} ({code})"),
                Some(Value::Number(code)) => format!("{message} ({code})"),
                _ => message.to_owned(),
            }
        }
        other => other.to_string(),
    };
    truncate(&text, 300)
}

/// At most `max` characters of a text.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

#[derive(Debug, Default)]
struct PartialCall {
    index: Option<u64>,
    id: String,
    name: String,
    arguments: String,
}

/// Puts tool calls together from the fragments of a stream.
///
/// A call's first fragment has its id and name; the arguments come in
/// pieces. Fragments of several calls are told apart by `index`. Seen in
/// the wild and handled: no `index` at all (a new id starts a new call),
/// the id or name repeated on every fragment, the arguments sent whole as
/// an object, no id (one is made up), and no arguments (`{}`).
#[derive(Debug, Default)]
pub struct ToolCallAccumulator {
    calls: Vec<PartialCall>,
}

impl ToolCallAccumulator {
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// Adds one element of a delta's `tool_calls`.
    pub fn add(&mut self, fragment: &Value) {
        let Some(fragment) = fragment.as_object() else {
            return;
        };
        let index = fragment.get("index").and_then(Value::as_u64);
        let id = fragment.get("id").and_then(Value::as_str).unwrap_or("");
        let function = fragment.get("function").and_then(Value::as_object);
        let name = function
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let position = match index {
            Some(index) => self.calls.iter().position(|call| call.index == Some(index)),
            None => match self.calls.last() {
                Some(last) if id.is_empty() || last.id == id => Some(self.calls.len() - 1),
                _ => None,
            },
        };
        let call = match position {
            Some(position) => &mut self.calls[position],
            None => {
                self.calls.push(PartialCall {
                    index,
                    ..PartialCall::default()
                });
                self.calls.last_mut().expect("just pushed")
            }
        };
        if call.id.is_empty() {
            call.id = id.to_owned();
        }
        if call.name.is_empty() {
            call.name = name.to_owned();
        } else if !name.is_empty() && name != call.name {
            // A name that comes in pieces.
            call.name.push_str(name);
        }
        match function.and_then(|f| f.get("arguments")) {
            Some(Value::String(piece)) => call.arguments.push_str(piece),
            Some(Value::Null) | None => {}
            Some(whole) => call.arguments = whole.to_string(),
        }
    }

    /// The calls, in the order the model made them.
    pub fn finish(mut self) -> Vec<ToolCall> {
        // Calls are ordered by index where there is one; a stable sort
        // keeps the order of arrival otherwise.
        self.calls
            .sort_by_key(|call| call.index.unwrap_or(u64::MAX));
        self.calls
            .into_iter()
            .filter(|call| !call.name.trim().is_empty())
            .enumerate()
            .map(|(position, call)| ToolCall {
                id: if call.id.is_empty() {
                    format!("call_{position}")
                } else {
                    call.id
                },
                name: call.name.trim().to_owned(),
                arguments: if call.arguments.trim().is_empty() {
                    "{}".to_owned()
                } else {
                    call.arguments
                },
            })
            .collect()
    }
}

/// What one event of a stream means for the caller.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamStep {
    /// Text to show.
    pub text: String,
    /// The stream said it is done (`[DONE]`).
    pub done: bool,
}

/// Puts a completion together from the events of a stream.
#[derive(Debug, Default)]
pub struct StreamAccumulator {
    content: String,
    tool_calls: ToolCallAccumulator,
    finish_reason: Option<String>,
    usage: Option<Usage>,
    /// Whether anything that looks like a completion chunk was seen.
    seen_chunk: bool,
}

impl StreamAccumulator {
    /// Handles the `data` of one event.
    pub fn on_data(&mut self, data: &str) -> Result<StreamStep, LlmError> {
        let data = data.trim();
        if data.is_empty() {
            return Ok(StreamStep::default());
        }
        if data == "[DONE]" {
            return Ok(StreamStep {
                text: String::new(),
                done: true,
            });
        }
        let chunk: Value = serde_json::from_str(data)
            .map_err(|error| LlmError::Protocol(format!("a stream event is not JSON ({error})")))?;
        // An error object in the middle of the stream. OpenRouter sends
        // it together with a choice whose finish_reason is "error".
        if let Some(error) = chunk.get("error").filter(|error| !error.is_null()) {
            return Err(LlmError::Provider(error_message(error)));
        }
        if let Some(usage) = chunk.get("usage").and_then(Usage::from_json) {
            self.usage = Some(usage);
        }
        let mut step = StreamStep::default();
        let choices = chunk.get("choices").and_then(Value::as_array);
        if choices.is_some() {
            self.seen_chunk = true;
        }
        // Only the first choice: `n` is never asked for.
        let Some(choice) = choices.and_then(|choices| {
            choices
                .iter()
                .find(|choice| choice.get("index").and_then(Value::as_u64).unwrap_or(0) == 0)
        }) else {
            return Ok(step);
        };
        // Some servers send whole `message`s in a stream.
        let delta = choice.get("delta").or_else(|| choice.get("message"));
        if let Some(delta) = delta {
            if let Some(content) = delta.get("content") {
                let text = content_text(content);
                self.content.push_str(&text);
                step.text = text;
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    self.tool_calls.add(call);
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            if reason == "error" {
                return Err(LlmError::Provider(
                    "the provider ended the response with an error".to_owned(),
                ));
            }
            self.finish_reason = Some(reason.to_owned());
        }
        Ok(step)
    }

    /// Whether the stream has produced anything a completion could be
    /// made of.
    pub fn has_output(&self) -> bool {
        self.seen_chunk
    }

    /// The text so far.
    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn finish(self) -> Completion {
        Completion {
            content: self.content,
            tool_calls: self.tool_calls.finish(),
            finish_reason: self.finish_reason,
            usage: self.usage,
        }
    }
}

/// Reads a response that is not a stream.
pub fn parse_completion(body: &str) -> Result<Completion, LlmError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|error| LlmError::Protocol(format!("the response is not JSON ({error})")))?;
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        return Err(LlmError::Provider(error_message(error)));
    }
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| LlmError::Protocol("the response has no choices".to_owned()))?;
    let message = choice
        .get("message")
        .ok_or_else(|| LlmError::Protocol("the response has no message".to_owned()))?;
    let mut tool_calls = ToolCallAccumulator::default();
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (index, call) in calls.iter().enumerate() {
            // Whole calls have no index; give each its own.
            let mut call = call.clone();
            if let Some(object) = call.as_object_mut() {
                object.insert("index".to_owned(), json!(index));
            }
            tool_calls.add(&call);
        }
    }
    Ok(Completion {
        content: message.get("content").map(content_text).unwrap_or_default(),
        tool_calls: tool_calls.finish(),
        finish_reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_owned),
        usage: value.get("usage").and_then(Usage::from_json),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accumulate(events: &[&str]) -> Result<(Completion, String, bool), LlmError> {
        let mut accumulator = StreamAccumulator::default();
        let mut shown = String::new();
        let mut done = false;
        for event in events {
            let step = accumulator.on_data(event)?;
            shown.push_str(&step.text);
            done |= step.done;
        }
        Ok((accumulator.finish(), shown, done))
    }

    #[test]
    fn text_is_put_together() {
        let (completion, shown, done) = accumulate(&[
            r#"{"id":"1","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
            r#"{"id":"1","choices":[{"index":0,"delta":{"content":"Hel"},"finish_reason":null}]}"#,
            r#"{"id":"1","choices":[{"index":0,"delta":{"content":"lo"},"finish_reason":null}]}"#,
            r#"{"id":"1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            r#"{"id":"1","choices":[],"usage":{"prompt_tokens":12,"completion_tokens":2,"total_tokens":14}}"#,
            "[DONE]",
        ])
        .unwrap();
        assert_eq!(completion.content, "Hello");
        assert_eq!(shown, "Hello");
        assert!(done);
        assert_eq!(completion.finish_reason.as_deref(), Some("stop"));
        assert_eq!(
            completion.usage,
            Some(Usage {
                prompt_tokens: 12,
                completion_tokens: 2,
                total_tokens: 14
            })
        );
        assert!(completion.tool_calls.is_empty());
    }

    #[test]
    fn odd_chunks_are_tolerated() {
        // No usage, null content, content as parts, reasoning deltas,
        // empty data, a second choice, and no [DONE].
        let (completion, shown, done) = accumulate(&[
            "",
            r#"{"choices":[{"delta":{"content":null,"reasoning":"hmm"}}]}"#,
            r#"{"choices":[{"delta":{"content":[{"type":"text","text":"A"},{"type":"image"}]}}]}"#,
            r#"{"choices":[{"index":1,"delta":{"content":"IGNORED"}},{"index":0,"delta":{"content":"B"}}]}"#,
            r#"{"object":"something.else"}"#,
            r#"{"choices":[{"delta":{"content":"C"},"finish_reason":"length"}],"usage":null}"#,
        ])
        .unwrap();
        assert_eq!(completion.content, "ABC");
        assert_eq!(shown, "ABC");
        assert!(!done);
        assert_eq!(completion.finish_reason.as_deref(), Some("length"));
        assert_eq!(completion.usage, None);
    }

    #[test]
    fn errors_in_the_stream_are_errors() {
        let error = accumulate(&[
            r#"{"choices":[{"delta":{"content":"Hi"}}]}"#,
            r#"{"error":{"message":"Provider disconnected","code":502}}"#,
        ])
        .unwrap_err();
        assert_eq!(
            error,
            LlmError::Provider("Provider disconnected (502)".to_owned())
        );
        let error = accumulate(&[r#"{"error":"overloaded"}"#]).unwrap_err();
        assert_eq!(error, LlmError::Provider("overloaded".to_owned()));
        let error =
            accumulate(&[r#"{"choices":[{"delta":{"content":""},"finish_reason":"error"}]}"#])
                .unwrap_err();
        assert!(matches!(error, LlmError::Provider(_)));
        let error = accumulate(&["{not json"]).unwrap_err();
        assert!(matches!(error, LlmError::Protocol(_)));
        // A null error member is not an error.
        assert!(accumulate(&[r#"{"error":null,"choices":[{"delta":{"content":"x"}}]}"#]).is_ok());
    }

    fn calls(fragments: &[&str]) -> Vec<ToolCall> {
        let mut accumulator = ToolCallAccumulator::default();
        for fragment in fragments {
            accumulator.add(&serde_json::from_str(fragment).unwrap());
        }
        accumulator.finish()
    }

    fn call(id: &str, name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
        }
    }

    #[test]
    fn tool_call_fragments_are_accumulated() {
        // The usual shape.
        assert_eq!(
            calls(&[
                r#"{"index":0,"id":"call_a","type":"function","function":{"name":"read_blip","arguments":""}}"#,
                r#"{"index":0,"function":{"arguments":"{\"blip"}}"#,
                r#"{"index":0,"function":{"arguments":"_id\":\"b+1\"}"}}"#,
            ]),
            [call("call_a", "read_blip", r#"{"blip_id":"b+1"}"#)]
        );
        // Two calls in parallel, their fragments interleaved.
        assert_eq!(
            calls(&[
                r#"{"index":0,"id":"a","function":{"name":"one","arguments":"{\"x\":"}}"#,
                r#"{"index":1,"id":"b","function":{"name":"two","arguments":"{\"y\":"}}"#,
                r#"{"index":0,"function":{"arguments":"1}"}}"#,
                r#"{"index":1,"function":{"arguments":"2}"}}"#,
            ]),
            [
                call("a", "one", r#"{"x":1}"#),
                call("b", "two", r#"{"y":2}"#)
            ]
        );
        // Indices out of order of arrival.
        assert_eq!(
            calls(&[
                r#"{"index":1,"id":"b","function":{"name":"two","arguments":"{}"}}"#,
                r#"{"index":0,"id":"a","function":{"name":"one","arguments":"{}"}}"#,
            ]),
            [call("a", "one", "{}"), call("b", "two", "{}")]
        );
        // No index: a new id starts a new call, fragments without an id
        // continue the last one.
        assert_eq!(
            calls(&[
                r#"{"id":"a","function":{"name":"one","arguments":"{\"x\""}}"#,
                r#"{"function":{"arguments":":1}"}}"#,
                r#"{"id":"b","function":{"name":"two","arguments":"{}"}}"#,
            ]),
            [call("a", "one", r#"{"x":1}"#), call("b", "two", "{}")]
        );
        // Id and name repeated on every fragment.
        assert_eq!(
            calls(&[
                r#"{"index":0,"id":"a","function":{"name":"one","arguments":"{"}}"#,
                r#"{"index":0,"id":"a","function":{"name":"one","arguments":"}"}}"#,
            ]),
            [call("a", "one", "{}")]
        );
        // The name in pieces.
        assert_eq!(
            calls(&[
                r#"{"index":0,"id":"a","function":{"name":"read_"}}"#,
                r#"{"index":0,"function":{"name":"blip","arguments":"{}"}}"#,
            ]),
            [call("a", "read_blip", "{}")]
        );
        // Arguments as an object, no id, no arguments, null arguments.
        assert_eq!(
            calls(&[
                r#"{"index":0,"function":{"name":"one","arguments":{"x":1}}}"#,
                r#"{"index":1,"id":"b","function":{"name":"two"}}"#,
                r#"{"index":2,"id":"c","function":{"name":"three","arguments":null}}"#,
            ]),
            [
                call("call_0", "one", r#"{"x":1}"#),
                call("b", "two", "{}"),
                call("c", "three", "{}")
            ]
        );
        // Fragments without a name are dropped; junk is ignored.
        assert_eq!(
            calls(&[
                r#"{"index":0,"id":"a","function":{"arguments":"{}"}}"#,
                "7",
                "null"
            ]),
            []
        );
    }

    #[test]
    fn streamed_tool_calls_end_up_in_the_completion() {
        let (completion, shown, _) = accumulate(&[
            r#"{"choices":[{"delta":{"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"search","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"query\":\"in:inbox\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ])
        .unwrap();
        assert_eq!(shown, "");
        assert_eq!(completion.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(
            completion.tool_calls,
            [call("call_1", "search", r#"{"query":"in:inbox"}"#)]
        );
    }

    #[test]
    fn whole_responses_are_read() {
        let completion = parse_completion(
            r#"{"id":"x","choices":[{"index":0,"message":{"role":"assistant","content":"Hi there"},
                "finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#,
        )
        .unwrap();
        assert_eq!(completion.content, "Hi there");
        assert_eq!(completion.usage.unwrap().total_tokens, 5);

        let completion = parse_completion(
            r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[
                {"id":"a","type":"function","function":{"name":"one","arguments":"{\"x\":1}"}},
                {"id":"b","type":"function","function":{"name":"two","arguments":{"y":2}}}]},
                "finish_reason":"tool_calls"}]}"#,
        )
        .unwrap();
        assert_eq!(completion.content, "");
        assert_eq!(
            completion.tool_calls,
            [
                call("a", "one", r#"{"x":1}"#),
                call("b", "two", r#"{"y":2}"#)
            ]
        );
        assert_eq!(completion.usage, None);

        assert_eq!(
            parse_completion(r#"{"error":{"message":"bad key","code":"invalid_api_key"}}"#),
            Err(LlmError::Provider("bad key (invalid_api_key)".to_owned()))
        );
        assert!(matches!(parse_completion("{}"), Err(LlmError::Protocol(_))));
        assert!(matches!(
            parse_completion("<html>"),
            Err(LlmError::Protocol(_))
        ));
    }

    #[test]
    fn messages_are_written_as_the_api_wants() {
        assert_eq!(
            Message::System("s".to_owned()).to_json(),
            json!({"role": "system", "content": "s"})
        );
        assert_eq!(
            Message::Assistant {
                content: String::new(),
                tool_calls: vec![call("a", "one", "{}")]
            }
            .to_json(),
            json!({"role": "assistant", "content": null, "tool_calls": [
                {"id": "a", "type": "function", "function": {"name": "one", "arguments": "{}"}}
            ]})
        );
        assert_eq!(
            Message::Assistant {
                content: "hi".to_owned(),
                tool_calls: vec![]
            }
            .to_json(),
            json!({"role": "assistant", "content": "hi"})
        );
        assert_eq!(
            Message::Tool {
                tool_call_id: "a".to_owned(),
                content: "r".to_owned()
            }
            .to_json(),
            json!({"role": "tool", "tool_call_id": "a", "content": "r"})
        );
    }
}
