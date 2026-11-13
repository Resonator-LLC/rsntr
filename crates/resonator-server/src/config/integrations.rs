//! The `[ai]` section of the configuration: model providers and the
//! built-in agents. The settings are declared here, next to the rest of
//! the configuration, so that there is one file and one `Config`; what
//! they mean is implemented by the `resonator-agents` crate.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize, Serializer};

/// What is printed in place of a secret.
pub const REDACTED: &str = "<redacted>";

/// A secret setting: read from the file like any string, never printed.
/// `resonator config` and `Debug` show [`REDACTED`] instead.
#[derive(Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// The secret itself.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(if self.0.is_empty() { "" } else { REDACTED })
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_empty() {
            "Secret(empty)"
        } else {
            "Secret(..)"
        })
    }
}

/// Whether a header with this name is likely to carry a credential.
pub fn is_sensitive_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["auth", "key", "token", "secret", "cookie", "password"]
        .iter()
        .any(|word| name.contains(word))
}

fn serialize_headers<S: Serializer>(
    headers: &BTreeMap<String, String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_map(headers.iter().map(|(name, value)| {
        let shown = if is_sensitive_header(name) && !value.is_empty() {
            REDACTED
        } else {
            value.as_str()
        };
        (name, shown)
    }))
}

/// Parameters passed through to a provider as they are: a TOML table that
/// becomes part of the JSON request body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExtraParams(pub toml::Table);

// A table can hold floats, which have no total equality; configurations
// are compared only to tell whether they are the same text.
impl Eq for ExtraParams {}

impl ExtraParams {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Which tools the model behind an agent may call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentTools {
    /// None: the agent only writes its reply.
    None,
    /// Reading more of, replying elsewhere in, and editing its own blips
    /// in the wave it was triggered in. The default.
    #[default]
    Wave,
    /// Also searching and reading the other waves the agent is a
    /// participant of, creating waves and adding participants.
    All,
}

/// An OpenAI-compatible model provider (`[ai.providers.<name>]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AiProviderConfig {
    /// The base URL of the API, up to and including the version segment:
    /// requests go to `<base_url>/chat/completions`.
    pub base_url: String,
    /// The API key. Prefer `api_key_env`, which keeps the key out of the
    /// file.
    pub api_key: Secret,
    /// The name of the environment variable that holds the API key; read
    /// when the server starts. If neither this nor `api_key` is set, the
    /// variable `RESONATOR_AI_<NAME>_API_KEY` is tried (the provider's
    /// name in upper case, dashes as underscores); without any key no
    /// `Authorization` header is sent, which suits local servers.
    pub api_key_env: String,
    /// Extra request headers, e.g. OpenRouter's `HTTP-Referer` and
    /// `X-Title`.
    #[serde(serialize_with = "serialize_headers")]
    pub headers: BTreeMap<String, String>,
    /// Extra members of the JSON request body, passed through as they
    /// are (providers differ): `extra_body = { temperature = 0.7 }`.
    pub extra_body: ExtraParams,
    /// Whether to ask for a streamed response (`stream: true`). Without
    /// it the reply appears in one piece.
    pub stream: bool,
    /// Whether to ask for token counts on streamed responses
    /// (`stream_options.include_usage`). Turn off for servers that reject
    /// the parameter.
    pub include_usage: bool,
    /// Milliseconds to wait for the connection.
    pub connect_timeout_ms: u64,
    /// Seconds to wait for the first byte of the response.
    pub first_byte_timeout_secs: u64,
    /// Seconds a streamed response may be silent.
    pub idle_timeout_secs: u64,
    /// Seconds one request may take in all.
    pub request_timeout_secs: u64,
    /// Retries of a request that failed with 429, a 5xx status or a
    /// connection error before anything was received.
    pub max_retries: u32,
    /// Milliseconds before the first retry; doubles with each one. A
    /// `Retry-After` header takes precedence, up to `retry_max_wait_secs`.
    pub retry_backoff_ms: u64,
    /// The longest wait before a retry, in seconds.
    pub retry_max_wait_secs: u64,
}

impl Default for AiProviderConfig {
    fn default() -> Self {
        AiProviderConfig {
            base_url: String::new(),
            api_key: Secret::default(),
            api_key_env: String::new(),
            headers: BTreeMap::new(),
            extra_body: ExtraParams::default(),
            stream: true,
            include_usage: true,
            connect_timeout_ms: 10_000,
            first_byte_timeout_secs: 60,
            idle_timeout_secs: 60,
            request_timeout_secs: 600,
            max_retries: 2,
            retry_backoff_ms: 500,
            retry_max_wait_secs: 30,
        }
    }
}

/// A built-in agent (`[[ai.agents]]`): a participant `<name>@<domain>`
/// that answers through a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AiAgentConfig {
    /// The local part of the agent's address.
    pub name: String,
    /// The name of one of `[ai.providers]`.
    pub provider: String,
    /// The model, as the provider names it.
    pub model: String,
    /// Replaces the built-in description of who the agent is. The rules
    /// about Wave, plain output and untrusted content are always added.
    pub system_prompt: String,
    /// `none`, `wave` (this wave only) or `all`.
    pub tools: AgentTools,
    /// Most tokens of one model response (`max_tokens`); 0 leaves it to
    /// the provider.
    pub max_output_tokens: u32,
    /// Most characters of the conversation sent to the model.
    pub context_chars: usize,
    /// Most rounds of tool calls in one response.
    pub max_tool_iterations: u32,
    /// What the agent posts when it is added to a wave nobody has written
    /// in; empty for nothing.
    pub greeting: String,
    /// Extra members of the JSON request body for this agent; they
    /// override the provider's `extra_body`.
    pub extra_body: ExtraParams,
}

impl Default for AiAgentConfig {
    fn default() -> Self {
        AiAgentConfig {
            name: String::new(),
            provider: String::new(),
            model: String::new(),
            system_prompt: String::new(),
            tools: AgentTools::Wave,
            max_output_tokens: 2048,
            context_chars: 24_000,
            max_tool_iterations: 6,
            greeting: String::new(),
            extra_body: ExtraParams::default(),
        }
    }
}

/// The `[ai]` section. Without agents nothing here has any effect: no
/// account is created and no request is ever made.
///
/// Privacy: an agent sends the content of a wave to its provider only if
/// the agent is a participant of that wave, and only when it is triggered
/// (added to the wave, or one of its blips directly replied to). The log
/// names agent, wave, provider, model and token counts, never content or
/// keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AiConfig {
    /// Turns every agent off without removing the configuration.
    pub enabled: bool,
    /// Responses that may be written at the same time, over all agents;
    /// further ones wait their turn.
    pub max_concurrent_responses: usize,
    /// Responses that may wait for their turn; beyond that triggers are
    /// refused like a rate limit.
    pub max_queued_responses: usize,
    /// Responses started per minute over all agents and users; 0 for no
    /// limit.
    pub requests_per_minute: u32,
    /// Responses started per day (UTC) over all agents and users; 0 for
    /// no limit.
    pub daily_requests: u32,
    /// Responses one user may trigger per minute; 0 for no limit.
    pub user_requests_per_minute: u32,
    /// Responses one user may trigger per day (UTC); 0 for no limit.
    pub user_daily_requests: u32,
    /// What happens to a trigger over a limit: a short note in the reply
    /// (`true`) or no reply at all (`false`).
    pub limit_notice: bool,
    /// Milliseconds of quiet before an agent answers: a reply to it must
    /// have been left alone that long, and so must a wave it was just
    /// added to. People type bit by bit, and nothing says when they are
    /// done.
    pub reply_debounce_ms: u64,
    /// How many times an agent may redo one answer because the message it
    /// answered (or, for being added to a wave, the human-written text of
    /// the wave) was edited afterwards; 0 never redoes an answer. Every
    /// redo counts against the limits like any other response.
    pub max_reanswers: u32,
    /// Milliseconds of streamed text gathered into one edit of the reply
    /// blip.
    pub stream_batch_ms: u64,
    /// The model providers, by name.
    pub providers: BTreeMap<String, AiProviderConfig>,
    /// The agents.
    pub agents: Vec<AiAgentConfig>,
}

impl Default for AiConfig {
    fn default() -> Self {
        AiConfig {
            enabled: true,
            max_concurrent_responses: 4,
            max_queued_responses: 32,
            requests_per_minute: 30,
            daily_requests: 0,
            user_requests_per_minute: 6,
            user_daily_requests: 200,
            limit_notice: true,
            reply_debounce_ms: 10_000,
            max_reanswers: 5,
            stream_batch_ms: 150,
            providers: BTreeMap::new(),
            agents: Vec::new(),
        }
    }
}

/// Whether a name can be the local part of an agent's or robot's address:
/// lower-case letters, digits, dots, dashes and underscores, starting
/// with a letter or digit. `+` and `#`, which the robot address syntax
/// allows, mark proxies and versions and cannot be part of an account's
/// name.
pub fn is_valid_robot_id(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
}

impl AiConfig {
    /// Whether any agent is to run.
    pub fn is_active(&self) -> bool {
        self.enabled && !self.agents.is_empty()
    }

    /// Checks the section; the error is the message for the operator.
    pub fn validate(&self) -> Result<(), String> {
        for (name, provider) in &self.providers {
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Err(format!(
                    "ai.providers: '{name}' is not a valid provider name (letters, digits, - and _)"
                ));
            }
            let authority = provider
                .base_url
                .strip_prefix("http://")
                .or_else(|| provider.base_url.strip_prefix("https://"));
            if !authority.is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/')) {
                return Err(format!(
                    "ai.providers.{name}: base_url '{}' is not an http(s) URL",
                    provider.base_url
                ));
            }
            for (header, value) in &provider.headers {
                let valid_name = !header.is_empty()
                    && header
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c));
                if !valid_name || value.chars().any(|c| c.is_control()) {
                    return Err(format!(
                        "ai.providers.{name}: '{header}' is not a valid header"
                    ));
                }
            }
            if provider.request_timeout_secs == 0 || provider.idle_timeout_secs == 0 {
                return Err(format!(
                    "ai.providers.{name}: timeouts must be at least 1 second"
                ));
            }
        }
        let mut names = std::collections::BTreeSet::new();
        for agent in &self.agents {
            if !is_valid_robot_id(&agent.name) {
                return Err(format!(
                    "ai.agents: '{}' is not a valid agent name (lower-case letters, digits, \
                     '.', '-' and '_')",
                    agent.name
                ));
            }
            if !names.insert(agent.name.as_str()) {
                return Err(format!("ai.agents: two agents are named '{}'", agent.name));
            }
            if !self.providers.contains_key(&agent.provider) {
                return Err(format!(
                    "ai.agents.{}: there is no provider '{}' in [ai.providers]",
                    agent.name, agent.provider
                ));
            }
            if agent.model.trim().is_empty() {
                return Err(format!("ai.agents.{}: model must be set", agent.name));
            }
            if agent.context_chars < 1000 {
                return Err(format!(
                    "ai.agents.{}: context_chars must be at least 1000",
                    agent.name
                ));
            }
        }
        if self.max_concurrent_responses == 0 {
            return Err("ai.max_concurrent_responses must be at least 1".to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
[providers.openrouter]
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"
headers = { "HTTP-Referer" = "https://wave.example.com", "X-Title" = "Resonator", "X-Api-Key" = "k" }
extra_body = { temperature = 0.5, provider = { sort = "throughput" } }

[providers.abliteration]
base_url = "https://api.abliteration.ai/v1"
api_key = "ak_secret"

[providers.local]
base_url = "http://127.0.0.1:11434/v1"

[[agents]]
name = "assistant"
provider = "openrouter"
model = "anthropic/claude-sonnet-4.5"

[[agents]]
name = "local-llama"
provider = "local"
model = "llama3.2"
tools = "all"
max_output_tokens = 512
"#;

    #[test]
    fn the_example_parses_with_defaults() {
        let ai: AiConfig = toml::from_str(EXAMPLE).unwrap();
        ai.validate().unwrap();
        assert!(ai.is_active());
        assert_eq!(ai.providers.len(), 3);
        assert_eq!(ai.providers["openrouter"].api_key_env, "OPENROUTER_API_KEY");
        assert!(ai.providers["openrouter"].stream);
        assert_eq!(ai.providers["abliteration"].api_key.expose(), "ak_secret");
        assert_eq!(ai.agents[0].tools, AgentTools::Wave);
        assert_eq!(ai.agents[0].max_output_tokens, 2048);
        assert_eq!(ai.agents[1].tools, AgentTools::All);
        assert_eq!(ai.reply_debounce_ms, 10_000);
        assert_eq!(ai.max_reanswers, 5);
    }

    #[test]
    fn secrets_are_not_printed() {
        let ai: AiConfig = toml::from_str(EXAMPLE).unwrap();
        let printed = toml::to_string(&ai).unwrap();
        assert!(!printed.contains("ak_secret"), "{printed}");
        assert!(printed.contains("api_key = \"<redacted>\""), "{printed}");
        // Header values that look like credentials are hidden, others not.
        assert!(printed.contains("https://wave.example.com"), "{printed}");
        assert!(printed.contains("X-Api-Key = \"<redacted>\""), "{printed}");
        let debug = format!("{ai:?}");
        assert!(!debug.contains("ak_secret"), "{debug}");
    }

    #[test]
    fn nothing_is_active_without_agents() {
        assert!(!AiConfig::default().is_active());
        let mut ai: AiConfig = toml::from_str(EXAMPLE).unwrap();
        ai.enabled = false;
        assert!(!ai.is_active());
    }

    #[test]
    fn invalid_sections_are_reported() {
        let check = |text: &str| toml::from_str::<AiConfig>(text).unwrap().validate();
        let error = check("[[agents]]\nname = \"a\"\nprovider = \"p\"\nmodel = \"m\"").unwrap_err();
        assert!(error.contains("no provider 'p'"), "{error}");
        let provider = "[providers.p]\nbase_url = \"http://localhost:1/v1\"\n";
        for name in ["", "A", "a+b", "a#1", "a b", "-a", "a@b"] {
            let error = check(&format!(
                "{provider}[[agents]]\nname = \"{name}\"\nprovider = \"p\"\nmodel = \"m\""
            ))
            .unwrap_err();
            assert!(error.contains("not a valid agent name"), "{name}: {error}");
        }
        let error = check(&format!(
            "{provider}[[agents]]\nname = \"a\"\nprovider = \"p\"\nmodel = \"m\"\n\
             [[agents]]\nname = \"a\"\nprovider = \"p\"\nmodel = \"m\""
        ))
        .unwrap_err();
        assert!(error.contains("two agents"), "{error}");
        let error = check("[providers.p]\nbase_url = \"ftp://x\"").unwrap_err();
        assert!(error.contains("not an http(s) URL"), "{error}");
        let error = check(&format!(
            "{provider}[[agents]]\nname = \"a\"\nprovider = \"p\""
        ))
        .unwrap_err();
        assert!(error.contains("model must be set"), "{error}");
        assert!(toml::from_str::<AiConfig>("[providers.p]\nbase = 1").is_err());
        assert!(toml::from_str::<AiConfig>("[[agents]]\ntools = \"everything\"").is_err());
    }

    #[test]
    fn robot_ids() {
        for valid in ["assistant", "a", "my-bot", "bot_2", "a.b", "7bot"] {
            assert!(is_valid_robot_id(valid), "{valid}");
        }
        for invalid in ["", "Bot", "a+b", "a#b", "a b", ".a", "a@b", "é"] {
            assert!(!is_valid_robot_id(invalid), "{invalid}");
        }
    }
}
