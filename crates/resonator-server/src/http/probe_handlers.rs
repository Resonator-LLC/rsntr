//! `GET /federation/probe?address=<address>`: whether a participant of
//! another domain can be added to a wave from here, for the client's
//! add-participant dialog. Asks that domain's server (federation's
//! `probe`) through the [`RemoteProbe`] federation installs; 404 without
//! federation.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use futures_util::future::BoxFuture;
use resonator_model::id::ParticipantId;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{AppState, Context, failure, json};

/// Probes allowed per signed-in user per minute.
pub const PROBES_PER_MINUTE: u32 = 30;

/// What a probe of a remote address found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeOutcome {
    /// The address's server answered.
    pub reachable: bool,
    /// This server and that one may federate.
    pub allowed: bool,
    /// Whether the address exists there, if it said.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
    /// Why not, for people.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Asks other servers about their addresses (federation's `probe`).
pub trait RemoteProbe: Send + Sync {
    fn probe<'a>(&'a self, address: &'a ParticipantId) -> BoxFuture<'a, ProbeOutcome>;
}

impl AppState {
    /// Installs what answers `/federation/probe`. Returns false if one was
    /// installed already.
    pub fn set_remote_probe(&self, probe: Arc<dyn RemoteProbe>) -> bool {
        self.remote_probe.set(probe).is_ok()
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct ProbeQuery {
    address: Option<String>,
}

/// `GET /federation/probe?address=bob@example.org`: signed-in users only,
/// [`PROBES_PER_MINUTE`] each. 400 for an address that is not valid or
/// not of another domain, 429 over the limit.
pub(super) async fn probe(
    State(state): State<AppState>,
    context: Context,
    Query(query): Query<ProbeQuery>,
) -> Response {
    let Some(remote) = state.remote_probe.get().cloned() else {
        return failure(StatusCode::NOT_FOUND, "Federation is not enabled.");
    };
    let Some(identity) = context.identity.as_ref().filter(|i| i.can_read()) else {
        return failure(StatusCode::FORBIDDEN, "Sign in first.");
    };
    let Ok(address) = ParticipantId::of(query.address.as_deref().unwrap_or_default()) else {
        return failure(StatusCode::BAD_REQUEST, "Not a valid address.");
    };
    if address.domain() == state.config.domain {
        return failure(StatusCode::BAD_REQUEST, "The address is of this server.");
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        });
    let limited = state
        .probe_limiter
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .acquire(identity.participant.address(), now);
    if let Err(wait) = limited {
        return failure(
            StatusCode::TOO_MANY_REQUESTS,
            format!("Too many probes; try again in {} s.", (wait + 999) / 1000),
        );
    }
    let outcome = remote.probe(&address).await;
    json(StatusCode::OK, json!(outcome))
}
