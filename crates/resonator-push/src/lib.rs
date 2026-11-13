//! The resonator push relay: a tiny, stateless HTTP service that wakes
//! sleeping devices.
//!
//! An app in the background cannot be reached, so the nodes that have
//! something for it ask the relay to send it a silent push:
//!
//! - `POST /seal {platform, token}` → `{sealed}`: the device's node seals
//!   its push token under the relay's key and from then on hands out only
//!   the opaque blob; no other node ever sees the token.
//! - `POST /wake {sealed, sender, ts, sig}`: a node (federation key
//!   `sender`, Ed25519) asks for a wake. `sig` covers
//!   `"resonator/wake/1\0" ‖ sealed ‖ ts` (`ts` as 8 bytes, big-endian).
//!   Stale `ts` (more than 5 minutes off), bad signatures, replays and
//!   bad blobs are refused; wakes are rate-limited per client address,
//!   per sender and per device. The answer is `{"status":"sent"}`, or
//!   `{"status":"gone"}` (410) when the push service says the token is
//!   dead, so senders drop the blob.
//! - `GET /health`.
//!
//! Wakes carry no content: the relay learns only that some device should
//! wake. It keeps nothing but in-memory rate limits. See
//! `docs/push-relay.md`.

pub mod apns;
pub mod config;
pub mod fcm;
pub mod jwt;
pub mod limits;
pub mod seal;
pub mod testing;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::Deserialize;
use serde_json::{Value, json};

pub use config::Config;
use limits::{Limiter, Replays};
use seal::{Platform, Sealed, SealingKey};

/// What a wake signature is over: this, then the blob, then `ts`.
pub const WAKE_CONTEXT: &[u8] = b"resonator/wake/1\0";

/// The bytes a wake request's `sig` signs.
pub fn wake_message(sealed: &str, ts: i64) -> Vec<u8> {
    [WAKE_CONTEXT, sealed.as_bytes(), &ts.to_be_bytes()].concat()
}

/// What a push service made of a push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Sent,
    /// The token is no longer valid.
    Gone,
    /// Anything else (no secrets in the text).
    Failed(String),
}

/// Failure to set the relay up.
#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("sealing key: {0}")]
    SealingKey(std::io::Error),
    #[error("APNs: {0}")]
    Apns(jwt::KeyError),
    #[error("FCM: {0}")]
    Fcm(jwt::KeyError),
}

/// The relay: its key, the push services configured, the rate limits.
pub struct Relay {
    config: Config,
    key: SealingKey,
    apns: Option<apns::Apns>,
    fcm: Option<fcm::Fcm>,
    seal_per_ip: Limiter<IpAddr>,
    wake_per_ip: Limiter<IpAddr>,
    wake_per_sender: Limiter<[u8; 32]>,
    wake_per_device: Limiter<[u8; 32]>,
    replays: Replays,
}

impl std::fmt::Debug for Relay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relay")
            .field("listen", &self.config.listen)
            .field("apns", &self.apns.is_some())
            .field("fcm", &self.fcm.is_some())
            .finish_non_exhaustive()
    }
}

impl Relay {
    /// Loads (or makes) the sealing key and reads the push services'
    /// keys.
    pub fn new(config: Config) -> Result<Self, RelayError> {
        let key =
            SealingKey::load_or_create(&config.sealing_key).map_err(RelayError::SealingKey)?;
        let apns = config
            .apns
            .enabled()
            .then(|| apns::Apns::new(&config.apns))
            .transpose()
            .map_err(RelayError::Apns)?;
        let fcm = config
            .fcm
            .enabled()
            .then(|| fcm::Fcm::new(&config.fcm))
            .transpose()
            .map_err(RelayError::Fcm)?;
        let limits = &config.limits;
        let skew = Duration::from_secs(limits.max_skew_secs);
        Ok(Relay {
            seal_per_ip: Limiter::new(vec![limits.seal_per_ip], limits.max_keys),
            wake_per_ip: Limiter::new(vec![limits.wake_per_ip], limits.max_keys),
            wake_per_sender: Limiter::new(vec![limits.wake_per_sender], limits.max_keys),
            wake_per_device: Limiter::new(
                vec![limits.wake_per_device, limits.wake_per_device_long],
                limits.max_keys,
            ),
            replays: Replays::new(2 * skew + Duration::from_secs(1)),
            key,
            apns,
            fcm,
            config,
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The HTTP routes (serve with `into_make_service_with_connect_info`).
    pub fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/health", get(health))
            .route("/seal", post(seal_token))
            .route("/wake", post(wake))
            .layer(DefaultBodyLimit::max(16 * 1024))
            .with_state(self)
    }

    /// Serves on `listener` until `shutdown` resolves.
    pub async fn serve(
        self: Arc<Self>,
        listener: tokio::net::TcpListener,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        axum::serve(
            listener,
            self.router()
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown)
        .await
    }

    fn client_ip(&self, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
        if self.config.trust_forwarded_for
            && let Some(ip) = headers
                .get("x-forwarded-for")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.rsplit(',').next())
                .and_then(|last| last.trim().parse().ok())
        {
            return ip;
        }
        peer.ip()
    }

    async fn push(&self, sealed: &Sealed) -> Option<Outcome> {
        match sealed.platform {
            Platform::Apns | Platform::ApnsSandbox => {
                let apns = self.apns.as_ref()?;
                Some(
                    apns.send(&sealed.token, sealed.platform == Platform::ApnsSandbox)
                        .await,
                )
            }
            Platform::Fcm => Some(self.fcm.as_ref()?.send(&sealed.token).await),
        }
    }
}

fn answer(status: StatusCode, word: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"status": word})))
}

async fn health(State(relay): State<Arc<Relay>>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "apns": relay.apns.is_some(),
        "fcm": relay.fcm.is_some(),
    }))
}

#[derive(Deserialize)]
struct SealRequest {
    platform: String,
    token: String,
}

async fn seal_token(
    State(relay): State<Arc<Relay>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request: Result<Json<SealRequest>, axum::extract::rejection::JsonRejection>,
) -> (StatusCode, Json<Value>) {
    if !relay.seal_per_ip.check(&relay.client_ip(peer, &headers)) {
        return answer(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    let Ok(Json(request)) = request else {
        return answer(StatusCode::BAD_REQUEST, "bad_request");
    };
    let Some(platform) = Platform::parse(&request.platform) else {
        return answer(StatusCode::BAD_REQUEST, "bad_platform");
    };
    let Some(token) = seal::check_token(platform, &request.token) else {
        return answer(StatusCode::BAD_REQUEST, "bad_token");
    };
    let sealed = relay.key.seal(&Sealed {
        platform,
        token,
        issued_at: jwt::now_secs(),
    });
    tracing::info!(platform = platform.as_str(), "sealed a push token");
    (StatusCode::OK, Json(json!({"sealed": sealed})))
}

#[derive(Deserialize)]
struct WakeRequest {
    sealed: String,
    sender: String,
    ts: i64,
    sig: String,
}

/// A sender key: 64 hex characters, or the 52-character z-base-32 form
/// (a resonator key domain).
pub fn parse_key(text: &str) -> Option<[u8; 32]> {
    if text.len() == 64 {
        return seal::decode_hex(text);
    }
    if text.len() != 52 {
        return None;
    }
    const ALPHABET: &[u8; 32] = b"ybndrfg8ejkmcpqxot1uwisza345h769";
    let mut out = Vec::with_capacity(33);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in text.bytes() {
        let value = ALPHABET.iter().position(|a| *a == c)? as u32;
        acc = (acc << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // 260 bits: the 4 left over must be zero.
    if acc != 0 {
        return None;
    }
    out.try_into().ok()
}

async fn wake(
    State(relay): State<Arc<Relay>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request: Result<Json<WakeRequest>, axum::extract::rejection::JsonRejection>,
) -> (StatusCode, Json<Value>) {
    if !relay.wake_per_ip.check(&relay.client_ip(peer, &headers)) {
        return answer(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    let Ok(Json(request)) = request else {
        return answer(StatusCode::BAD_REQUEST, "bad_request");
    };
    let skew = i64::try_from(relay.config.limits.max_skew_secs).unwrap_or(i64::MAX);
    if jwt::now_secs().abs_diff(request.ts) > skew.unsigned_abs() {
        return answer(StatusCode::UNAUTHORIZED, "stale");
    }
    let Some(sender) = parse_key(&request.sender) else {
        return answer(StatusCode::BAD_REQUEST, "bad_sender");
    };
    let signature = URL_SAFE_NO_PAD
        .decode(request.sig.trim_end_matches('='))
        .unwrap_or_default();
    if UnparsedPublicKey::new(&ED25519, sender)
        .verify(&wake_message(&request.sealed, request.ts), &signature)
        .is_err()
    {
        return answer(StatusCode::UNAUTHORIZED, "bad_signature");
    }
    let id: [u8; 32] = ring::digest::digest(&ring::digest::SHA256, &signature)
        .as_ref()
        .try_into()
        .unwrap_or_default();
    if !relay.replays.first_time(id, Instant::now()) {
        return answer(StatusCode::CONFLICT, "replay");
    }
    if !relay.wake_per_sender.check(&sender) {
        return answer(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    let Ok(sealed) = relay.key.unseal(&request.sealed) else {
        return answer(StatusCode::BAD_REQUEST, "bad_blob");
    };
    // Per device, whatever blob of its token is used.
    let device: [u8; 32] = ring::digest::digest(
        &ring::digest::SHA256,
        format!("{}\0{}", sealed.platform.as_str(), sealed.token).as_bytes(),
    )
    .as_ref()
    .try_into()
    .unwrap_or_default();
    if !relay.wake_per_device.check(&device) {
        return answer(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    let platform = sealed.platform.as_str();
    match relay.push(&sealed).await {
        None => answer(StatusCode::SERVICE_UNAVAILABLE, "unsupported"),
        Some(Outcome::Sent) => {
            tracing::info!(platform, "sent a wake");
            answer(StatusCode::OK, "sent")
        }
        Some(Outcome::Gone) => {
            tracing::info!(platform, "a push token is gone");
            answer(StatusCode::GONE, "gone")
        }
        Some(Outcome::Failed(reason)) => {
            tracing::warn!(platform, %reason, "a wake was not sent");
            answer(StatusCode::BAD_GATEWAY, "failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_parse_from_hex_and_z_base_32() {
        assert_eq!(parse_key(&"00".repeat(32)), Some([0; 32]));
        assert_eq!(parse_key(&"y".repeat(52)), Some([0; 32]));
        // 0xff… is "9" × 51, then the last bit and four zero bits
        // (0b10000, "o").
        let mut ones = "9".repeat(51);
        ones.push('o');
        assert_eq!(parse_key(&ones), Some([0xff; 32]));
        assert_eq!(parse_key(&"9".repeat(52)), None);
        assert_eq!(parse_key("short"), None);
        assert_eq!(parse_key(&"l".repeat(52)), None);
    }
}
