//! Registration, sign-in, sign-out and access tokens.
//!
//! The first three follow `UserRegistrationServlet`, `AuthenticationServlet`
//! and `SignOutServlet` of Apache Wave (Apache License 2.0), as JSON APIs
//! that also accept form posts: a request that posts a form and accepts
//! HTML (a plain `<form>`) gets a redirect or a small page; every other
//! request gets JSON.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use serde_json::json;

use super::{AppState, Context, RequestContext, failure, internal_error, json, session_cookie};
use crate::auth::{AccessTokenInfo, AuthError, Scopes};
use crate::metrics::Metrics;

const RESPONSE_STATUS_SUCCESS: &str = "SUCCESS";

/// The fields of a credentials request, from a JSON object or a form.
struct Fields {
    values: HashMap<String, String>,
    /// Whether the client posted a form and wants a page back.
    html: bool,
}

impl Fields {
    /// Fails with the message for a 400 response.
    fn parse(headers: &HeaderMap, body: &Bytes) -> Result<Self, &'static str> {
        let content_type = headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let accepts_html = headers
            .get(header::ACCEPT)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|accept| accept.contains("text/html"));
        if content_type.starts_with("application/json") {
            let object: HashMap<String, serde_json::Value> =
                serde_json::from_slice(body).map_err(|_| "Request body is not a JSON object")?;
            let values = object
                .into_iter()
                .filter_map(|(key, value)| match value {
                    serde_json::Value::String(text) => Some((key, text)),
                    _ => None,
                })
                .collect();
            Ok(Fields {
                values,
                html: false,
            })
        } else {
            let values: HashMap<String, String> = serde_urlencoded_from_bytes(body)
                .ok_or("Request body is not a URL-encoded form")?;
            Ok(Fields {
                values,
                html: accepts_html,
            })
        }
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }
}

/// Decodes `application/x-www-form-urlencoded` data.
fn serde_urlencoded_from_bytes(body: &[u8]) -> Option<HashMap<String, String>> {
    let text = std::str::from_utf8(body).ok()?;
    let mut values = HashMap::new();
    for pair in text.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        values.insert(percent_decode(key)?, percent_decode(value)?);
    }
    Some(values)
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = text.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 2;
            }
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A minimal page; the real client replaces these in a later step.
fn page(status: StatusCode, title: &str, body: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/html;charset=utf-8")],
        format!(
            "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>{}</title></head>\
             <body>\n<h1>{}</h1>\n{body}\n</body></html>\n",
            escape_html(title),
            escape_html(title),
        ),
    )
        .into_response()
}

fn credentials_form(action: &str, button: &str, message: &str) -> String {
    let message = if message.is_empty() {
        String::new()
    } else {
        format!("<p role=\"alert\">{}</p>\n", escape_html(message))
    };
    format!(
        "{message}<form method=\"post\" action=\"{}\">\n\
         <p><label>Address <input name=\"address\" autocomplete=\"username\"></label></p>\n\
         <p><label>Password <input name=\"password\" type=\"password\"></label></p>\n\
         <p><button type=\"submit\">{}</button></p>\n</form>",
        escape_html(action),
        escape_html(button),
    )
}

/// A redirect target given by the client, if it stays on this server
/// (`AuthenticationServlet.redirectLoggedInUser` refuses URLs with a host).
fn safe_redirect(target: Option<&str>) -> Option<&str> {
    target.filter(|target| {
        target.starts_with('/')
            && !target.starts_with("//")
            && !target.contains('\\')
            && !target.chars().any(char::is_control)
    })
}

fn redirect_to(target: &str) -> Response {
    Redirect::to(target).into_response()
}

fn form_action(path: &str, redirect: Option<&str>) -> String {
    match redirect {
        Some(target) => format!("{path}?r={}", percent_encode(target)),
        None => path.to_owned(),
    }
}

fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn rate_limited(state: &AppState, error: &AuthError, retry_after_secs: u64) -> Response {
    Metrics::inc(&state.metrics.rate_limited_total);
    let mut response = failure(StatusCode::TOO_MANY_REQUESTS, error);
    if let Ok(value) = HeaderValue::from_str(&retry_after_secs.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

type Params = Query<HashMap<String, String>>;

// ---- Sign-in ----

/// `GET /auth/signin`: redirects a signed-in user, shows a form otherwise.
pub(super) async fn signin_page(context: Context, Query(params): Params) -> Response {
    let redirect = safe_redirect(params.get("r").map(String::as_str));
    if context.identity.is_some() {
        return redirect_to(redirect.unwrap_or("/"));
    }
    page(
        StatusCode::OK,
        "Sign in",
        &credentials_form(&form_action("/auth/signin", redirect), "Sign in", ""),
    )
}

/// `POST /auth/signin` with `address` and `password`.
pub(super) async fn signin(
    State(state): State<AppState>,
    context: Context,
    Query(params): Params,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let fields = match Fields::parse(&headers, &body) {
        Ok(fields) => fields,
        Err(message) => return failure(StatusCode::BAD_REQUEST, message),
    };
    let redirect = safe_redirect(params.get("r").map(String::as_str));
    let address = fields.get("address").unwrap_or("");
    let password = fields.get("password").unwrap_or("");
    match state.auth.sign_in(address, password, context.client).await {
        Ok(grant) => {
            Metrics::inc(&state.metrics.sign_ins_total);
            let cookie = session_cookie(
                &grant.session_id,
                grant.max_age_secs,
                state.cookie_secure(&context),
            );
            let mut response = if fields.html {
                redirect_to(redirect.unwrap_or("/"))
            } else {
                json(
                    StatusCode::OK,
                    json!({
                        "status": RESPONSE_STATUS_SUCCESS,
                        "address": grant.identity.participant.address(),
                        "domain": state.config.domain,
                    }),
                )
            };
            response.headers_mut().insert(header::SET_COOKIE, cookie);
            response
        }
        Err(error @ AuthError::RateLimited { retry_after_secs }) => {
            rate_limited(&state, &error, retry_after_secs)
        }
        Err(error @ AuthError::InvalidCredentials) => {
            Metrics::inc(&state.metrics.sign_in_failures_total);
            tracing::info!(client = %context.client, "User authentication failed");
            if fields.html {
                page(
                    StatusCode::FORBIDDEN,
                    "Sign in",
                    &credentials_form(
                        &form_action("/auth/signin", redirect),
                        "Sign in",
                        &error.to_string(),
                    ),
                )
            } else {
                failure(StatusCode::FORBIDDEN, error)
            }
        }
        Err(error) => internal_error(error),
    }
}

// ---- Sign-out ----

/// `GET /auth/signout`: a page with the button that posts. (The original
/// signs out on GET, which any page can trigger.)
pub(super) async fn signout_page(Query(params): Params) -> Response {
    let redirect = safe_redirect(params.get("r").map(String::as_str));
    page(
        StatusCode::OK,
        "Sign out",
        &format!(
            "<form method=\"post\" action=\"{}\"><p><button type=\"submit\">Sign out</button></p></form>",
            escape_html(&form_action("/auth/signout", redirect))
        ),
    )
}

/// `POST /auth/signout`: ends the session and every connection using it.
pub(super) async fn signout(
    State(state): State<AppState>,
    context: Context,
    Query(params): Params,
    headers: HeaderMap,
) -> Response {
    if let Some(identity) = &context.identity
        && let Err(error) = state.auth.sign_out(identity).await
    {
        return internal_error(error);
    }
    let html = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"));
    let mut response = if html {
        match safe_redirect(params.get("r").map(String::as_str)) {
            Some(target) => redirect_to(target),
            None => page(StatusCode::OK, "Signed out", "<p>Logged out.</p>"),
        }
    } else {
        json(StatusCode::OK, json!({"status": RESPONSE_STATUS_SUCCESS}))
    };
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie("", 0, state.cookie_secure(&context)),
    );
    response
}

// ---- Registration ----

/// `GET /auth/register`: the registration form.
pub(super) async fn register_page(State(state): State<AppState>) -> Response {
    if !state.config.registration_open() {
        return page(
            StatusCode::OK,
            "Register",
            &format!(
                "<p>{}</p>",
                escape_html(&AuthError::RegistrationDisabled.to_string())
            ),
        );
    }
    page(
        StatusCode::OK,
        "Register",
        &credentials_form("/auth/register", "Register", ""),
    )
}

/// `POST /auth/register` with `address` and `password`.
pub(super) async fn register(
    State(state): State<AppState>,
    context: Context,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let fields = match Fields::parse(&headers, &body) {
        Ok(fields) => fields,
        Err(message) => return failure(StatusCode::BAD_REQUEST, message),
    };
    let result = state
        .auth
        .register(
            fields.get("address"),
            fields.get("password").unwrap_or(""),
            context.client,
            context.identity.as_ref(),
        )
        .await;
    let (status, message, address) = match &result {
        Ok(id) => (
            StatusCode::OK,
            "Registration complete.".to_owned(),
            Some(id.address().to_owned()),
        ),
        Err(error @ AuthError::RateLimited { retry_after_secs }) => {
            return rate_limited(&state, error, *retry_after_secs);
        }
        Err(error @ (AuthError::Storage(_) | AuthError::Internal(_))) => {
            tracing::error!(error = ?error, "registration failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                AuthError::Internal(String::new()).to_string(),
                None,
            )
        }
        // The original answers every refused registration with 403.
        Err(error) => (StatusCode::FORBIDDEN, error.to_string(), None),
    };
    if fields.html {
        let form = if result.is_ok() {
            "<p><a href=\"/auth/signin\">Sign in</a></p>".to_owned()
        } else {
            credentials_form("/auth/register", "Register", "")
        };
        return page(
            status,
            "Register",
            &format!("<p role=\"alert\">{}</p>\n{form}", escape_html(&message)),
        );
    }
    match address {
        Some(address) => json(
            status,
            json!({"status": RESPONSE_STATUS_SUCCESS, "message": message, "address": address}),
        ),
        None => failure(status, message),
    }
}

// ---- Personal access tokens ----

fn token_json(info: &AccessTokenInfo) -> serde_json::Value {
    json!({
        "id": info.id,
        "name": info.name,
        "scopes": info.scopes.names(),
        "created_at": info.created_at,
        "expires_at": info.expires_at,
        "last_used_at": info.last_used_at,
    })
}

/// The signed-in identity that may manage tokens, or the status and
/// message of the refusal.
fn token_manager(
    context: &RequestContext,
) -> Result<&crate::auth::Identity, (StatusCode, &'static str)> {
    match &context.identity {
        None => Err((StatusCode::UNAUTHORIZED, "Not logged in")),
        Some(identity) if !identity.is_session() => Err((
            StatusCode::FORBIDDEN,
            "Access tokens can only be managed from a signed-in session",
        )),
        Some(identity) => Ok(identity),
    }
}

/// `GET /auth/tokens`: the caller's tokens (never the token values).
pub(super) async fn list_tokens(State(state): State<AppState>, context: Context) -> Response {
    let identity = match token_manager(&context) {
        Ok(identity) => identity,
        Err((status, message)) => return failure(status, message),
    };
    match state.auth.list_tokens(&identity.participant).await {
        Ok(tokens) => json(
            StatusCode::OK,
            json!({"tokens": tokens.iter().map(token_json).collect::<Vec<_>>()}),
        ),
        Err(error) => internal_error(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateToken {
    name: String,
    /// Defaults to read and write.
    scopes: Option<Vec<String>>,
    expires_in_secs: Option<u64>,
}

/// `POST /auth/tokens` with `{"name", "scopes"?, "expires_in_secs"?}`: the
/// only response that ever contains the token.
pub(super) async fn create_token(
    State(state): State<AppState>,
    context: Context,
    body: Bytes,
) -> Response {
    let identity = match token_manager(&context) {
        Ok(identity) => identity,
        Err((status, message)) => return failure(status, message),
    };
    let request: CreateToken = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            return failure(
                StatusCode::BAD_REQUEST,
                format!("Invalid token request: {error}"),
            );
        }
    };
    let scopes = match &request.scopes {
        None => Ok(Scopes::ALL),
        Some(names) => Scopes::from_names(names.iter().map(String::as_str)),
    };
    let scopes = match scopes {
        Ok(scopes) => scopes,
        Err(message) => return failure(StatusCode::BAD_REQUEST, message),
    };
    match state
        .auth
        .create_token(identity, &request.name, scopes, request.expires_in_secs)
        .await
    {
        Ok((token, info)) => {
            let mut body = token_json(&info);
            body["token"] = json!(token);
            json(StatusCode::CREATED, body)
        }
        Err(error @ AuthError::Invalid(_)) => failure(StatusCode::BAD_REQUEST, error),
        Err(error @ AuthError::Forbidden(_)) => failure(StatusCode::FORBIDDEN, error),
        Err(error) => internal_error(error),
    }
}

/// `DELETE /auth/tokens/{id}`.
pub(super) async fn revoke_token(
    State(state): State<AppState>,
    context: Context,
    Path(id): Path<String>,
) -> Response {
    let identity = match token_manager(&context) {
        Ok(identity) => identity,
        Err((status, message)) => return failure(status, message),
    };
    let Ok(id) = id.parse::<i64>() else {
        return failure(StatusCode::NOT_FOUND, "No such token");
    };
    match state.auth.revoke_token(&identity.participant, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => failure(StatusCode::NOT_FOUND, "No such token"),
        Err(error) => internal_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms_are_decoded() {
        let values =
            serde_urlencoded_from_bytes(b"address=Frodo+B%40example.com&password=a%26b&x").unwrap();
        assert_eq!(values["address"], "Frodo B@example.com");
        assert_eq!(values["password"], "a&b");
        assert_eq!(values["x"], "");
        assert!(serde_urlencoded_from_bytes(b"a=%zz").is_none());
        assert!(serde_urlencoded_from_bytes(b"a=%4").is_none());
        assert!(serde_urlencoded_from_bytes(b"a=%ff").is_none());
        assert!(serde_urlencoded_from_bytes(&[0xff]).is_none());
        assert!(serde_urlencoded_from_bytes(b"").unwrap().is_empty());
    }

    #[test]
    fn only_local_redirects_are_followed() {
        assert_eq!(safe_redirect(Some("/")), Some("/"));
        assert_eq!(safe_redirect(Some("/#w+abc")), Some("/#w+abc"));
        assert_eq!(safe_redirect(Some("//evil.example")), None);
        assert_eq!(safe_redirect(Some("https://evil.example/")), None);
        assert_eq!(safe_redirect(Some("/\\evil.example")), None);
        assert_eq!(safe_redirect(Some("/a\nb")), None);
        assert_eq!(safe_redirect(Some("relative")), None);
        assert_eq!(safe_redirect(None), None);
    }

    #[test]
    fn html_is_escaped() {
        assert_eq!(
            escape_html("<a href=\"x\">&</a>"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;"
        );
        assert_eq!(
            form_action("/auth/signin", Some("/#a b")),
            "/auth/signin?r=/%23a%20b"
        );
    }
}
