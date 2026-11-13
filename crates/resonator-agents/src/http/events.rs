//! `GET /robot/events`: the event bundles of a robot without URL, as
//! server-sent events.
//!
//! Each bundle is one event `event: bundle` whose `data` is the bundle's
//! JSON, exactly what a webhook robot would be posted. The robot answers
//! with operations on `POST /robot/rpc`. A comment line is sent every 15
//! seconds to keep the connection open. Bundles that wait while the robot
//! is not reading are bounded (`robot_event_stream_buffer`, oldest dropped);
//! bundles for a robot nobody listens for are dropped (delivery is at most
//! once). Several connections of one robot each get every bundle.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Extension;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use resonator_model::id::ParticipantId;
use resonator_server::http::{AppState, RequestContext};

use super::{Shared, error, is_robot};
use crate::robots::connector::{EventHub, StreamListener};

/// Removes the listener when the connection goes.
struct Subscription {
    hub: Arc<EventHub>,
    robot: ParticipantId,
    listener: Arc<StreamListener>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.hub.unlisten(&self.robot, &self.listener);
    }
}

pub async fn stream(
    axum::extract::State(state): axum::extract::State<AppState>,
    Extension(context): Extension<RequestContext>,
    Extension(shared): Extension<Arc<Shared>>,
) -> Response {
    let Some(identity) = context.identity else {
        return error(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    if !is_robot(&identity) {
        return error(StatusCode::FORBIDDEN, "The event stream is for robots");
    }
    let robot = identity.participant.clone();
    let listener = shared.hub.listen(&robot);
    let subscription = Subscription {
        hub: Arc::clone(&shared.hub),
        robot,
        listener,
    };
    // Ends when the robot's token is revoked (rotated, robot deleted).
    let revocations = state.auth.revocations();
    let stream = futures_util::stream::unfold(
        (subscription, identity, revocations),
        |(subscription, identity, mut revocations)| async move {
            loop {
                tokio::select! {
                    bundle = subscription.listener.next() => {
                        let event = Event::default().event("bundle").data(bundle);
                        return Some((
                            Ok::<Event, Infallible>(event),
                            (subscription, identity, revocations),
                        ));
                    }
                    revoked = revocations.recv() => match revoked {
                        Ok(revocation) if revocation.applies_to(&identity) => return None,
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(_) => return None,
                    },
                }
            }
        },
    );
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}
