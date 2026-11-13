//! Device mode (`mode = "device"`, see [`crate::config::DeviceConfig`]):
//! the loopback token, the gate that keeps other machines out, and the
//! contacts that replace the directory.
//!
//! - `GET /?token=<token>` from a loopback peer signs the device's one
//!   account in: it starts an ordinary session (cookie, and with it the
//!   WebSocket's socket token) and redirects to `/`, which drops the token
//!   from the address bar and the history.
//! - Requests whose client is not loopback get 403, unless
//!   `device.allow_remote` is set.
//! - `GET /contacts?query=&limit=` answers like `/directory`; `POST
//!   /contacts` with `{"address"}` or `{"link": "resonator:contact/<address>"}`
//!   adds one (probing an address of another domain through federation
//!   when it can), and `DELETE /contacts/<address>` removes one added by
//!   hand.

use std::net::IpAddr;
use std::sync::OnceLock;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::header::{self, HeaderValue};
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use resonator_model::id::ParticipantId;
use serde::Deserialize;
use serde_json::json;
use subtle::ConstantTimeEq;

use super::{AppState, Context, RequestContext, failure, internal_error, json, session_cookie};

/// The prefix of contact links (`resonator:contact/<address>`); a QR code
/// carries the same text.
pub const CONTACT_LINK_PREFIX: &str = "resonator:contact/";

/// Most contacts one `/contacts` request returns.
const MAX_CONTACT_RESULTS: usize = 50;

/// What device mode adds to the application state.
pub struct DeviceAccess {
    /// The one account.
    pub participant: ParticipantId,
    /// The token of `GET /?token=`; random at every start.
    token: String,
    /// This device's key (`<z32>`), set by federation in person mode.
    device_key: OnceLock<String>,
}

impl std::fmt::Debug for DeviceAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceAccess")
            .field("participant", &self.participant)
            .finish_non_exhaustive()
    }
}

impl DeviceAccess {
    /// Device mode's state for `participant`, with a fresh random token.
    pub(super) fn new(participant: ParticipantId, kept: Option<&str>) -> Self {
        let mut bytes = [0u8; 32];
        // The operating system's random source; there is no going on
        // without one.
        getrandom::fill(&mut bytes).expect("the system's random number generator");
        DeviceAccess {
            participant,
            token: kept
                .filter(|token| token.len() >= 32)
                .map_or_else(|| URL_SAFE_NO_PAD.encode(bytes), str::to_owned),
            device_key: OnceLock::new(),
        }
    }

    /// Whether `presented` is the token (in constant time).
    fn accepts(&self, presented: &str) -> bool {
        bool::from(presented.as_bytes().ct_eq(self.token.as_bytes()))
    }
}

impl AppState {
    /// Device mode: the token that signs the account in from loopback
    /// (`GET /?token=`). `None` in server mode. Never log it.
    pub fn device_token(&self) -> Option<&str> {
        self.device.as_ref().map(|device| device.token.as_str())
    }

    /// Device mode: the one account.
    pub fn device_account(&self) -> Option<&ParticipantId> {
        self.device.as_ref().map(|device| &device.participant)
    }

    /// Device mode: records this device's key, which `/session` reports.
    /// Returns false outside device mode or if one was set already.
    pub fn set_device_key(&self, key: String) -> bool {
        self.device
            .as_ref()
            .is_some_and(|device| device.device_key.set(key).is_ok())
    }
}

/// Whether `address` is this machine: loopback, IPv4-mapped included.
pub(super) fn is_loopback(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// The value of the `token` query parameter, if any.
fn token_param(uri: &Uri) -> Option<&str> {
    uri.query()?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == "token")
        .map(|(_, value)| value)
}

/// Device mode's part of [`super::context_layer`], before anything else:
/// refuses clients that are not loopback (unless `allow_remote`), and
/// answers `GET /?token=`. `None` lets the request through.
pub(super) async fn gate(
    state: &AppState,
    context: &RequestContext,
    method: &Method,
    uri: &Uri,
) -> Option<Response> {
    let device = state.device.as_ref()?;
    let local = is_loopback(context.client);
    if !local && !state.config.device.allow_remote {
        return Some(failure(
            StatusCode::FORBIDDEN,
            "This resonator serves its own device only.",
        ));
    }
    if uri.path() != "/" || !matches!(*method, Method::GET | Method::HEAD) {
        return None;
    }
    let presented = token_param(uri)?;
    // The token never works from another machine, nor through a proxy.
    if !local || context.trusted_peer || !device.accepts(presented) {
        tracing::info!(client = %context.client, "refused a device sign-in token");
        return Some(failure(StatusCode::FORBIDDEN, "Invalid token."));
    }
    let grant = match state.auth.create_session(&device.participant).await {
        Ok(grant) => grant,
        Err(error) => return Some(internal_error(error)),
    };
    tracing::info!(participant = %device.participant, "signed in by the device token");
    let mut response = (
        StatusCode::SEE_OTHER,
        [(header::LOCATION, HeaderValue::from_static("/"))],
    )
        .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie(
            &grant.session_id,
            grant.max_age_secs,
            state.cookie_secure(context),
        ),
    );
    Some(response)
}

/// What `/session` adds in device mode.
pub(super) fn session_info(state: &AppState) -> Option<serde_json::Value> {
    let device = state.device.as_ref()?;
    let mut info = json!({
        "account": device.participant.address(),
        "name": state.config.person.device_name,
        "allow_remote": state.config.device.allow_remote,
    });
    if let Some(key) = device.device_key.get() {
        info["key"] = json!(key);
    }
    Some(info)
}

/// The signed-in caller of a contacts request, or the refusal.
fn contacts_caller(
    state: &AppState,
    context: &RequestContext,
) -> Result<ParticipantId, StatusCode> {
    if state.device.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    match &context.identity {
        Some(identity) if identity.can_read() => Ok(identity.participant.clone()),
        _ => Err(StatusCode::FORBIDDEN),
    }
}

/// `GET /contacts?query=&limit=`: the caller's contacts, in the shape of
/// `/directory` (`{"accounts": [{address, name, image_url, kind}]}`) plus
/// `added` (added by hand rather than met on a wave). 404 outside device
/// mode.
pub(super) async fn list(
    State(state): State<AppState>,
    context: Context,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let owner = match contacts_caller(&state, &context) {
        Ok(owner) => owner,
        Err(status) => return status.into_response(),
    };
    let query = params.get("query").map_or("", |q| q.trim());
    if query.chars().count() > 256 {
        return failure(StatusCode::BAD_REQUEST, "Query too long");
    }
    let limit = params
        .get("limit")
        .and_then(|limit| limit.parse::<usize>().ok())
        .unwrap_or(20)
        .clamp(1, MAX_CONTACT_RESULTS);
    let found = match state
        .server
        .database()
        .contact_store()
        .search(owner.address(), query, limit + 1)
        .await
    {
        Ok(found) => found,
        Err(error) => return internal_error(error),
    };
    let accounts = found
        .into_iter()
        .filter(|contact| !contact.address.starts_with("welcome-bot@"))
        .take(limit)
        .map(|contact| {
            json!({
                "address": contact.address,
                "name": super::wave_handlers::profile_name(&contact.address),
                "image_url": format!("/iniavatars/100x100/{}", contact.address),
                "kind": contact.kind,
                "added": contact.added,
            })
        })
        .collect::<Vec<_>>();
    json(StatusCode::OK, json!({ "accounts": accounts }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AddContact {
    address: Option<String>,
    /// `resonator:contact/<address>`.
    link: Option<String>,
}

/// The address a contact request names: `address`, or the address of a
/// contact `link`.
fn requested_address(request: &AddContact) -> Option<ParticipantId> {
    let text = match (&request.address, &request.link) {
        (Some(address), None) => address.trim(),
        (None, Some(link)) => link.trim().strip_prefix(CONTACT_LINK_PREFIX)?,
        _ => return None,
    };
    ParticipantId::of(&text.to_lowercase())
        .ok()
        .filter(|id| !id.name().is_empty())
}

/// `POST /contacts` with `{"address"}` or `{"link"}`: adds a contact.
/// An address of another domain is probed through federation when it is
/// on (`probe` in the answer: the verdict of `/federation/probe`); the
/// contact is kept whatever the verdict, as the other device may only be
/// offline. 400 for anything but one valid address, 404 outside device
/// mode.
pub(super) async fn add(State(state): State<AppState>, context: Context, body: Bytes) -> Response {
    let owner = match contacts_caller(&state, &context) {
        Ok(owner) => owner,
        Err(status) => return status.into_response(),
    };
    if !context.identity.as_ref().is_some_and(|i| i.can_write()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let request: AddContact = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            return failure(StatusCode::BAD_REQUEST, format!("Invalid contact: {error}"));
        }
    };
    let Some(address) = requested_address(&request) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "Give one valid address, or a contact link (resonator:contact/<address>).",
        );
    };
    if address == owner {
        return failure(StatusCode::BAD_REQUEST, "That is this account.");
    }
    let probe = match state.remote_probe.get() {
        Some(remote) if address.domain() != state.config.domain => {
            Some(remote.probe(&address).await)
        }
        _ => None,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        });
    let added = match state
        .server
        .database()
        .contact_store()
        .add(owner.address(), address.address(), now)
        .await
    {
        Ok(added) => added,
        Err(error) => return internal_error(error),
    };
    let mut body = json!({
        "address": address.address(),
        "name": super::wave_handlers::profile_name(address.address()),
        "added": added,
    });
    if let Some(probe) = probe {
        body["probe"] = json!(probe);
    }
    json(
        if added {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        body,
    )
}

/// `DELETE /contacts/<address>`: removes a contact added by hand. 204, or
/// 404 if it was not one (or outside device mode).
pub(super) async fn remove(
    State(state): State<AppState>,
    context: Context,
    Path(address): Path<String>,
) -> Response {
    let owner = match contacts_caller(&state, &context) {
        Ok(owner) => owner,
        Err(status) => return status.into_response(),
    };
    if !context.identity.as_ref().is_some_and(|i| i.can_write()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state
        .server
        .database()
        .contact_store()
        .remove(owner.address(), &address.to_lowercase())
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => failure(StatusCode::NOT_FOUND, "No such contact"),
        Err(error) => internal_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_addresses() {
        for local in ["127.0.0.1", "127.8.9.1", "::1", "::ffff:127.0.0.1"] {
            assert!(is_loopback(local.parse().unwrap()), "{local}");
        }
        for remote in ["0.0.0.0", "10.0.0.1", "100.64.0.1", "::", "::ffff:10.0.0.1"] {
            assert!(!is_loopback(remote.parse().unwrap()), "{remote}");
        }
    }

    #[test]
    fn token_parameter() {
        let uri: Uri = "/?a=1&token=abc&b=2".parse().unwrap();
        assert_eq!(token_param(&uri), Some("abc"));
        assert_eq!(token_param(&"/".parse().unwrap()), None);
        assert_eq!(token_param(&"/?tokens=x".parse().unwrap()), None);
    }

    #[test]
    fn contact_requests() {
        let request = |address: Option<&str>, link: Option<&str>| AddContact {
            address: address.map(str::to_owned),
            link: link.map(str::to_owned),
        };
        assert_eq!(
            requested_address(&request(Some(" Bob@Example.org "), None))
                .unwrap()
                .address(),
            "bob@example.org"
        );
        assert_eq!(
            requested_address(&request(None, Some("resonator:contact/bob@example.org")))
                .unwrap()
                .address(),
            "bob@example.org"
        );
        assert!(requested_address(&request(None, Some("https://x/bob@example.org"))).is_none());
        assert!(requested_address(&request(Some("a@x.org"), Some("b@x.org"))).is_none());
        assert!(requested_address(&request(Some("@example.org"), None)).is_none());
        assert!(requested_address(&request(None, None)).is_none());
    }
}
