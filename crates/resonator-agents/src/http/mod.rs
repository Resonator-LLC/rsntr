//! The HTTP endpoints of robots and tools, mounted next to the wave
//! server's own routes (`PreparedApp::serve(extra_routes)`).
//!
//! | Route | Who | What |
//! |---|---|---|
//! | `POST /robot/dataapi`, `POST /robot/dataapi/rpc` | any identity with `write` scope | the Data API (`Registry::DataApi`) |
//! | `POST /robot/rpc` | robot tokens only | the active robot API (`Registry::Active`) |
//! | `/robot/register…` | people (session or token) | robot registration, see [`register`] |
//! | `GET /robot/events` | robot tokens only | event bundles of robots without URL, as server-sent events |
//! | `POST /mcp` | bearer tokens | the Model Context Protocol endpoint, see [`crate::mcp`] |
//!
//! Every route finds the caller in the server's `RequestContext`.

// Handlers return ready responses as errors, as axum handlers do.
#![allow(clippy::result_large_err)]

pub mod dataapi;
pub mod events;
pub mod register;

use std::sync::Arc;

use axum::Router;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use resonator_robots::events::RobotDispatcher;
use resonator_robots::ops::OperationExecutor;
use resonator_server::auth::{Credential, Identity};
use resonator_server::http::AppState;
use resonator_server::storage::RobotStore;
use serde_json::{Value, json};

use crate::robots::connector::{EventHub, RobotConnector};

/// What the robot and tool endpoints share.
pub struct Shared {
    pub executor: Arc<OperationExecutor>,
    pub dispatcher: RobotDispatcher,
    pub hub: Arc<EventHub>,
    pub connector: RobotConnector,
    pub robots: RobotStore,
}

/// The routes, with the settings of the configuration applied: nothing
/// under `/robot` unless `robot_api_enabled`, no `/mcp` unless
/// `mcp_enabled`.
pub fn routes(state: &AppState, shared: Arc<Shared>) -> Router<AppState> {
    let mut router = Router::new();
    if state.config.robot_api_enabled {
        router = router
            .route("/robot/dataapi", post(dataapi::data_api))
            .route("/robot/dataapi/rpc", post(dataapi::data_api))
            .route("/robot/rpc", post(dataapi::active_api))
            .route(
                "/robot/register",
                get(register::list).post(register::create),
            )
            .route(
                "/robot/register/{address}",
                get(register::show)
                    .put(register::update)
                    .delete(register::remove),
            )
            .route(
                "/robot/register/{address}/token",
                post(register::rotate_token),
            )
            .route(
                "/robot/register/{address}/secret",
                post(register::rotate_secret),
            )
            .route("/robot/events", get(events::stream));
    }
    if state.config.mcp_enabled {
        router = router.route(
            "/mcp",
            post(crate::mcp::handle)
                .get(crate::mcp::method_not_allowed)
                .delete(crate::mcp::method_not_allowed),
        );
    }
    router.layer(axum::Extension(shared))
}

/// A JSON response.
pub(crate) fn json_response(status: StatusCode, value: Value) -> Response {
    (
        status,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        value.to_string(),
    )
        .into_response()
}

/// A JSON error `{"error": message}`.
pub(crate) fn error(status: StatusCode, message: &str) -> Response {
    let mut response = json_response(status, json!({ "error": message }));
    if status == StatusCode::UNAUTHORIZED {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    response
}

/// Whether the identity is a robot's (it authenticated with a robot
/// token).
pub(crate) fn is_robot(identity: &Identity) -> bool {
    matches!(identity.credential, Credential::RobotToken(_))
}
