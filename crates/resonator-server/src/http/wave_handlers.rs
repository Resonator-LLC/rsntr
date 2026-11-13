//! The WebSocket upgrade, the read-only wave endpoints and the
//! operational endpoints.
//!
//! `fetch`, `waveref`, `profiles`, `set_locale` and `session` follow
//! `FetchServlet`, `WaveRefServlet`, `FetchProfilesServlet` (with
//! `InitialsProfilesFetcher`), `LocaleServlet` and
//! `WaveClientServlet.getSessionJson` of Apache Wave (Apache License 2.0).

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, Query, State};
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use resonator_model::id::constants::CONVERSATION_ROOT_WAVELET;
use resonator_model::id::{IdSerialiser, ModernIdSerialiser, WaveRef, WaveletId, WaveletName};
use resonator_model::proto::json::{JsonMessage, to_json};
use resonator_model::proto::profile::ProfileResponse;
use resonator_model::proto::profile::profile_response::FetchedProfile;
use resonator_model::proto::waveclient_rpc::WaveViewSnapshot;
use resonator_model::proto::{ProtoError, serialize_document, serialize_wavelet};
use serde_json::json;
use subtle::ConstantTimeEq;

use super::{AppState, Context, failure, internal_error, json};
use crate::account::AccountData;
use crate::rpc::serve_socket;
use crate::waveserver::WaveletProvider;

type Params = Query<HashMap<String, String>>;

/// `GET /`: a placeholder until the web client is embedded.
pub(super) async fn index(State(state): State<AppState>, context: Context) -> Response {
    let who = match context.participant() {
        Some(participant) => format!(
            "<p>Signed in as {}.</p>\n<form method=\"post\" action=\"/auth/signout\">\
             <p><button type=\"submit\">Sign out</button></p></form>",
            participant
                .address()
                .replace('&', "&amp;")
                .replace('<', "&lt;")
        ),
        None => "<p><a href=\"/auth/signin\">Sign in</a> or \
                 <a href=\"/auth/register\">register</a>.</p>"
            .to_owned(),
    };
    (
        [(header::CONTENT_TYPE, "text/html;charset=utf-8")],
        format!(
            "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>resonator</title></head>\
             <body>\n<h1>resonator</h1>\n<p>The wave server for {} is running.</p>\n{who}\n\
             </body></html>\n",
            state.config.domain
        ),
    )
        .into_response()
}

/// `GET /healthz`: 200 if the database answers.
pub(super) async fn healthz(State(state): State<AppState>) -> Response {
    match state.server.database().schema_version().await {
        Ok(schema_version) => json(
            StatusCode::OK,
            json!({"status": "ok", "schema_version": schema_version}),
        ),
        Err(error) => {
            tracing::error!(%error, "health check failed");
            json(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"status": "unavailable"}),
            )
        }
    }
}

/// `GET /metrics`: Prometheus text, if enabled; with a configured token it
/// requires `Authorization: Bearer <token>`.
pub(super) async fn metrics(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !state.config.metrics_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    let expected = state.config.metrics_token.as_bytes();
    if !expected.is_empty() {
        let presented = super::bearer_token(&headers).unwrap_or("").as_bytes();
        if !bool::from(presented.ct_eq(expected)) {
            return (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer")],
            )
                .into_response();
        }
    }
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        state.metrics_snapshot().render() + &state.metrics.render_extensions(),
    )
        .into_response()
}

/// `GET /session`: what the original's host page hands the client
/// (`domain`, `address` if signed in, `id`: a seed for the client's id
/// generator), plus where and how to connect the WebSocket.
pub(super) async fn session(State(state): State<AppState>, context: Context) -> Response {
    let mut seed = [0u8; 8];
    if let Err(error) = getrandom::fill(&mut seed) {
        return internal_error(error);
    }
    // Ten web-safe base64 characters, as `RandomBase64Generator.next(10)`.
    let mut id_seed = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(seed);
    id_seed.truncate(10);
    let mut body = json!({
        "domain": state.config.domain,
        "id": id_seed,
        "websocket_address": state.config.websocket_address(),
        "registration_enabled": state.config.registration_open(),
    });
    // Only when on, so that nothing changes for servers without it.
    if state.config.federation.enabled {
        body["federation"] = json!(true);
    }
    // Device mode only: one person's device, signed in by its app.
    if let Some(device) = super::device_handlers::session_info(&state) {
        body["mode"] = json!("device");
        body["device"] = device;
    }
    if let Some(participant) = context.participant() {
        body["address"] = json!(participant.address());
    }
    // Only a cookie session gets a socket token; a bearer client presents
    // its bearer token instead.
    if let Some(session_id) = &context.session_id {
        body["socket_token"] = json!(state.auth.socket_token(session_id));
    }
    json(StatusCode::OK, body)
}

/// `GET /socket`: upgrades to the wave protocol WebSocket. The connection
/// starts as whoever the request is authenticated as.
pub(super) async fn socket(
    State(state): State<AppState>,
    context: Context,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    // Browsers send cookies with cross-site WebSocket handshakes, so the
    // origin is checked here as for state-changing requests.
    if headers.contains_key(header::ORIGIN) && !state.origin_allowed(&headers, context.trusted_peer)
    {
        crate::metrics::Metrics::inc(&state.metrics.csrf_rejections_total);
        return failure(StatusCode::FORBIDDEN, "Cross-site request rejected");
    }
    let limit = state.config.websocket_max_message_size;
    let socket_context = Arc::clone(&state.socket);
    let identity = context.identity.clone();
    upgrade
        .max_message_size(limit)
        .max_frame_size(limit)
        .on_upgrade(move |socket| serve_socket(socket, socket_context, identity))
}

fn proto_json<M: JsonMessage>(message: Result<M, ProtoError>) -> Response {
    match message.and_then(|message| to_json(&message)) {
        Ok(text) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            text,
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

/// `GET /fetch/<waveref>`: a snapshot of a wave's root conversation
/// wavelet, of a wavelet or of a document, in the protocol's JSON form.
/// 404 for a malformed reference; 403 for anything the caller may not
/// see, including what does not exist.
pub(super) async fn fetch(
    State(state): State<AppState>,
    context: Context,
    Path(path): Path<String>,
) -> Response {
    let Ok(waveref) = WaveRef::decode_from_path(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let forbidden = || StatusCode::FORBIDDEN.into_response();
    let Some(identity) = context.identity.as_ref().filter(|i| i.can_read()) else {
        return forbidden();
    };
    let wavelet_id = match waveref.wavelet_id() {
        Some(wavelet_id) => wavelet_id.clone(),
        None => match WaveletId::of(waveref.wave_id().domain(), CONVERSATION_ROOT_WAVELET) {
            Ok(wavelet_id) => wavelet_id,
            Err(_) => return StatusCode::NOT_FOUND.into_response(),
        },
    };
    let wavelet_name = WaveletName::of(waveref.wave_id().clone(), wavelet_id);
    match state
        .frontend
        .check_access(&identity.participant, &wavelet_name)
        .await
    {
        Ok(true) => {}
        Ok(false) => return forbidden(),
        Err(error) => return internal_error(error),
    }
    tracing::debug!(wavelet = %wavelet_name, "Fetching snapshot of wavelet");
    let snapshot = match state.server.get_snapshot(&wavelet_name).await {
        Ok(Some(committed)) => committed.snapshot,
        Ok(None) => return forbidden(),
        Err(error) => return internal_error(error),
    };
    if let Some(document_id) = waveref.document_id() {
        match snapshot.document(document_id) {
            Some(document) => proto_json(serialize_document(document)),
            None => forbidden(),
        }
    } else if waveref.wavelet_id().is_some() {
        proto_json(serialize_wavelet(&snapshot, snapshot.hashed_version()))
    } else {
        proto_json(
            serialize_wavelet(&snapshot, snapshot.hashed_version()).and_then(|wavelet| {
                Ok(WaveViewSnapshot {
                    wave_id: Some(ModernIdSerialiser.serialise_wave_id(waveref.wave_id())?),
                    wavelet: vec![wavelet],
                })
            }),
        )
    }
}

/// `GET /waveref/<waveref>`: redirects to the client at that wave, by way
/// of the sign-in page if nobody is signed in.
pub(super) async fn waveref(context: Context, uri: Uri) -> Response {
    let path = uri.path().replacen("/waveref/", "", 1);
    let target = if context.identity.is_some() {
        format!("/#{path}")
    } else {
        format!("/auth/signin?r=/#{path}")
    };
    match HeaderValue::from_str(&target) {
        Ok(location) => (StatusCode::FOUND, [(header::LOCATION, location)]).into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

/// Most accounts one `/directory` request returns.
const MAX_DIRECTORY_RESULTS: usize = 50;

/// `GET /directory?query=&limit=`: the accounts of this server, for
/// suggesting people to add to a wave (not in the original, which had no
/// way to find other users). 403 unless signed in; 404 when
/// `directory_enabled` is off. `kind` is `human`, `agent` or `robot`.
pub(super) async fn directory(
    State(state): State<AppState>,
    context: Context,
    Query(params): Params,
) -> Response {
    if !state.config.directory_open() {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !context.identity.as_ref().is_some_and(|i| i.can_read()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let query = params.get("query").map_or("", |q| q.trim());
    let limit = params
        .get("limit")
        .and_then(|limit| limit.parse::<usize>().ok())
        .unwrap_or(20)
        .clamp(1, MAX_DIRECTORY_RESULTS);
    if query.chars().count() > 256 {
        return failure(StatusCode::BAD_REQUEST, "Query too long");
    }
    let found = match state
        .auth
        .accounts()
        .search_accounts(query, limit + 1)
        .await
    {
        Ok(found) => found,
        Err(_) => return internal_error("cannot read the accounts"),
    };
    let accounts = found
        .into_iter()
        // The account that owns welcome waves is not someone to talk to.
        .filter(|(address, _)| !address.starts_with("welcome-bot@"))
        .take(limit)
        .map(|(address, kind)| {
            json!({
                "address": address,
                "name": profile_name(&address),
                "image_url": format!("/iniavatars/100x100/{address}"),
                "kind": kind,
            })
        })
        .collect::<Vec<_>>();
    json(StatusCode::OK, json!({ "accounts": accounts }))
}

/// The display name of an address: the capitalised parts of its name
/// (`FetchProfilesService.SIMPLE_PROFILES_FETCHER`).
pub(super) fn profile_name(address: &str) -> String {
    let name = address.split('@').next().unwrap_or("");
    if name.is_empty() {
        // The shared domain participant, `@example.com`.
        return address.to_owned();
    }
    name.split(['.', '_'])
        .filter(|fragment| !fragment.is_empty())
        .map(|fragment| {
            let mut chars = fragment.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Most addresses one profile request may name.
const MAX_PROFILE_ADDRESSES: usize = 500;

/// `GET /profile/?addresses=a@x,b@x`: names and avatar URLs derived from
/// the addresses (`InitialsProfilesFetcher`). 403 unless signed in.
///
/// Addition to the original: the profile of a robot account carries
/// `"robotKind": "agent"` (a built-in AI agent) or `"robotKind": "robot"`
/// (a registered robot) next to the numbered proto fields; people's
/// profiles are unchanged. Clients that do not know the member ignore it.
pub(super) async fn profiles(
    State(state): State<AppState>,
    context: Context,
    Query(params): Params,
) -> Response {
    if !context.identity.as_ref().is_some_and(|i| i.can_read()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(addresses) = params.get("addresses") else {
        return failure(StatusCode::BAD_REQUEST, "Missing addresses parameter");
    };
    let profiles = addresses
        .split(',')
        .take(MAX_PROFILE_ADDRESSES)
        .map(|address| FetchedProfile {
            address: Some(address.to_owned()),
            name: Some(profile_name(address)),
            image_url: Some(format!("/iniavatars/100x100/{address}")),
            profile_url: Some(String::new()),
        })
        .collect::<Vec<_>>();
    let mut kinds = Vec::with_capacity(profiles.len());
    for profile in &profiles {
        let address = profile.address.as_deref().unwrap_or("");
        let kind = match resonator_model::id::ParticipantId::of(address) {
            Ok(id) => match state.auth.accounts().get_account(&id).await {
                Ok(Some(account)) if account.is_agent() => Some("agent"),
                Ok(Some(account)) if account.is_robot() => Some("robot"),
                _ => None,
            },
            Err(_) => None,
        };
        kinds.push(kind);
    }
    if kinds.iter().all(Option::is_none) {
        return proto_json(Ok(ProfileResponse { profiles }));
    }
    let mut value = match to_json(&ProfileResponse { profiles })
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
    {
        Some(value) => value,
        None => return internal_error("cannot write the profiles"),
    };
    if let Some(list) = value.get_mut("1").and_then(serde_json::Value::as_array_mut) {
        for (profile, kind) in list.iter_mut().zip(kinds) {
            if let (Some(object), Some(kind)) = (profile.as_object_mut(), kind) {
                object.insert("robotKind".to_owned(), json!(kind));
            }
        }
    }
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        value.to_string(),
    )
        .into_response()
}

/// The initials shown for an address: the first letters of the first two
/// parts of its name.
fn initials(address: &str) -> String {
    profile_name(address)
        .split(' ')
        .filter_map(|part| part.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

/// `GET /iniavatars/<width>x<height>/<address>`: an avatar with the
/// address's initials, as SVG. (The original serves one fixed JPEG.)
pub(super) async fn initials_avatar(Path(path): Path<String>) -> Response {
    let (size, address) = path.split_once('/').unwrap_or(("100x100", path.as_str()));
    let (width, height) = size
        .split_once('x')
        .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
        .unwrap_or((100, 100));
    let (width, height) = (width.clamp(8, 1024), height.clamp(8, 1024));
    let address = address.to_lowercase();
    // A stable colour per address.
    let hue = address.bytes().fold(0u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte.into())
    }) % 360;
    // Alphanumeric characters only, so there is nothing to escape.
    let text = initials(&address);
    let font_size = width.min(height) * 2 / 5;
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
         viewBox=\"0 0 {width} {height}\"><rect width=\"100%\" height=\"100%\" \
         fill=\"hsl({hue},45%,45%)\"/><text x=\"50%\" y=\"50%\" dy=\".35em\" \
         text-anchor=\"middle\" font-family=\"sans-serif\" font-size=\"{font_size}\" \
         fill=\"#fff\">{text}</text></svg>"
    );
    (
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        svg,
    )
        .into_response()
}

/// `GET /locale/`: the signed-in user's locale.
pub(super) async fn get_locale(State(state): State<AppState>, context: Context) -> Response {
    let Some(identity) = context.identity.as_ref().filter(|i| i.can_read()) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    match state
        .auth
        .accounts()
        .get_account(&identity.participant)
        .await
    {
        Ok(account) => {
            let locale = account
                .as_ref()
                .and_then(AccountData::as_human)
                .and_then(|human| human.locale.clone());
            json(StatusCode::OK, json!({"locale": locale}))
        }
        Err(error) => internal_error(error),
    }
}

fn is_locale(locale: &str) -> bool {
    !locale.is_empty()
        && locale.len() <= 35
        && locale
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// `POST /locale/` with a `locale` parameter (query or form): stores the
/// signed-in user's locale. 403 unless signed in.
pub(super) async fn set_locale(
    State(state): State<AppState>,
    context: Context,
    Query(mut params): Params,
    body: Bytes,
) -> Response {
    let Some(identity) = context.identity.as_ref().filter(|i| i.can_write()) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let locale = params.remove("locale").or_else(|| {
        // A form body.
        std::str::from_utf8(&body).ok().and_then(|form| {
            form.split('&')
                .filter_map(|pair| pair.split_once('='))
                .find(|(key, _)| *key == "locale")
                .map(|(_, value)| value.to_owned())
        })
    });
    let Some(locale) = locale.filter(|locale| is_locale(locale)) else {
        return failure(StatusCode::BAD_REQUEST, "Missing or invalid locale");
    };
    let accounts = state.auth.accounts();
    let account = match accounts.get_account(&identity.participant).await {
        Ok(account) => account,
        Err(error) => return internal_error(error),
    };
    let Some(AccountData::Human(mut human)) = account else {
        return failure(StatusCode::BAD_REQUEST, "Only human accounts have a locale");
    };
    human.locale = Some(locale);
    match accounts.put_account(AccountData::Human(human)).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => internal_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_names_follow_the_original() {
        assert_eq!(profile_name("john.smith@example.com"), "John Smith");
        assert_eq!(profile_name("john_smith@example.com"), "John Smith");
        assert_eq!(profile_name("john..smith.@example.com"), "John Smith");
        assert_eq!(profile_name("alice@example.com"), "Alice");
        assert_eq!(profile_name("@example.com"), "@example.com");
        assert_eq!(profile_name("\u{e9}mile@example.com"), "\u{c9}mile");
    }

    #[test]
    fn initials_are_safe_text() {
        assert_eq!(initials("john.smith@example.com"), "JS");
        assert_eq!(initials("alice@example.com"), "A");
        assert_eq!(initials("a.b.c@example.com"), "AB");
        assert_eq!(initials("<script>@example.com"), "S");
        assert_eq!(initials("@example.com"), "E");
        assert_eq!(initials("&<>\"@x"), "");
    }

    #[test]
    fn locales_are_validated() {
        for ok in ["en", "en_GB", "pt-BR", "zh_Hant_TW"] {
            assert!(is_locale(ok), "{ok}");
        }
        for bad in ["", "en GB", "en;x", "<script>", &"x".repeat(36)] {
            assert!(!is_locale(bad), "{bad}");
        }
    }
}
