//! The tools an agent's model may call. `wave`: read a blip, a thread or
//! the whole wave it is answering in, reply elsewhere in that wave, edit
//! its own blips. `all`: also search and read the agent's other waves,
//! create waves and add participants. Everything runs as the agent, so the
//! server's access checks apply on top of these rules.

use resonator_server::config::AgentTools;
use serde_json::{Value, json};

use crate::llm::ToolSpec;
use crate::tools::{self, Edit, Runner};

/// Where the agent is answering.
#[derive(Debug, Clone)]
pub struct Scope {
    pub wave: String,
    pub wavelet: String,
    pub domain: String,
    pub tools: AgentTools,
    /// Characters a read may return.
    pub read_budget: usize,
}

fn spec(name: &str, description: &str, parameters: Value) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters,
    }
}

/// The tools offered for a scope.
pub fn specs(tools: AgentTools) -> Vec<ToolSpec> {
    if tools == AgentTools::None {
        return Vec::new();
    }
    let all = tools == AgentTools::All;
    let wave_param = if all {
        json!({"type": "string", "description": "A wave id like example.com/w+abc; default: the wave you are answering in"})
    } else {
        json!({"type": "string", "description": "Leave out: only the wave you are answering in is available"})
    };
    let mut out = vec![
        spec(
            "read_wave",
            "Read a wave as an outline (blip ids, authors, times, text). Give blip_id for one blip in full, or thread_id for a thread.",
            json!({"type": "object", "properties": {
                "wave_id": wave_param,
                "blip_id": {"type": "string"},
                "thread_id": {"type": "string"}
            }}),
        ),
        spec(
            "reply_to_blip",
            "Post a reply to a blip of the wave you are answering in (other than your main reply, which is your final message).",
            json!({"type": "object", "properties": {
                "blip_id": {"type": "string"},
                "text": {"type": "string"}
            }, "required": ["blip_id", "text"]}),
        ),
        spec(
            "edit_own_blip",
            "Edit one of your own blips in this wave: replace the first occurrence of 'find' with 'replace_with', or add 'append' at its end.",
            json!({"type": "object", "properties": {
                "blip_id": {"type": "string"},
                "find": {"type": "string"},
                "replace_with": {"type": "string"},
                "append": {"type": "string"}
            }, "required": ["blip_id"]}),
        ),
    ];
    if all {
        out.extend([
            spec(
                "search_waves",
                "Search the waves you are a participant of. Query words, or in:inbox, with:address, creator:address, title:word, label:N, after:YYYY-MM-DD, before:YYYY-MM-DD; -term excludes, OR between groups.",
                json!({"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}),
            ),
            spec(
                "create_wave",
                "Create a new wave with a title and text, with you and the given participants.",
                json!({"type": "object", "properties": {
                    "title": {"type": "string"},
                    "text": {"type": "string"},
                    "participants": {"type": "array", "items": {"type": "string"}}
                }, "required": ["title"]}),
            ),
            spec(
                "add_participant",
                "Add a participant (an address like name@example.com) to a wave.",
                json!({"type": "object", "properties": {
                    "wave_id": {"type": "string"},
                    "address": {"type": "string"}
                }, "required": ["address"]}),
            ),
        ]);
    }
    out
}

fn text(arguments: &Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn required(arguments: &Value, key: &str) -> Result<String, String> {
    text(arguments, key).ok_or_else(|| format!("the argument '{key}' is missing"))
}

/// The wavelet a call is about: the current one unless `all` allows
/// another.
fn target(scope: &Scope, arguments: &Value) -> Result<(String, String), String> {
    match text(arguments, "wave_id").filter(|w| !w.trim().is_empty()) {
        Some(wave) if wave.trim() != scope.wave => {
            if scope.tools != AgentTools::All {
                return Err("you can only use the wave you are answering in".to_owned());
            }
            tools::wavelet_of(&wave, None)
        }
        _ => Ok((scope.wave.clone(), scope.wavelet.clone())),
    }
}

/// Runs a tool call; the result is text for the model.
pub async fn call(runner: &dyn Runner, scope: &Scope, name: &str, arguments: &str) -> String {
    let arguments: Value = match serde_json::from_str(if arguments.trim().is_empty() {
        "{}"
    } else {
        arguments
    }) {
        Ok(value @ Value::Object(_)) => value,
        _ => return "Error: the arguments must be a JSON object".to_owned(),
    };
    match run(runner, scope, name, &arguments).await {
        Ok(text) => text,
        Err(error) => format!("Error: {error}"),
    }
}

async fn run(
    runner: &dyn Runner,
    scope: &Scope,
    name: &str,
    arguments: &Value,
) -> Result<String, String> {
    let all = scope.tools == AgentTools::All;
    let wave_tool = scope.tools != AgentTools::None;
    match name {
        "read_wave" if wave_tool => {
            let (wave, wavelet) = target(scope, arguments)?;
            tools::read_wave(
                runner,
                &wave,
                &wavelet,
                text(arguments, "blip_id").as_deref(),
                text(arguments, "thread_id").as_deref(),
                scope.read_budget,
            )
            .await
        }
        "reply_to_blip" if wave_tool => {
            let blip = required(arguments, "blip_id")?;
            let body = required(arguments, "text")?;
            let result =
                tools::reply(runner, &scope.wave, &scope.wavelet, &blip, &body, false).await?;
            Ok(result.to_string())
        }
        "edit_own_blip" if wave_tool => {
            let blip = required(arguments, "blip_id")?;
            let view = tools::fetch(runner, &scope.wave, &scope.wavelet).await?;
            let own = view
                .blips
                .get(&blip)
                .is_some_and(|b| b.creator == runner.actor().address());
            if !own {
                return Err(format!("blip {blip} is not one of your blips in this wave"));
            }
            let edit = if let Some(find) = text(arguments, "find") {
                Edit::Replace {
                    find,
                    with: text(arguments, "replace_with").unwrap_or_default(),
                    all: false,
                }
            } else if let Some(append) = text(arguments, "append") {
                Edit::Append { text: append }
            } else {
                return Err("give 'find' and 'replace_with', or 'append'".to_owned());
            };
            let result =
                tools::edit_blip(runner, &scope.wave, &scope.wavelet, &blip, &edit).await?;
            Ok(result.to_string())
        }
        "search_waves" if all => {
            let query = required(arguments, "query")?;
            Ok(tools::search(runner, &query, 10).await?.to_string())
        }
        "create_wave" if all => {
            let title = required(arguments, "title")?;
            let body = text(arguments, "text").unwrap_or_default();
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
            let result =
                tools::create_wave(runner, &scope.domain, &title, &body, &participants).await?;
            Ok(result.to_string())
        }
        "add_participant" if all => {
            let (wave, wavelet) = target(scope, arguments)?;
            let address = required(arguments, "address")?;
            Ok(tools::participant(runner, &wave, &wavelet, &address, true)
                .await?
                .to_string())
        }
        _ => Err(format!("there is no tool '{name}' available to you")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_sets() {
        assert!(specs(AgentTools::None).is_empty());
        let names = |t| specs(t).into_iter().map(|s| s.name).collect::<Vec<_>>();
        assert_eq!(
            names(AgentTools::Wave),
            ["read_wave", "reply_to_blip", "edit_own_blip"]
        );
        assert_eq!(names(AgentTools::All).len(), 6);
    }

    #[test]
    fn wave_scope_cannot_target_other_waves() {
        let scope = Scope {
            wave: "x.org/w+a".to_owned(),
            wavelet: "x.org/conv+root".to_owned(),
            domain: "x.org".to_owned(),
            tools: AgentTools::Wave,
            read_budget: 1000,
        };
        assert!(target(&scope, &json!({"wave_id": "x.org/w+b"})).is_err());
        assert_eq!(
            target(&scope, &json!({"wave_id": "x.org/w+a"})).unwrap().0,
            "x.org/w+a"
        );
        let all = Scope {
            tools: AgentTools::All,
            ..scope
        };
        assert_eq!(
            target(&all, &json!({"wave_id": "x.org/w+b"})).unwrap().0,
            "x.org/w+b"
        );
    }
}
