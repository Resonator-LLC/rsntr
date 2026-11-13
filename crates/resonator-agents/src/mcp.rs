//! The Model Context Protocol endpoint, `POST /mcp`: wave tools for AI
//! clients, as the user (or robot) whose bearer token authenticates.
//!
//! Streamable HTTP with plain JSON responses, no sessions. Two eras:
//!
//! - **2026-07-28** (modern, stateless): every request carries
//!   `_meta["io.modelcontextprotocol/protocolVersion"]`; the headers
//!   `MCP-Protocol-Version`, `Mcp-Method` and (for `tools/call`)
//!   `Mcp-Name` must match the body (else 400, `-32020`); an unsupported
//!   version gets 400 and `-32022` with the supported versions; unknown
//!   methods get 404 and `-32601`; `server/discover` describes the server;
//!   results carry `resultType: "complete"` and the server's identity.
//! - **2025-11-25, 2025-06-18, 2025-03-26, 2024-11-05** (legacy):
//!   `initialize` negotiates the version, `ping` answers, no session id is
//!   minted (every request stands alone, which those revisions allow).
//!
//! `GET` and `DELETE` get 405. A request with an `Origin` that is not the
//! server's own or configured as allowed gets 403. Without a valid token:
//! 401. A token with only the `read` scope sees and may call only the
//! read tools.
//!
//! Text positions in tools are UTF-16 code units of the blip text as
//! `read_wave` shows it; matching text (`find`, `after_text`) is preferred.

use std::sync::Arc;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use resonator_robots::ops::Registry;
use resonator_server::http::{AppState, RequestContext};
use serde_json::{Value, json};

use crate::http::Shared;
use crate::tools::{self, Edit, ExecutorRunner, Runner};

/// The modern revision.
pub const MODERN: &str = "2026-07-28";
/// The legacy revisions, newest first.
pub const LEGACY: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_SERVER: &str = "io.modelcontextprotocol/serverInfo";

const INSTRUCTIONS: &str = "Tools for Resonator, a threaded conversation system. A wave is a \
conversation of messages called blips; blips have replies in threads. Start with search_waves \
or list_unread to find waves, read_wave to read one (blip ids are in [brackets]), then reply, \
edit or create. Edit text by quoting it ('find', 'after_text') rather than by offsets; offsets, \
where used, are UTF-16 code units of the blip text, the blip starting with a line break at 0.";

fn server_info() -> Value {
    json!({"name": "resonator", "version": env!("CARGO_PKG_VERSION")})
}

fn reply(status: StatusCode, body: Value) -> Response {
    let mut response = (status, body.to_string()).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn rpc_error(
    status: StatusCode,
    id: &Value,
    code: i64,
    message: &str,
    data: Option<Value>,
) -> Response {
    let mut error = json!({"code": code, "message": message});
    if let Some(data) = data {
        error["data"] = data;
    }
    let mut body = json!({"jsonrpc": "2.0", "error": error});
    if !id.is_null() {
        body["id"] = id.clone();
    }
    reply(status, body)
}

/// `GET` / `DELETE /mcp`.
pub async fn method_not_allowed() -> Response {
    let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST"));
    response
}

/// A tool: name, whether it changes anything, description, schema.
struct ToolDef {
    name: &'static str,
    writes: bool,
    description: &'static str,
    schema: fn() -> Value,
}

fn wave_param() -> Value {
    json!({"type": "string", "description": "The wave id, like example.com/w+abc123 (from search_waves)"})
}

const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "search_waves",
        writes: false,
        description: "Search your waves. The query is words to find, and/or in:inbox, with:name@domain, creator:name@domain. Returns wave ids, titles, snippets, unread counts.",
        schema: || {
            json!({"type": "object", "properties": {
            "query": {"type": "string"},
            "limit": {"type": "integer", "minimum": 1, "maximum": 50}
        }, "required": ["query"]})
        },
    },
    ToolDef {
        name: "read_wave",
        writes: false,
        description: "Read a wave as a threaded outline: [blip id] author, time, then the text; replies indented. Give blip_id for one blip or thread_id for one thread. Long waves are cut, with markers.",
        schema: || {
            json!({"type": "object", "properties": {
            "wave_id": wave_param(),
            "blip_id": {"type": "string"},
            "thread_id": {"type": "string"},
            "max_chars": {"type": "integer", "minimum": 500}
        }, "required": ["wave_id"]})
        },
    },
    ToolDef {
        name: "list_unread",
        writes: false,
        description: "Waves in your inbox with unread blips.",
        schema: || {
            json!({"type": "object", "properties": {
                "limit": {"type": "integer", "minimum": 1, "maximum": 50}
            }})
        },
    },
    ToolDef {
        name: "create_wave",
        writes: true,
        description: "Create a new wave. The title becomes its first line; text follows. Participants are addresses like name@example.com; you are added.",
        schema: || {
            json!({"type": "object", "properties": {
            "title": {"type": "string"},
            "text": {"type": "string"},
            "participants": {"type": "array", "items": {"type": "string"}}
        }, "required": ["title"]})
        },
    },
    ToolDef {
        name: "append_blip",
        writes: true,
        description: "Add a new blip at the end of a wave.",
        schema: || {
            json!({"type": "object", "properties": {
            "wave_id": wave_param(), "text": {"type": "string"}
        }, "required": ["wave_id", "text"]})
        },
    },
    ToolDef {
        name: "reply_to_blip",
        writes: true,
        description: "Reply to a blip: a new reply thread under it, or with continue_thread the next blip after it in its thread.",
        schema: || {
            json!({"type": "object", "properties": {
            "wave_id": wave_param(), "blip_id": {"type": "string"}, "text": {"type": "string"},
            "continue_thread": {"type": "boolean"}
        }, "required": ["wave_id", "blip_id", "text"]})
        },
    },
    ToolDef {
        name: "inline_reply",
        writes: true,
        description: "Reply inside a blip's text, anchored right after the first occurrence of after_text (quote it exactly).",
        schema: || {
            json!({"type": "object", "properties": {
            "wave_id": wave_param(), "blip_id": {"type": "string"},
            "after_text": {"type": "string"}, "text": {"type": "string"}
        }, "required": ["wave_id", "blip_id", "after_text", "text"]})
        },
    },
    ToolDef {
        name: "edit_blip",
        writes: true,
        description: "Edit a blip's text. Either find + replace_with (first occurrence, or all with replace_all), or start + end + replace_with (UTF-16 offsets, start >= 1), or append (a leading \\n starts a new line).",
        schema: || {
            json!({"type": "object", "properties": {
            "wave_id": wave_param(), "blip_id": {"type": "string"},
            "find": {"type": "string"}, "replace_with": {"type": "string"},
            "replace_all": {"type": "boolean"},
            "start": {"type": "integer"}, "end": {"type": "integer"},
            "append": {"type": "string"}
        }, "required": ["wave_id", "blip_id"]})
        },
    },
    ToolDef {
        name: "delete_blip",
        writes: true,
        description: "Delete a blip (and its replies).",
        schema: || {
            json!({"type": "object", "properties": {
            "wave_id": wave_param(), "blip_id": {"type": "string"}
        }, "required": ["wave_id", "blip_id"]})
        },
    },
    ToolDef {
        name: "set_title",
        writes: true,
        description: "Set a wave's title (its first line).",
        schema: || {
            json!({"type": "object", "properties": {
            "wave_id": wave_param(), "title": {"type": "string"}
        }, "required": ["wave_id", "title"]})
        },
    },
    ToolDef {
        name: "add_participant",
        writes: true,
        description: "Add a participant (name@example.com) to a wave.",
        schema: || {
            json!({"type": "object", "properties": {
            "wave_id": wave_param(), "address": {"type": "string"}
        }, "required": ["wave_id", "address"]})
        },
    },
    ToolDef {
        name: "remove_participant",
        writes: true,
        description: "Remove a participant from a wave.",
        schema: || {
            json!({"type": "object", "properties": {
            "wave_id": wave_param(), "address": {"type": "string"}
        }, "required": ["wave_id", "address"]})
        },
    },
    ToolDef {
        name: "mark_read",
        writes: true,
        description: "Mark a wave, or one blip of it, as read.",
        schema: || {
            json!({"type": "object", "properties": {
            "wave_id": wave_param(), "blip_id": {"type": "string"}
        }, "required": ["wave_id"]})
        },
    },
];

fn tool_list(can_write: bool) -> Vec<Value> {
    TOOLS
        .iter()
        .filter(|tool| can_write || !tool.writes)
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": (tool.schema)(),
                "annotations": {"readOnlyHint": !tool.writes, "destructiveHint": tool.name == "delete_blip" || tool.name == "remove_participant"},
            })
        })
        .collect()
}

/// Decodes `=?base64?…?=` header values.
fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?.trim();
    if let Some(encoded) = value
        .strip_prefix("=?base64?")
        .and_then(|v| v.strip_suffix("?="))
    {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        return String::from_utf8(bytes).ok();
    }
    Some(value.to_owned())
}

/// `POST /mcp`.
pub async fn handle(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Extension(shared): Extension<Arc<Shared>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let null = Value::Null;
    if headers.contains_key(header::ORIGIN) && !state.origin_allowed(&headers, context.trusted_peer)
    {
        return rpc_error(
            StatusCode::FORBIDDEN,
            &null,
            -32600,
            "Origin not allowed",
            None,
        );
    }
    let Some(identity) = context.identity else {
        let mut response = rpc_error(
            StatusCode::UNAUTHORIZED,
            &null,
            -32001,
            "Authentication required: send Authorization: Bearer <personal access token or robot token>",
            None,
        );
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return response;
    };
    let message: Value = match serde_json::from_slice(&body) {
        Ok(message) => message,
        Err(_) => return rpc_error(StatusCode::BAD_REQUEST, &null, -32700, "Parse error", None),
    };
    let Some(object) = message.as_object() else {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            &null,
            -32600,
            "Invalid request: send one JSON-RPC message per POST (no batches)",
            None,
        );
    };
    let method = object.get("method").and_then(Value::as_str).unwrap_or("");
    let id = object.get("id").cloned().unwrap_or(Value::Null);
    if method.is_empty() {
        // A response or garbage: nothing a client should send.
        return rpc_error(
            StatusCode::BAD_REQUEST,
            &id,
            -32600,
            "Invalid request",
            None,
        );
    }
    if !object.contains_key("id") {
        // Notifications (notifications/initialized, cancelled, …).
        return StatusCode::ACCEPTED.into_response();
    }
    let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
    let modern_version = params
        .get("_meta")
        .and_then(|meta| meta.get(META_VERSION))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let header_version = header_value(&headers, "mcp-protocol-version");
    let modern = modern_version.is_some();
    if let Some(version) = &modern_version {
        let mismatch = |what: String| {
            rpc_error(
                StatusCode::BAD_REQUEST,
                &id,
                -32020,
                &format!("Header mismatch: {what}"),
                None,
            )
        };
        match &header_version {
            None => return mismatch("the MCP-Protocol-Version header is missing".to_owned()),
            Some(header) if header != version => {
                return mismatch(format!(
                    "MCP-Protocol-Version header '{header}' does not match body value '{version}'"
                ));
            }
            _ => {}
        }
        match header_value(&headers, "mcp-method") {
            Some(header) if header == method => {}
            Some(header) => {
                return mismatch(format!(
                    "Mcp-Method header '{header}' does not match body value '{method}'"
                ));
            }
            None => return mismatch("the Mcp-Method header is missing".to_owned()),
        }
        if method == "tools/call" {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            match header_value(&headers, "mcp-name") {
                Some(header) if header == name => {}
                Some(header) => {
                    return mismatch(format!(
                        "Mcp-Name header '{header}' does not match body value '{name}'"
                    ));
                }
                None => return mismatch("the Mcp-Name header is missing".to_owned()),
            }
        }
        if version != MODERN {
            return unsupported(&id, version);
        }
    } else if let Some(version) = &header_version
        && method != "initialize"
        && version != MODERN
        && !LEGACY.contains(&version.as_str())
    {
        return unsupported(&id, version);
    }
    let can_write = identity.can_write();
    let finish = |result: Value| {
        let mut result = result;
        if modern && let Some(object) = result.as_object_mut() {
            object.insert("resultType".to_owned(), json!("complete"));
            object.insert("_meta".to_owned(), json!({META_SERVER: server_info()}));
        }
        reply(
            StatusCode::OK,
            json!({"jsonrpc": "2.0", "id": id, "result": result}),
        )
    };
    match method {
        "initialize" if !modern => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("");
            let version = if LEGACY.contains(&requested) {
                requested
            } else {
                LEGACY[0]
            };
            finish(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": server_info(),
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" if !modern => finish(json!({})),
        "server/discover" => {
            let mut supported = vec![MODERN];
            supported.extend(LEGACY);
            let mut result = json!({
                "supportedVersions": supported,
                "capabilities": {"tools": {}},
                "instructions": INSTRUCTIONS,
                "ttlMs": 3_600_000,
                "cacheScope": "public",
            });
            if !modern {
                result["serverInfo"] = server_info();
            }
            finish(result)
        }
        "tools/list" => {
            let mut result = json!({"tools": tool_list(can_write)});
            if modern {
                result["ttlMs"] = json!(60_000);
                result["cacheScope"] = json!("private");
            }
            finish(result)
        }
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let Some(tool) = TOOLS.iter().find(|tool| tool.name == name) else {
                return rpc_error(
                    StatusCode::OK,
                    &id,
                    -32602,
                    &format!("Unknown tool '{name}'; call tools/list for the available tools"),
                    None,
                );
            };
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let outcome = if tool.writes && !can_write {
                Err(format!(
                    "{name} changes waves, but this token has only the read scope; use a token with the write scope"
                ))
            } else {
                let runner = ExecutorRunner {
                    executor: &shared.executor,
                    actor: identity.participant.clone(),
                    registry: Registry::DataApi,
                };
                call_tool(&runner, &state, name, &arguments).await
            };
            let max = state.config.mcp_max_result_chars.max(1000);
            tracing::info!(participant = %identity.participant, tool = name, ok = outcome.is_ok(), "mcp tool call");
            let (text, is_error) = match outcome {
                Ok(text) => (tools::cap(&text, max), false),
                Err(error) => (error, true),
            };
            finish(json!({"content": [{"type": "text", "text": text}], "isError": is_error}))
        }
        _ => rpc_error(
            if modern {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::OK
            },
            &id,
            -32601,
            &format!("Method not found: {method}"),
            None,
        ),
    }
}

fn unsupported(id: &Value, requested: &str) -> Response {
    let mut supported = vec![MODERN];
    supported.extend(LEGACY);
    rpc_error(
        StatusCode::BAD_REQUEST,
        id,
        -32022,
        "Unsupported protocol version",
        Some(json!({"supported": supported, "requested": requested})),
    )
}

fn arg<'a>(arguments: &'a Value, key: &str) -> Option<&'a str> {
    arguments.get(key).and_then(Value::as_str)
}

fn required<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, String> {
    arg(arguments, key)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("the argument '{key}' is required"))
}

fn limit(arguments: &Value, default: i32) -> i32 {
    arguments
        .get("limit")
        .and_then(Value::as_i64)
        .map_or(default, |n| n.clamp(1, 50) as i32)
}

async fn call_tool(
    runner: &dyn Runner,
    state: &AppState,
    name: &str,
    arguments: &Value,
) -> Result<String, String> {
    if !arguments.is_object() {
        return Err("the arguments must be an object".to_owned());
    }
    let wavelet = || -> Result<(String, String), String> {
        tools::wavelet_of(required(arguments, "wave_id")?, None)
    };
    let max = state.config.mcp_max_result_chars.max(1000);
    let value = match name {
        "search_waves" => {
            tools::search(runner, required(arguments, "query")?, limit(arguments, 10)).await?
        }
        "list_unread" => tools::list_unread(runner, limit(arguments, 20)).await?,
        "read_wave" => {
            let (wave, wavelet) = wavelet()?;
            let budget = arguments
                .get("max_chars")
                .and_then(Value::as_u64)
                .map_or(max, |n| (n as usize).clamp(500, max));
            return tools::read_wave(
                runner,
                &wave,
                &wavelet,
                arg(arguments, "blip_id"),
                arg(arguments, "thread_id"),
                budget,
            )
            .await;
        }
        "create_wave" => {
            let participants: Vec<String> = arguments
                .get("participants")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            tools::create_wave(
                runner,
                &state.config.domain,
                required(arguments, "title")?,
                arg(arguments, "text").unwrap_or(""),
                &participants,
            )
            .await?
        }
        "append_blip" => {
            let (wave, wavelet) = wavelet()?;
            tools::append_blip(runner, &wave, &wavelet, required(arguments, "text")?).await?
        }
        "reply_to_blip" => {
            let (wave, wavelet) = wavelet()?;
            tools::reply(
                runner,
                &wave,
                &wavelet,
                required(arguments, "blip_id")?,
                required(arguments, "text")?,
                arguments
                    .get("continue_thread")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            )
            .await?
        }
        "inline_reply" => {
            let (wave, wavelet) = wavelet()?;
            tools::inline_reply(
                runner,
                &wave,
                &wavelet,
                required(arguments, "blip_id")?,
                required(arguments, "after_text")?,
                required(arguments, "text")?,
            )
            .await?
        }
        "edit_blip" => {
            let (wave, wavelet) = wavelet()?;
            let with = arg(arguments, "replace_with").unwrap_or("").to_owned();
            let edit = if let Some(find) = arg(arguments, "find") {
                Edit::Replace {
                    find: find.to_owned(),
                    with,
                    all: arguments
                        .get("replace_all")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                }
            } else if let (Some(start), Some(end)) = (
                arguments.get("start").and_then(Value::as_u64),
                arguments.get("end").and_then(Value::as_u64),
            ) {
                Edit::Range {
                    start: start as usize,
                    end: end as usize,
                    with,
                }
            } else if let Some(text) = arg(arguments, "append") {
                Edit::Append {
                    text: text.to_owned(),
                }
            } else {
                return Err(
                    "give find + replace_with, start + end + replace_with, or append".to_owned(),
                );
            };
            tools::edit_blip(
                runner,
                &wave,
                &wavelet,
                required(arguments, "blip_id")?,
                &edit,
            )
            .await?
        }
        "delete_blip" => {
            let (wave, wavelet) = wavelet()?;
            tools::delete_blip(runner, &wave, &wavelet, required(arguments, "blip_id")?).await?
        }
        "set_title" => {
            let (wave, wavelet) = wavelet()?;
            tools::set_title(runner, &wave, &wavelet, required(arguments, "title")?).await?
        }
        "add_participant" | "remove_participant" => {
            let (wave, wavelet) = wavelet()?;
            tools::participant(
                runner,
                &wave,
                &wavelet,
                required(arguments, "address")?,
                name == "add_participant",
            )
            .await?
        }
        "mark_read" => {
            let (wave, wavelet) = wavelet()?;
            tools::mark_read(runner, &wave, &wavelet, arg(arguments, "blip_id")).await?
        }
        _ => return Err(format!("unknown tool {name}")),
    };
    Ok(serde_json::to_string_pretty(&value).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_tokens_see_read_tools_only() {
        let read: Vec<String> = tool_list(false)
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(read, ["search_waves", "read_wave", "list_unread"]);
        assert_eq!(tool_list(true).len(), TOOLS.len());
        for tool in tool_list(true) {
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn encoded_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "mcp-name",
            HeaderValue::from_static("=?base64?cmVhZF93YXZl?="),
        );
        assert_eq!(
            header_value(&headers, "mcp-name").as_deref(),
            Some("read_wave")
        );
    }
}
