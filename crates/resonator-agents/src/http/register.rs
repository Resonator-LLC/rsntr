//! Robot registration as JSON (`RobotRegistrationServlet`, `register/`).
//!
//! - `POST /robot/register` `{"name": "echo", "url": "https://bot.example/x"?,
//!   "events": ["WAVELET_SELF_ADDED", …]?}` → 201 `{"address", "url",
//!   "token", "webhook_secret", "events", "capabilities_error"?}`. The
//!   token (`wn_bot_…`) and the webhook secret (`wn_whsec_…`) are shown
//!   only here and when rotated. Without a URL the robot gets its events
//!   from `GET /robot/events`; `events` chooses which (default: added,
//!   blip created, document changed, participants changed). With a URL
//!   the capabilities are fetched from `<url>/_wave/capabilities.xml`.
//! - `GET /robot/register` → `{"robots": [..]}`: the caller's robots, all
//!   of them for the admin.
//! - `GET /robot/register/{address}` → one robot.
//! - `PUT /robot/register/{address}` `{"url": "…" | null, "events": [..]?}`
//!   → changes the URL and fetches the capabilities again.
//! - `POST /robot/register/{address}/token` → `{"token"}`, a new token;
//!   the old one stops working.
//! - `POST /robot/register/{address}/secret` → `{"webhook_secret"}`.
//! - `DELETE /robot/register/{address}` → 204.
//!
//! Only people manage robots, each their own; the admin manages all.
//! Robots that belong to someone else are reported as not found. Names
//! follow the agent name rule (`is_valid_robot_id`): lower-case letters,
//! digits, `.`, `-`, `_`; never `+` or `#`.

use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use resonator_model::id::ParticipantId;
use resonator_server::account::{
    AccountData, Capability, EventType, ProtocolVersion, RobotAccountData, RobotCapabilities,
};
use resonator_server::auth::Identity;
use resonator_server::config::is_valid_robot_id;
use resonator_server::http::{AppState, RequestContext};
use resonator_server::storage::RobotRegistration;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Shared, error, is_robot, json_response};
use crate::robots::sign;

/// The events a robot without URL gets unless it says otherwise.
pub const DEFAULT_STREAM_EVENTS: [EventType; 4] = [
    EventType::WaveletSelfAdded,
    EventType::WaveletBlipCreated,
    EventType::DocumentChanged,
    EventType::WaveletParticipantsChanged,
];

#[derive(Debug, Deserialize)]
pub struct CreateRequest {
    name: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    events: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRequest {
    /// Absent: unchanged; `null`: no URL (event stream).
    #[serde(default, deserialize_with = "double_option")]
    url: Option<Option<String>>,
    #[serde(default)]
    events: Option<Vec<String>>,
}

fn double_option<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::deserialize(deserializer)?))
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The caller, if a person who may manage robots (`write` for changes).
fn manager(context: &RequestContext, write: bool) -> Result<Identity, Response> {
    let Some(identity) = context.identity.clone() else {
        return Err(error(StatusCode::UNAUTHORIZED, "Authentication required"));
    };
    if is_robot(&identity) {
        return Err(error(StatusCode::FORBIDDEN, "Robots cannot manage robots"));
    }
    if (write && !identity.can_write()) || !identity.can_read() {
        return Err(error(StatusCode::FORBIDDEN, "Insufficient token scope"));
    }
    Ok(identity)
}

fn is_admin(state: &AppState, identity: &Identity) -> bool {
    state.config.admin().as_ref() == Some(&identity.participant)
}

fn stream_capabilities(events: Option<&[String]>) -> Result<RobotCapabilities, Response> {
    let types: Vec<EventType> = match events {
        None => DEFAULT_STREAM_EVENTS.to_vec(),
        Some(names) => names
            .iter()
            .map(|name| {
                name.parse::<EventType>().map_err(|_| {
                    error(
                        StatusCode::BAD_REQUEST,
                        &format!("Unknown event type {name}"),
                    )
                })
            })
            .collect::<Result<_, _>>()?,
    };
    Ok(RobotCapabilities::new(
        types.into_iter().map(Capability::new).collect::<Vec<_>>(),
        "stream",
        ProtocolVersion::DEFAULT,
    ))
}

fn normalize_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_owned()
}

async fn check_url(shared: &Shared, url: &str) -> Result<(), Response> {
    shared
        .connector
        .policy()
        .check_url_resolving(url)
        .await
        .map(|_| ())
        .map_err(|e| error(StatusCode::BAD_REQUEST, &format!("Robot URL refused: {e}")))
}

fn describe(account: &RobotAccountData, registration: &RobotRegistration) -> Value {
    let events: Vec<&str> = account
        .capabilities
        .as_ref()
        .map(|c| c.capabilities.keys().map(|t| t.as_str()).collect())
        .unwrap_or_default();
    json!({
        "address": account.id.address(),
        "owner": registration.owner.address(),
        "url": account.url(),
        "events": events,
        "capabilities_known": account.capabilities.is_some(),
        "created_at": registration.created_at,
    })
}

fn storage_error(error: impl std::fmt::Display) -> Response {
    tracing::error!(%error, "robot registration storage error");
    self::error(StatusCode::INTERNAL_SERVER_ERROR, "Storage error")
}

/// The robot at `address` if the caller may manage it.
async fn owned(
    state: &AppState,
    shared: &Shared,
    identity: &Identity,
    address: &str,
) -> Result<(RobotAccountData, RobotRegistration), Response> {
    let not_found = || error(StatusCode::NOT_FOUND, "No such robot");
    let id = ParticipantId::of(address).map_err(|_| not_found())?;
    let registration = shared
        .robots
        .registration(&id)
        .await
        .map_err(storage_error)?
        .ok_or_else(not_found)?;
    if registration.owner != identity.participant && !is_admin(state, identity) {
        return Err(not_found());
    }
    match state.auth.accounts().get_account(&id).await {
        Ok(Some(AccountData::Robot(robot))) if !robot.is_agent() => Ok((robot, registration)),
        Ok(_) => Err(not_found()),
        Err(e) => Err(storage_error(e)),
    }
}

pub async fn create(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Extension(shared): Extension<Arc<Shared>>,
    axum::Json(request): axum::Json<CreateRequest>,
) -> Response {
    let identity = match manager(&context, true) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let admin = is_admin(&state, &identity);
    if !state.config.robot_registration_enabled && !admin {
        return error(
            StatusCode::FORBIDDEN,
            "Robot registration is disabled by the administrator",
        );
    }
    let name = request.name.trim().to_owned();
    if !is_valid_robot_id(&name) {
        return error(
            StatusCode::BAD_REQUEST,
            "Invalid robot name: use lower-case letters, digits, '.', '-' and '_'",
        );
    }
    if !admin {
        match shared
            .robots
            .registrations(Some(&identity.participant))
            .await
        {
            Ok(list) if list.len() >= state.config.robot_max_per_user => {
                return error(
                    StatusCode::FORBIDDEN,
                    &format!(
                        "You have {} robots, the most allowed",
                        state.config.robot_max_per_user
                    ),
                );
            }
            Ok(_) => {}
            Err(e) => return storage_error(e),
        }
    }
    let address = format!("{name}@{}", state.config.domain);
    let Ok(id) = ParticipantId::of(&address) else {
        return error(StatusCode::BAD_REQUEST, "Invalid robot name");
    };
    let url = request
        .url
        .as_deref()
        .map(normalize_url)
        .filter(|u| !u.is_empty());
    if let Some(url) = &url
        && let Err(response) = check_url(&shared, url).await
    {
        return response;
    }
    let account = match &url {
        Some(url) => match RobotAccountData::robot(id.clone(), url.clone(), None, None, true) {
            Ok(account) => account,
            Err(e) => return error(StatusCode::BAD_REQUEST, &e.to_string()),
        },
        None => {
            let capabilities = match stream_capabilities(request.events.as_deref()) {
                Ok(capabilities) => capabilities,
                Err(response) => return response,
            };
            RobotAccountData::streaming_robot(id.clone(), None, Some(capabilities))
        }
    };
    match state
        .auth
        .accounts()
        .create_account(AccountData::Robot(account))
        .await
    {
        Ok(true) => {}
        Ok(false) => return error(StatusCode::CONFLICT, "That address is taken"),
        Err(e) => return storage_error(e),
    }
    let Ok(secret) = sign::new_secret() else {
        let _ = state.auth.accounts().remove_account(&id).await;
        return error(StatusCode::INTERNAL_SERVER_ERROR, "No randomness");
    };
    let registration = RobotRegistration {
        robot: id.clone(),
        owner: identity.participant.clone(),
        webhook_secret: secret.clone(),
        created_at: now_millis(),
    };
    if let Err(e) = shared.robots.put_registration(registration.clone()).await {
        let _ = state.auth.accounts().remove_account(&id).await;
        return storage_error(e);
    }
    // Capabilities: fetched now if possible, else when first needed.
    let mut capabilities_error = None;
    if url.is_some() {
        capabilities_error = refresh_capabilities(&state, &shared, &id).await.err();
    }
    let token = match state.auth.issue_robot_token(&id).await {
        Ok(token) => token,
        Err(e) => return storage_error(e),
    };
    let account = match state.auth.accounts().get_account(&id).await {
        Ok(Some(AccountData::Robot(robot))) => robot,
        _ => return error(StatusCode::INTERNAL_SERVER_ERROR, "Robot vanished"),
    };
    tracing::info!(robot = %id, owner = %identity.participant, "robot registered");
    let mut body = describe(&account, &registration);
    body["token"] = json!(token);
    body["webhook_secret"] = json!(secret);
    if let Some(e) = capabilities_error {
        body["capabilities_error"] = json!(e);
    }
    json_response(StatusCode::CREATED, body)
}

/// Fetches and stores a robot's capabilities.
async fn refresh_capabilities(
    state: &AppState,
    shared: &Shared,
    id: &ParticipantId,
) -> Result<(), String> {
    let Ok(Some(AccountData::Robot(mut account))) = state.auth.accounts().get_account(id).await
    else {
        return Err("no such robot".to_owned());
    };
    let capabilities = shared.connector.fetch(&account).await.map_err(|e| e.0)?;
    account.capabilities = Some(capabilities);
    state
        .auth
        .accounts()
        .put_account(AccountData::Robot(account))
        .await
        .map_err(|e| e.to_string())
}

pub async fn list(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Extension(shared): Extension<Arc<Shared>>,
) -> Response {
    let identity = match manager(&context, false) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let owner = (!is_admin(&state, &identity)).then_some(&identity.participant);
    let registrations = match shared.robots.registrations(owner).await {
        Ok(list) => list,
        Err(e) => return storage_error(e),
    };
    let mut robots = Vec::new();
    for registration in registrations {
        if let Ok(Some(AccountData::Robot(robot))) =
            state.auth.accounts().get_account(&registration.robot).await
        {
            robots.push(describe(&robot, &registration));
        }
    }
    json_response(StatusCode::OK, json!({ "robots": robots }))
}

pub async fn show(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Extension(shared): Extension<Arc<Shared>>,
    Path(address): Path<String>,
) -> Response {
    let identity = match manager(&context, false) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    match owned(&state, &shared, &identity, &address).await {
        Ok((robot, registration)) => json_response(StatusCode::OK, describe(&robot, &registration)),
        Err(response) => response,
    }
}

pub async fn update(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Extension(shared): Extension<Arc<Shared>>,
    Path(address): Path<String>,
    axum::Json(request): axum::Json<UpdateRequest>,
) -> Response {
    let identity = match manager(&context, true) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let (mut robot, registration) = match owned(&state, &shared, &identity, &address).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    if let Some(url) = &request.url {
        let url = url.as_deref().map(normalize_url).filter(|u| !u.is_empty());
        if let Some(url) = &url
            && let Err(response) = check_url(&shared, url).await
        {
            return response;
        }
        if let Err(e) = robot.set_url(url) {
            return error(StatusCode::BAD_REQUEST, &e.to_string());
        }
    }
    if robot.url().is_none() {
        let events = request.events.as_deref();
        if events.is_some() || robot.capabilities.is_none() || request.url.is_some() {
            match stream_capabilities(events) {
                Ok(capabilities) => robot.capabilities = Some(capabilities),
                Err(response) => return response,
            }
        }
    } else {
        // Fetched again below; until then the robot gets nothing.
        robot.capabilities = None;
    }
    if let Err(e) = state
        .auth
        .accounts()
        .put_account(AccountData::Robot(robot.clone()))
        .await
    {
        return storage_error(e);
    }
    let mut capabilities_error = None;
    if robot.url().is_some() {
        capabilities_error = refresh_capabilities(&state, &shared, &robot.id).await.err();
    }
    let robot = match state.auth.accounts().get_account(&robot.id).await {
        Ok(Some(AccountData::Robot(robot))) => robot,
        _ => robot,
    };
    let mut body = describe(&robot, &registration);
    if let Some(e) = capabilities_error {
        body["capabilities_error"] = json!(e);
    }
    json_response(StatusCode::OK, body)
}

pub async fn rotate_token(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Extension(shared): Extension<Arc<Shared>>,
    Path(address): Path<String>,
) -> Response {
    let identity = match manager(&context, true) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let (robot, _) = match owned(&state, &shared, &identity, &address).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    match state.auth.issue_robot_token(&robot.id).await {
        Ok(token) => json_response(StatusCode::OK, json!({ "token": token })),
        Err(e) => storage_error(e),
    }
}

pub async fn rotate_secret(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Extension(shared): Extension<Arc<Shared>>,
    Path(address): Path<String>,
) -> Response {
    let identity = match manager(&context, true) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let (_, mut registration) = match owned(&state, &shared, &identity, &address).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    let Ok(secret) = sign::new_secret() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "No randomness");
    };
    registration.webhook_secret = secret.clone();
    match shared.robots.put_registration(registration).await {
        Ok(()) => json_response(StatusCode::OK, json!({ "webhook_secret": secret })),
        Err(e) => storage_error(e),
    }
}

pub async fn remove(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Extension(shared): Extension<Arc<Shared>>,
    Path(address): Path<String>,
) -> Response {
    let identity = match manager(&context, true) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let (robot, _) = match owned(&state, &shared, &identity, &address).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    if let Err(e) = state.auth.accounts().remove_account(&robot.id).await {
        return storage_error(e);
    }
    if let Err(e) = shared.robots.remove_registration(&robot.id).await {
        return storage_error(e);
    }
    tracing::info!(robot = %robot.id, by = %identity.participant, "robot removed");
    StatusCode::NO_CONTENT.into_response()
}
