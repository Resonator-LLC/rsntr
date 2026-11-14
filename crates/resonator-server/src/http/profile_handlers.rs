//! The caller's own name and avatar, and avatars by address.
//!
//! - `GET /profile/me`: `{address, name, customName, imageUrl, hasAvatar}`.
//! - `POST /profile/me` with `{"name": "..."}`: sets the name; an empty
//!   name goes back to the one derived from the address.
//! - `PUT /profile/me/avatar` with the image as the body (PNG, JPEG, GIF
//!   or WebP, at most 5 MB): the centre square, at most 256 px, as PNG.
//! - `DELETE /profile/me/avatar`: back to the initials.
//! - `GET /avatar/<address>`: the stored avatar (any signed-in caller),
//!   else a redirect to the initials.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use resonator_model::id::ParticipantId;
use serde::Deserialize;
use serde_json::json;

use super::{AppState, Context, failure, internal_error, json};
use crate::profile::{MAX_AVATAR_UPLOAD, ShownProfile, clean_name, make_avatar};

fn body(profile: &ShownProfile) -> serde_json::Value {
    json!({
        "address": profile.address,
        "name": profile.name,
        "customName": profile.custom_name,
        "imageUrl": profile.image_url,
        "hasAvatar": profile.has_avatar,
    })
}

fn caller(context: &Context, write: bool) -> Result<ParticipantId, Response> {
    match &context.identity {
        Some(identity)
            if (if write {
                identity.can_write()
            } else {
                identity.can_read()
            }) =>
        {
            Ok(identity.participant.clone())
        }
        _ => Err(StatusCode::FORBIDDEN.into_response()),
    }
}

async fn answer(state: &AppState, address: &ParticipantId) -> Response {
    match state.profiles.show(&[address.address().to_owned()]).await {
        Ok(shown) => match shown.first() {
            Some(profile) => json(StatusCode::OK, body(profile)),
            None => internal_error("no profile"),
        },
        Err(error) => internal_error(error),
    }
}

pub(super) async fn get_me(State(state): State<AppState>, context: Context) -> Response {
    match caller(&context, false) {
        Ok(me) => answer(&state, &me).await,
        Err(response) => response,
    }
}

#[derive(Deserialize)]
struct SetName {
    name: String,
}

pub(super) async fn set_name(
    State(state): State<AppState>,
    context: Context,
    request: Bytes,
) -> Response {
    let me = match caller(&context, true) {
        Ok(me) => me,
        Err(response) => return response,
    };
    let request: SetName = match serde_json::from_slice(&request) {
        Ok(request) => request,
        Err(_) => return failure(StatusCode::BAD_REQUEST, "Give {\"name\": \"...\"}."),
    };
    let name = match clean_name(&request.name) {
        Ok(name) => name,
        Err(error) => return failure(StatusCode::BAD_REQUEST, error),
    };
    if let Err(error) = state.profiles.set_own(&me, Some(name), None).await {
        return internal_error(error);
    }
    answer(&state, &me).await
}

pub(super) async fn set_avatar(
    State(state): State<AppState>,
    context: Context,
    image: Bytes,
) -> Response {
    let me = match caller(&context, true) {
        Ok(me) => me,
        Err(response) => return response,
    };
    if image.is_empty() || image.len() > MAX_AVATAR_UPLOAD {
        return failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "A picture of at most 5 MB, please.",
        );
    }
    let limits = state.profiles.limits();
    let png = match tokio::task::spawn_blocking(move || make_avatar(&image, limits)).await {
        Ok(Some(png)) => png,
        Ok(None) => {
            return failure(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "That is not a picture this server can read (PNG, JPEG, GIF or WebP).",
            );
        }
        Err(error) => return internal_error(error),
    };
    if let Err(error) = state.profiles.set_own(&me, None, Some(Some(png))).await {
        return internal_error(error);
    }
    answer(&state, &me).await
}

pub(super) async fn remove_avatar(State(state): State<AppState>, context: Context) -> Response {
    let me = match caller(&context, true) {
        Ok(me) => me,
        Err(response) => return response,
    };
    if let Err(error) = state.profiles.set_own(&me, None, Some(None)).await {
        return internal_error(error);
    }
    answer(&state, &me).await
}

pub(super) async fn avatar(
    State(state): State<AppState>,
    context: Context,
    Path(address): Path<String>,
) -> Response {
    if caller(&context, false).is_err() {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.profiles.store().avatar(&address).await {
        Ok(Some((png, hash))) => {
            let etag = format!(
                "\"{}\"",
                hash.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
            (
                [
                    (header::CONTENT_TYPE, "image/png".to_owned()),
                    // The URL carries the hash, so it never goes stale.
                    (
                        header::CACHE_CONTROL,
                        "private, max-age=31536000, immutable".to_owned(),
                    ),
                    (header::ETAG, etag),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
                ],
                png,
            )
                .into_response()
        }
        Ok(None) => (
            StatusCode::FOUND,
            [(header::LOCATION, format!("/iniavatars/100x100/{address}"))],
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}
