//! `/robot/dataapi`, `/robot/dataapi/rpc` and `/robot/rpc`
//! (`DataApiServlet`, `ActiveApiServlet`, `BaseApiServlet`).
//!
//! As in the original: the body is a JSON-RPC operation or a list of
//! them, the answer (HTTP 200, `application/json`) is the list of
//! responses in the order of the requests, a body that is not operations
//! gets HTTP 400. Unlike the original, a response is an error if the
//! server rejected the change it made (`responses_with_submit_errors`),
//! and authentication is a bearer token or session instead of OAuth 1.

use std::sync::Arc;

use axum::Extension;
use axum::body::Bytes;
use axum::http::StatusCode;
use axum::response::Response;
use resonator_robots::api::{
    JsonRpcResponse, OperationRequest, ParamsProperty, deserialize_operations,
};
use resonator_robots::ops::Registry;
use resonator_server::http::RequestContext;
use serde_json::Value;

use super::{Shared, error, is_robot, json_response};

fn parse(body: &Bytes) -> Result<Vec<OperationRequest>, Response> {
    let text = std::str::from_utf8(body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "The body is not UTF-8"))?;
    deserialize_operations(text).map_err(|e| {
        error(
            StatusCode::BAD_REQUEST,
            &format!("Unable to parse Json to list of OperationRequests: {e}"),
        )
    })
}

fn respond(responses: &[JsonRpcResponse]) -> Response {
    let value = serde_json::to_value(responses).unwrap_or(Value::Array(Vec::new()));
    json_response(StatusCode::OK, value)
}

/// Whether any operation asks to act as a proxy (`proxyingFor`).
fn uses_proxy(operations: &[OperationRequest]) -> bool {
    operations.iter().any(|operation| {
        operation
            .string(ParamsProperty::ProxyingFor)
            .is_some_and(|proxy| !proxy.is_empty())
    })
}

/// The Data API: programs acting as the user that authenticated.
pub async fn data_api(
    Extension(context): Extension<RequestContext>,
    Extension(shared): Extension<Arc<Shared>>,
    body: Bytes,
) -> Response {
    let Some(identity) = context.identity else {
        return error(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    if !identity.can_write() {
        return error(
            StatusCode::FORBIDDEN,
            "The Data API needs a token with the write scope",
        );
    }
    let operations = match parse(&body) {
        Ok(operations) => operations,
        Err(response) => return response,
    };
    // Acting as `robot+someone@domain` is for robots, which own their
    // proxies; for a person it would mean writing as an address that is
    // not theirs.
    if !is_robot(&identity) && uses_proxy(&operations) {
        return error(
            StatusCode::FORBIDDEN,
            "proxyingFor is only available to robots",
        );
    }
    let result = shared
        .executor
        .execute_detailed(&identity.participant, Registry::DataApi, &operations, None)
        .await;
    respond(&result.responses_with_submit_errors())
}

/// The active robot API: robots acting on their own, or answering events
/// they got from the event stream.
pub async fn active_api(
    Extension(context): Extension<RequestContext>,
    Extension(shared): Extension<Arc<Shared>>,
    body: Bytes,
) -> Response {
    let Some(identity) = context.identity else {
        return error(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    if !is_robot(&identity) {
        return error(
            StatusCode::FORBIDDEN,
            "/robot/rpc is for robots; use /robot/dataapi",
        );
    }
    let operations = match parse(&body) {
        Ok(operations) => operations,
        Err(response) => return response,
    };
    // Count the operations as a reaction to what the robot was last sent,
    // so that robots answering each other through the stream are stopped
    // by the loop guard like webhook robots.
    let hop = shared.hub.recent_hop(&identity.participant);
    let reactor = shared
        .dispatcher
        .reactor(identity.participant.clone(), Registry::Active, hop);
    let result = reactor.execute(operations).await;
    respond(&result.responses_with_submit_errors())
}
