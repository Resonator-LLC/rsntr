//! Silent pushes through APNs: HTTP/2, token-based authentication (an
//! ES256 provider token from the `.p8` key, kept about 50 minutes),
//! `apns-push-type: background`, `apns-priority: 5`, the payload
//! `{"aps":{"content-available":1}}`.

use std::time::{Duration, Instant};

use ring::signature::EcdsaKeyPair;
use serde_json::json;
use tokio::sync::Mutex;

use crate::Outcome;
use crate::config::ApnsConfig;
use crate::jwt::{KeyError, es256_key, now_secs, sign_es256};

/// How long a provider token is used (Apple allows 20 to 60 minutes).
const TOKEN_LIFETIME: Duration = Duration::from_secs(50 * 60);

/// The payload of a background (silent) notification.
pub const PAYLOAD: &str = r#"{"aps":{"content-available":1}}"#;

pub struct Apns {
    key: EcdsaKeyPair,
    key_id: String,
    team_id: String,
    topic: String,
    production: String,
    sandbox: String,
    client: reqwest::Client,
    token: Mutex<Option<(String, Instant)>>,
}

impl std::fmt::Debug for Apns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Apns")
            .field("topic", &self.topic)
            .finish_non_exhaustive()
    }
}

impl Apns {
    /// Reads the `.p8` key named in `config`.
    pub fn new(config: &ApnsConfig) -> Result<Self, KeyError> {
        let pem = std::fs::read_to_string(&config.key_path).map_err(|error| {
            KeyError(format!(
                "cannot read {}: {error}",
                config.key_path.display()
            ))
        })?;
        let key = es256_key(&pem)?;
        // APNs speaks HTTP/2 only: over TLS (ALPN h2) or, for a local
        // test endpoint, cleartext with prior knowledge.
        let client = reqwest::Client::builder()
            .http2_prior_knowledge()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|error| KeyError(format!("HTTP client: {error}")))?;
        Ok(Apns {
            key,
            key_id: config.key_id.clone(),
            team_id: config.team_id.clone(),
            topic: config.topic.clone(),
            production: config.endpoint.trim_end_matches('/').to_owned(),
            sandbox: config.sandbox_endpoint.trim_end_matches('/').to_owned(),
            client,
            token: Mutex::new(None),
        })
    }

    async fn provider_token(&self) -> Result<String, KeyError> {
        let mut cached = self.token.lock().await;
        if let Some((token, made)) = cached.as_ref()
            && made.elapsed() < TOKEN_LIFETIME
        {
            return Ok(token.clone());
        }
        let token = sign_es256(
            &self.key,
            &json!({"alg": "ES256", "kid": self.key_id}),
            &json!({"iss": self.team_id, "iat": now_secs()}),
        )?;
        *cached = Some((token.clone(), Instant::now()));
        Ok(token)
    }

    /// Sends a silent push to `token`.
    pub async fn send(&self, token: &str, sandbox: bool) -> Outcome {
        for attempt in 0..2 {
            let provider = match self.provider_token().await {
                Ok(provider) => provider,
                Err(error) => return Outcome::Failed(error.to_string()),
            };
            let base = if sandbox {
                &self.sandbox
            } else {
                &self.production
            };
            let response = self
                .client
                .post(format!("{base}/3/device/{token}"))
                .header("authorization", format!("bearer {provider}"))
                .header("apns-push-type", "background")
                .header("apns-priority", "5")
                .header("apns-topic", &self.topic)
                .header("content-type", "application/json")
                .body(PAYLOAD)
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    // reqwest errors name the URL, which holds the token.
                    let error = error.without_url();
                    return Outcome::Failed(format!("APNs unreachable: {error}"));
                }
            };
            let status = response.status().as_u16();
            if status == 200 {
                return Outcome::Sent;
            }
            let reason = response
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|body| body["reason"].as_str().map(str::to_owned))
                .unwrap_or_default();
            match (status, reason.as_str()) {
                (410, _) | (400, "BadDeviceToken" | "DeviceTokenNotForTopic") => {
                    return Outcome::Gone;
                }
                (403, "ExpiredProviderToken" | "InvalidProviderToken") if attempt == 0 => {
                    *self.token.lock().await = None;
                }
                _ => return Outcome::Failed(format!("APNs answered {status} {reason}")),
            }
        }
        Outcome::Failed("APNs refused the provider token".to_owned())
    }
}
