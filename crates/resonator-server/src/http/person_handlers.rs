//! Person mode's settings and "continue as new wave", through the
//! [`PersonControl`] federation installs ([`AppState::set_person_control`]).
//!
//! Device mode, the device's signed-in account only (404 otherwise):
//! - `GET /person`: this person and device, the devices (name, admin,
//!   last seen, lease holder, revoked), the host lease, anchor, co-hosts.
//! - `POST /person/revoke` `{"device"}`: revokes a device (admin).
//! - `POST /person/anchor` `{"anchor"}`: whether this device is an anchor.
//! - `POST /person/phrase` `{"confirm": true}`: the recovery phrase
//!   (admin; `Cache-Control: no-store`).
//! - `POST /person/recover` `{"phrase", "revoke_old", "peers"}`: makes
//!   this (fresh) device the person of the phrase; the node restarts.
//! - `POST /person/cohost` `{"domain", "until"}`: grants (or with `until`
//!   0 withdraws) co-hosting to a domain (admin).
//! - `GET /person/pair`, `POST /person/pair` `{"admin"}`: pairing
//!   invitations; a new one comes with its link and a QR code (SVG).
//! - `POST /person/pair/{id}/confirm`, `DELETE /person/pair/{id}`.
//! - `POST /person/pair/join` `{"link", "name"}`: pairs this device by a
//!   link; the node restarts as a device of that person.
//!
//! Every signed-in user:
//! - `GET /waves/host-status?wave=`: whether the wave's host is frozen.
//! - `POST /waves/continue` `{"wave"}`: continues a wave whose host is
//!   frozen as a new wave of the caller ([`crate::waveserver::fork`]).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::{self, HeaderValue};
use axum::response::Response;
use futures_util::future::BoxFuture;
use resonator_model::id::constants::CONVERSATION_ROOT_WAVELET;
use resonator_model::id::{ParticipantId, WaveId, WaveletId, WaveletName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{AppState, Context, RequestContext, failure, json};
use crate::waveserver::fork::ForkError;

/// A refusal of a [`PersonControl`] call: an HTTP status and a message
/// for people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlError {
    pub status: u16,
    pub message: String,
}

impl ControlError {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        ControlError {
            status,
            message: message.into(),
        }
    }

    fn response(&self) -> Response {
        failure(
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            self.message.clone(),
        )
    }
}

/// Whether a wave's host can be reached.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct HostStatus {
    /// The domain that hosts it.
    pub host: String,
    /// Hosted here.
    pub local: bool,
    /// The host is frozen: the wave may be continued as a new wave.
    pub frozen: bool,
    /// Why, for people.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

type Answer<'a> = BoxFuture<'a, Result<Value, ControlError>>;

/// What federation does for the person settings (see the module docs).
/// Results are JSON for the client as is.
pub trait PersonControl: Send + Sync {
    fn overview(&self) -> Answer<'_>;
    fn revoke<'a>(&'a self, device: &'a str) -> Answer<'a>;
    fn set_anchor(&self, anchor: bool) -> Answer<'_>;
    /// The recovery phrase (admin devices). Never log it.
    fn phrase(&self) -> Result<String, ControlError>;
    fn recover<'a>(&'a self, phrase: &'a str, revoke_old: bool, peers: Vec<String>) -> Answer<'a>;
    fn co_host<'a>(&'a self, domain: &'a str, until: i64) -> Answer<'a>;
    fn invite(&self, admin: bool) -> Answer<'_>;
    fn invitations(&self) -> Answer<'_>;
    fn confirm<'a>(&'a self, id: &'a str) -> Answer<'a>;
    fn cancel<'a>(&'a self, id: &'a str) -> Answer<'a>;
    /// Pairs this device by `text`: a pairing link or a pairing code.
    fn join<'a>(&'a self, text: &'a str, name: &'a str) -> Answer<'a>;
    /// How far the join under way has come: `{"stage": ...}` with
    /// `finding`, `verifying`, `waiting`, `done`, or null when none runs.
    fn join_progress(&self) -> Value;
    fn host_status<'a>(&'a self, wavelet: &'a WaveletName) -> BoxFuture<'a, HostStatus>;
}

impl AppState {
    /// Installs what answers `/person/...` and the host status. Returns
    /// false if one was installed already.
    pub fn set_person_control(&self, control: Arc<dyn PersonControl>) -> bool {
        self.person_control.set(control).is_ok()
    }
}

/// The control, for the device's own signed-in account.
fn device_control(
    state: &AppState,
    context: &RequestContext,
) -> Result<Arc<dyn PersonControl>, Box<Response>> {
    let not_found = || {
        Box::new(failure(
            StatusCode::NOT_FOUND,
            "Person mode is not enabled.",
        ))
    };
    let device = state.device.as_ref().ok_or_else(not_found)?;
    let control = state.person_control.get().cloned().ok_or_else(not_found)?;
    match &context.identity {
        Some(identity) if identity.can_read() && identity.participant == device.participant => {
            Ok(control)
        }
        _ => Err(Box::new(failure(StatusCode::FORBIDDEN, "Sign in first."))),
    }
}

fn answer(result: Result<Value, ControlError>) -> Response {
    match result {
        Ok(value) => json(StatusCode::OK, value),
        Err(error) => error.response(),
    }
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

macro_rules! control {
    ($state:expr, $context:expr) => {
        match device_control(&$state, &$context) {
            Ok(control) => control,
            Err(response) => return *response,
        }
    };
}

pub(super) async fn overview(State(state): State<AppState>, context: Context) -> Response {
    let control = control!(state, context);
    no_store(answer(control.overview().await))
}

#[derive(Debug, Deserialize)]
pub(super) struct DeviceBody {
    device: String,
}

pub(super) async fn revoke(
    State(state): State<AppState>,
    context: Context,
    Json(body): Json<DeviceBody>,
) -> Response {
    let control = control!(state, context);
    answer(control.revoke(&body.device).await)
}

#[derive(Debug, Deserialize)]
pub(super) struct AnchorBody {
    anchor: bool,
}

pub(super) async fn anchor(
    State(state): State<AppState>,
    context: Context,
    Json(body): Json<AnchorBody>,
) -> Response {
    let control = control!(state, context);
    answer(control.set_anchor(body.anchor).await)
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct ConfirmBody {
    #[serde(default)]
    confirm: bool,
}

pub(super) async fn phrase(
    State(state): State<AppState>,
    context: Context,
    Json(body): Json<ConfirmBody>,
) -> Response {
    let control = control!(state, context);
    if !body.confirm {
        return failure(
            StatusCode::BAD_REQUEST,
            "Confirm that you want to see the recovery phrase.",
        );
    }
    let response = match control.phrase() {
        Ok(words) => json(StatusCode::OK, json!({ "phrase": words })),
        Err(error) => error.response(),
    };
    no_store(response)
}

#[derive(Deserialize)]
pub(super) struct RecoverBody {
    phrase: String,
    #[serde(default)]
    revoke_old: bool,
    #[serde(default)]
    peers: Vec<String>,
}

pub(super) async fn recover(
    State(state): State<AppState>,
    context: Context,
    Json(body): Json<RecoverBody>,
) -> Response {
    let control = control!(state, context);
    let result = control
        .recover(&body.phrase, body.revoke_old, body.peers)
        .await;
    drop(body.phrase);
    no_store(answer(result))
}

#[derive(Debug, Deserialize)]
pub(super) struct CoHostBody {
    domain: String,
    #[serde(default)]
    until: i64,
}

pub(super) async fn co_host(
    State(state): State<AppState>,
    context: Context,
    Json(body): Json<CoHostBody>,
) -> Response {
    let control = control!(state, context);
    answer(control.co_host(body.domain.trim(), body.until).await)
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct InviteBody {
    #[serde(default)]
    admin: bool,
}

pub(super) async fn invite(
    State(state): State<AppState>,
    context: Context,
    body: Option<Json<InviteBody>>,
) -> Response {
    let control = control!(state, context);
    let admin = body.is_some_and(|Json(body)| body.admin);
    no_store(answer(control.invite(admin).await))
}

pub(super) async fn invitations(State(state): State<AppState>, context: Context) -> Response {
    let control = control!(state, context);
    no_store(answer(control.invitations().await))
}

pub(super) async fn confirm(
    State(state): State<AppState>,
    context: Context,
    Path(id): Path<String>,
) -> Response {
    let control = control!(state, context);
    answer(control.confirm(&id).await)
}

pub(super) async fn cancel(
    State(state): State<AppState>,
    context: Context,
    Path(id): Path<String>,
) -> Response {
    let control = control!(state, context);
    answer(control.cancel(&id).await)
}

/// `{"link": ...}` or `{"code": ...}` (either names the invitation).
#[derive(Deserialize)]
pub(super) struct JoinBody {
    #[serde(default)]
    link: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    name: String,
}

pub(super) async fn join(
    State(state): State<AppState>,
    context: Context,
    Json(body): Json<JoinBody>,
) -> Response {
    let control = control!(state, context);
    let text = if body.code.trim().is_empty() {
        body.link.trim()
    } else {
        body.code.trim()
    };
    if text.is_empty() {
        return failure(StatusCode::BAD_REQUEST, "Give a pairing code or link.");
    }
    no_store(answer(control.join(text, body.name.trim()).await))
}

pub(super) async fn join_progress(State(state): State<AppState>, context: Context) -> Response {
    let control = control!(state, context);
    no_store(json(StatusCode::OK, control.join_progress()))
}

#[derive(Debug, Deserialize)]
pub(super) struct WaveQuery {
    wave: Option<String>,
}

/// The signed-in caller and the root conversation of the wave named.
fn wave_request(
    context: &RequestContext,
    wave: Option<&str>,
) -> Result<(ParticipantId, WaveId, WaveletName), Box<Response>> {
    let Some(identity) = context.identity.as_ref().filter(|i| i.can_read()) else {
        return Err(Box::new(failure(StatusCode::FORBIDDEN, "Sign in first.")));
    };
    let bad = || Box::new(failure(StatusCode::BAD_REQUEST, "Not a valid wave id."));
    let wave_id = WaveId::deserialise(wave.unwrap_or_default()).map_err(|_| bad())?;
    let wavelet_id =
        WaveletId::of(wave_id.domain(), CONVERSATION_ROOT_WAVELET).map_err(|_| bad())?;
    Ok((
        identity.participant.clone(),
        wave_id.clone(),
        WaveletName::of(wave_id, wavelet_id),
    ))
}

async fn status_of(state: &AppState, name: &WaveletName) -> HostStatus {
    if state.server.is_local_wavelet(name) {
        return HostStatus {
            host: state.config.domain.clone(),
            local: true,
            frozen: false,
            reason: String::new(),
        };
    }
    match state.person_control.get() {
        Some(control) => control.host_status(name).await,
        None => HostStatus {
            host: name.wavelet_id.domain().to_owned(),
            ..HostStatus::default()
        },
    }
}

/// Whether `participant` takes part in the wavelet.
async fn takes_part(state: &AppState, name: &WaveletName, participant: &ParticipantId) -> bool {
    matches!(
        state
            .server
            .with_wavelet(name, |data| data.participants().contains(participant))
            .await,
        Ok(Some(true))
    )
}

pub(super) async fn host_status(
    State(state): State<AppState>,
    context: Context,
    Query(query): Query<WaveQuery>,
) -> Response {
    let (participant, _, name) = match wave_request(&context, query.wave.as_deref()) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    if !takes_part(&state, &name, &participant).await {
        return failure(StatusCode::NOT_FOUND, "No such wave.");
    }
    json(StatusCode::OK, json!(status_of(&state, &name).await))
}

#[derive(Debug, Deserialize)]
pub(super) struct ContinueBody {
    wave: String,
}

pub(super) async fn continue_wave(
    State(state): State<AppState>,
    context: Context,
    Json(body): Json<ContinueBody>,
) -> Response {
    let (participant, wave_id, name) = match wave_request(&context, Some(&body.wave)) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    if context.identity.as_ref().is_some_and(|i| !i.can_write()) {
        return failure(StatusCode::FORBIDDEN, "This sign-in cannot write.");
    }
    if !takes_part(&state, &name, &participant).await {
        return failure(StatusCode::NOT_FOUND, "No such wave.");
    }
    let status = status_of(&state, &name).await;
    if !status.frozen {
        return failure(
            StatusCode::CONFLICT,
            "The wave's host is reachable; it can only be continued once its host is gone.",
        );
    }
    match state.server.fork_wave(&wave_id, &participant).await {
        Ok(new_wave) => json(
            StatusCode::OK,
            json!({ "wave": new_wave.serialise().unwrap_or_else(|_| new_wave.to_string()) }),
        ),
        Err(ForkError::NotFound) => failure(StatusCode::NOT_FOUND, "No such wave."),
        Err(ForkError::NotParticipant) => failure(StatusCode::FORBIDDEN, "Not a participant."),
        Err(error) => {
            tracing::warn!(wave = %wave_id, %error, "cannot continue a wave");
            failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The wave could not be continued.",
            )
        }
    }
}
