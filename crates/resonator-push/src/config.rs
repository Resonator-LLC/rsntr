//! The relay's settings: built-in defaults, overridden by one TOML file,
//! overridden by `RESONATOR_PUSH_*` environment variables.
//!
//! Secrets are never settings: the APNs `.p8` key, the FCM service
//! account and the sealing key are named by path, and read from there.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A configuration that cannot be loaded or is invalid.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read config file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot parse config file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

/// The relay's settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// The HTTP listen address (`host:port`).
    pub listen: String,
    /// The file holding the relay's sealing key (made, mode 600, if
    /// missing). Losing it voids every sealed token handed out.
    pub sealing_key: PathBuf,
    /// Take the client address of rate limits from the last
    /// `X-Forwarded-For` entry (behind a reverse proxy only).
    pub trust_forwarded_for: bool,
    pub apns: ApnsConfig,
    pub fcm: FcmConfig,
    pub limits: Limits,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: "0.0.0.0:9900".to_owned(),
            sealing_key: PathBuf::from("push-sealing.key"),
            trust_forwarded_for: false,
            apns: ApnsConfig::default(),
            fcm: FcmConfig::default(),
            limits: Limits::default(),
        }
    }
}

/// Apple Push Notification service, token-based authentication. Off
/// while `key_path` is empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApnsConfig {
    /// The `.p8` signing key from the Apple developer account.
    pub key_path: PathBuf,
    /// Its key ID (10 characters).
    pub key_id: String,
    /// The team ID (10 characters).
    pub team_id: String,
    /// The app's bundle ID (`apns-topic`).
    pub topic: String,
    /// Production APNs.
    pub endpoint: String,
    /// The APNs sandbox (development builds).
    pub sandbox_endpoint: String,
}

impl Default for ApnsConfig {
    fn default() -> Self {
        ApnsConfig {
            key_path: PathBuf::new(),
            key_id: String::new(),
            team_id: String::new(),
            topic: "am.resonator.client.ios".to_owned(),
            endpoint: "https://api.push.apple.com".to_owned(),
            sandbox_endpoint: "https://api.sandbox.push.apple.com".to_owned(),
        }
    }
}

impl ApnsConfig {
    pub fn enabled(&self) -> bool {
        !self.key_path.as_os_str().is_empty()
    }
}

/// Firebase Cloud Messaging, HTTP v1 API. Off while
/// `service_account_path` is empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FcmConfig {
    /// The service account JSON key file (Firebase console, project
    /// settings, service accounts).
    pub service_account_path: PathBuf,
    /// The Firebase project ID; empty: the service account's.
    pub project_id: String,
    /// The FCM API.
    pub endpoint: String,
    /// The OAuth token endpoint; empty: the service account's
    /// `token_uri`.
    pub token_uri: String,
}

impl Default for FcmConfig {
    fn default() -> Self {
        FcmConfig {
            service_account_path: PathBuf::new(),
            project_id: String::new(),
            endpoint: "https://fcm.googleapis.com".to_owned(),
            token_uri: String::new(),
        }
    }
}

impl FcmConfig {
    pub fn enabled(&self) -> bool {
        !self.service_account_path.as_os_str().is_empty()
    }
}

/// A token bucket: up to `burst` at once, one more every `every_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rate {
    pub burst: u32,
    pub every_ms: u64,
}

impl Rate {
    pub const fn new(burst: u32, every_ms: u64) -> Self {
        Rate { burst, every_ms }
    }
}

/// The in-memory rate limits (lost at restart, by design).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    /// `/seal` per client address.
    pub seal_per_ip: Rate,
    /// `/wake` per client address.
    pub wake_per_ip: Rate,
    /// `/wake` per sender key.
    pub wake_per_sender: Rate,
    /// `/wake` per device (push token): the short-term bucket...
    pub wake_per_device: Rate,
    /// ...and the long-term one; both must allow a wake.
    pub wake_per_device_long: Rate,
    /// How far a wake's `ts` may be from the relay's clock.
    pub max_skew_secs: u64,
    /// Keys each limiter remembers at most.
    pub max_keys: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            seal_per_ip: Rate::new(20, 10_000),
            wake_per_ip: Rate::new(120, 500),
            wake_per_sender: Rate::new(20, 10_000),
            // One per 20 s after a burst of three, and about 30 an hour.
            wake_per_device: Rate::new(3, 20_000),
            wake_per_device_long: Rate::new(30, 120_000),
            max_skew_secs: 300,
            max_keys: 100_000,
        }
    }
}

impl Config {
    /// Reads `file` if given, applies the environment, and validates.
    pub fn load(file: Option<&Path>) -> Result<Self, ConfigError> {
        let mut config = match file {
            Some(path) => {
                let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
                    path: path.to_path_buf(),
                    source,
                })?;
                toml::from_str(&text).map_err(|source| ConfigError::Parse {
                    path: path.to_path_buf(),
                    source: Box::new(source),
                })?
            }
            None => Config::default(),
        };
        config.apply_env(|name| std::env::var(name).ok());
        config.validate()?;
        Ok(config)
    }

    /// Replaces the settings that `RESONATOR_PUSH_*` variables (read
    /// through `var`) set.
    pub fn apply_env(&mut self, var: impl Fn(&str) -> Option<String>) {
        let string = |name: &str, field: &mut String| {
            if let Some(value) = var(name) {
                *field = value;
            }
        };
        string("RESONATOR_PUSH_LISTEN", &mut self.listen);
        string("RESONATOR_PUSH_APNS_KEY_ID", &mut self.apns.key_id);
        string("RESONATOR_PUSH_APNS_TEAM_ID", &mut self.apns.team_id);
        string("RESONATOR_PUSH_APNS_TOPIC", &mut self.apns.topic);
        string("RESONATOR_PUSH_APNS_ENDPOINT", &mut self.apns.endpoint);
        string(
            "RESONATOR_PUSH_APNS_SANDBOX_ENDPOINT",
            &mut self.apns.sandbox_endpoint,
        );
        string("RESONATOR_PUSH_FCM_PROJECT_ID", &mut self.fcm.project_id);
        string("RESONATOR_PUSH_FCM_ENDPOINT", &mut self.fcm.endpoint);
        string("RESONATOR_PUSH_FCM_TOKEN_URI", &mut self.fcm.token_uri);
        let path = |name: &str, field: &mut PathBuf| {
            if let Some(value) = var(name) {
                *field = PathBuf::from(value);
            }
        };
        path("RESONATOR_PUSH_SEALING_KEY", &mut self.sealing_key);
        path("RESONATOR_PUSH_APNS_KEY_PATH", &mut self.apns.key_path);
        path(
            "RESONATOR_PUSH_FCM_SERVICE_ACCOUNT",
            &mut self.fcm.service_account_path,
        );
        if let Some(value) = var("RESONATOR_PUSH_TRUST_FORWARDED_FOR") {
            self.trust_forwarded_for = matches!(value.as_str(), "1" | "true" | "yes");
        }
    }

    /// Checks the settings that have constraints.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |message: String| Err(ConfigError::Invalid(message));
        if self.listen.parse::<std::net::SocketAddr>().is_err() {
            return invalid(format!("listen '{}' is not an ip:port", self.listen));
        }
        if self.sealing_key.as_os_str().is_empty() {
            return invalid("sealing_key must name a file".to_owned());
        }
        let url = |name: &str, value: &str| {
            if value.starts_with("https://") || value.starts_with("http://") {
                Ok(())
            } else {
                Err(ConfigError::Invalid(format!(
                    "{name} '{value}' is not a URL"
                )))
            }
        };
        if self.apns.enabled() {
            if self.apns.key_id.is_empty() || self.apns.team_id.is_empty() {
                return invalid("apns needs key_id and team_id with key_path".to_owned());
            }
            if self.apns.topic.is_empty() {
                return invalid("apns.topic must not be empty".to_owned());
            }
            url("apns.endpoint", &self.apns.endpoint)?;
            url("apns.sandbox_endpoint", &self.apns.sandbox_endpoint)?;
        }
        if self.fcm.enabled() {
            url("fcm.endpoint", &self.fcm.endpoint)?;
            if !self.fcm.token_uri.is_empty() {
                url("fcm.token_uri", &self.fcm.token_uri)?;
            }
        }
        for (name, rate) in [
            ("seal_per_ip", self.limits.seal_per_ip),
            ("wake_per_ip", self.limits.wake_per_ip),
            ("wake_per_sender", self.limits.wake_per_sender),
            ("wake_per_device", self.limits.wake_per_device),
            ("wake_per_device_long", self.limits.wake_per_device_long),
        ] {
            if rate.burst == 0 || rate.every_ms == 0 {
                return invalid(format!("limits.{name} needs burst and every_ms above 0"));
            }
        }
        if self.limits.max_keys == 0 {
            return invalid("limits.max_keys must be at least 1".to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_file_and_environment() {
        let config = Config::default();
        assert_eq!(config.listen, "0.0.0.0:9900");
        assert_eq!(config.apns.topic, "am.resonator.client.ios");
        assert!(!config.apns.enabled() && !config.fcm.enabled());
        config.validate().unwrap();

        let mut config: Config = toml::from_str(
            r#"
            listen = "127.0.0.1:1"
            sealing_key = "/k"
            [apns]
            key_path = "/a.p8"
            key_id = "KEY"
            team_id = "TEAM"
            [limits]
            wake_per_device = { burst = 1, every_ms = 5 }
            "#,
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.limits.wake_per_device, Rate::new(1, 5));
        assert_eq!(
            config.limits.wake_per_sender,
            Limits::default().wake_per_sender
        );
        config.apply_env(|name| match name {
            "RESONATOR_PUSH_APNS_TOPIC" => Some("app.x".to_owned()),
            "RESONATOR_PUSH_FCM_SERVICE_ACCOUNT" => Some("/sa.json".to_owned()),
            "RESONATOR_PUSH_TRUST_FORWARDED_FOR" => Some("true".to_owned()),
            _ => None,
        });
        assert_eq!(config.apns.topic, "app.x");
        assert!(config.fcm.enabled() && config.trust_forwarded_for);
        config.validate().unwrap();

        config.apns.key_id.clear();
        assert!(config.validate().is_err());
        assert!(toml::from_str::<Config>("nope = 1").is_err());
        let config = Config {
            listen: "mothership".to_owned(),
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }
}
