//! Data messages through FCM's HTTP v1 API: a high-priority data-only
//! message (`{"resonator": "wake"}`), authorised by an OAuth access token
//! that the service account's key obtains (an RS256 JWT assertion).

use std::time::{Duration, Instant};

use ring::signature::RsaKeyPair;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::Outcome;
use crate::config::FcmConfig;
use crate::jwt::{KeyError, now_secs, rs256_key, sign_rs256};

const SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

/// The parts of a service account JSON key the relay uses.
#[derive(Deserialize)]
struct ServiceAccount {
    client_email: String,
    private_key: String,
    #[serde(default)]
    token_uri: String,
    #[serde(default)]
    project_id: String,
}

pub struct Fcm {
    key: RsaKeyPair,
    email: String,
    token_uri: String,
    project_id: String,
    endpoint: String,
    client: reqwest::Client,
    access: Mutex<Option<(String, Instant)>>,
}

impl std::fmt::Debug for Fcm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fcm")
            .field("project_id", &self.project_id)
            .finish_non_exhaustive()
    }
}

impl Fcm {
    /// Reads the service account named in `config`.
    pub fn new(config: &FcmConfig) -> Result<Self, KeyError> {
        let path = &config.service_account_path;
        let text = std::fs::read_to_string(path)
            .map_err(|error| KeyError(format!("cannot read {}: {error}", path.display())))?;
        let account: ServiceAccount = serde_json::from_str(&text)
            .map_err(|_| KeyError(format!("{} is not a service account key", path.display())))?;
        let key = rs256_key(&account.private_key)?;
        let project_id = if config.project_id.is_empty() {
            account.project_id
        } else {
            config.project_id.clone()
        };
        if project_id.is_empty() {
            return Err(KeyError("fcm.project_id is not set".to_owned()));
        }
        let token_uri = [config.token_uri.as_str(), &account.token_uri]
            .into_iter()
            .find(|uri| !uri.is_empty())
            .unwrap_or(DEFAULT_TOKEN_URI)
            .to_owned();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|error| KeyError(format!("HTTP client: {error}")))?;
        Ok(Fcm {
            key,
            email: account.client_email,
            token_uri,
            project_id,
            endpoint: config.endpoint.trim_end_matches('/').to_owned(),
            client,
            access: Mutex::new(None),
        })
    }

    async fn access_token(&self) -> Result<String, String> {
        let mut cached = self.access.lock().await;
        if let Some((token, until)) = cached.as_ref()
            && Instant::now() < *until
        {
            return Ok(token.clone());
        }
        let now = now_secs();
        let assertion = sign_rs256(
            &self.key,
            &json!({"alg": "RS256", "typ": "JWT"}),
            &json!({
                "iss": self.email,
                "scope": SCOPE,
                "aud": self.token_uri,
                "iat": now,
                "exp": now + 3600,
            }),
        )
        .map_err(|error| error.to_string())?;
        let response = self
            .client
            .post(&self.token_uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(format!(
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={assertion}"
            ))
            .send()
            .await
            .map_err(|error| format!("OAuth unreachable: {}", error.without_url()))?;
        let status = response.status();
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|_| format!("OAuth answered {status} without JSON"))?;
        let Some(token) = body["access_token"].as_str() else {
            return Err(format!("OAuth answered {status} without a token"));
        };
        let lifetime = body["expires_in"].as_u64().unwrap_or(3600);
        // Renewed five minutes early.
        let until = Instant::now() + Duration::from_secs(lifetime.saturating_sub(300).max(60));
        *cached = Some((token.to_owned(), until));
        Ok(token.to_owned())
    }

    /// Sends a high-priority data message to `token`.
    pub async fn send(&self, token: &str) -> Outcome {
        let message = json!({
            "message": {
                "token": token,
                "data": {"resonator": "wake"},
                "android": {"priority": "high", "ttl": "300s"},
            }
        });
        for attempt in 0..2 {
            let access = match self.access_token().await {
                Ok(access) => access,
                Err(error) => return Outcome::Failed(error),
            };
            let response = self
                .client
                .post(format!(
                    "{}/v1/projects/{}/messages:send",
                    self.endpoint, self.project_id
                ))
                .bearer_auth(access)
                .header("content-type", "application/json")
                .body(message.to_string())
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    return Outcome::Failed(format!("FCM unreachable: {}", error.without_url()));
                }
            };
            let status = response.status().as_u16();
            if status == 200 {
                return Outcome::Sent;
            }
            let body: serde_json::Value = response.json().await.unwrap_or_default();
            if is_gone(status, &body) {
                return Outcome::Gone;
            }
            if status == 401 && attempt == 0 {
                *self.access.lock().await = None;
                continue;
            }
            let reason = body["error"]["status"].as_str().unwrap_or_default();
            return Outcome::Failed(format!("FCM answered {status} {reason}"));
        }
        Outcome::Failed("FCM refused the access token".to_owned())
    }
}

/// Whether an FCM error says the token is no longer valid: 404
/// `UNREGISTERED`, or a 400 `INVALID_ARGUMENT` about the registration
/// token.
fn is_gone(status: u16, body: &serde_json::Value) -> bool {
    let error = &body["error"];
    let codes: Vec<&str> = error["details"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|detail| detail["errorCode"].as_str())
        .collect();
    if status == 404 || codes.contains(&"UNREGISTERED") {
        return true;
    }
    status == 400
        && error["message"]
            .as_str()
            .is_some_and(|message| message.contains("registration token"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gone_answers() {
        assert!(is_gone(404, &json!({})));
        assert!(is_gone(
            400,
            &json!({"error": {"details": [{"errorCode": "UNREGISTERED"}]}})
        ));
        assert!(is_gone(
            400,
            &json!({"error": {"status": "INVALID_ARGUMENT",
                "message": "The registration token is not a valid FCM registration token"}})
        ));
        assert!(!is_gone(400, &json!({"error": {"message": "bad payload"}})));
        assert!(!is_gone(500, &json!({})));
    }
}
