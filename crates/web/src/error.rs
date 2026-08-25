//! Failure shapes shared by the HTTP handlers and the CLI reuse surface,
//! plus the envelope-code-to-status mapping of docs/web-api.md section 10.

use axum::http::StatusCode;

/// Terminal failure of one translated request. Mapped to an HTTP status
/// and JSON body by the web handlers, and to an exit code by the CLI's
/// csv subcommands.
#[derive(Debug, thiserror::Error)]
pub enum ApiFailure {
    /// The serving side's authenticator or policy said no (HTTP 403,
    /// CLI exit 2).
    #[error("denied: {}", reason.as_deref().unwrap_or("(no reason given)"))]
    Denied { reason: Option<String> },
    /// An error outcome; `code` is a wire error code from
    /// docs/rdf-envelope-protocol.md, or one of the HTTP-native codes
    /// `unauthorized` / `not-found` / `conflict` / `bad-request`.
    #[error("[{code}] {reason}")]
    Error { code: String, reason: String },
}

impl ApiFailure {
    pub fn error(code: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Error {
            code: code.into(),
            reason: reason.into(),
        }
    }

    /// An internal failure surfaced as `engine-error`.
    pub fn internal(reason: impl std::fmt::Display) -> Self {
        Self::error("engine-error", reason.to_string())
    }

    pub fn not_found(reason: impl Into<String>) -> Self {
        Self::error("not-found", reason)
    }

    pub fn bad_request(reason: impl Into<String>) -> Self {
        Self::error("bad-request", reason)
    }

    /// The mapped HTTP status (web-api.md section 10).
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Denied { .. } => StatusCode::FORBIDDEN,
            Self::Error { code, .. } => error_status(code),
        }
    }
}

/// Status for one error code: the envelope table of web-api.md section
/// 10 plus the HTTP-native codes this crate mints itself.
pub fn error_status(code: &str) -> StatusCode {
    match code {
        "auth-denied" => StatusCode::FORBIDDEN,
        "timeout" => StatusCode::GATEWAY_TIMEOUT,
        "limit-exceeded" => StatusCode::UNPROCESSABLE_ENTITY,
        "mod-unsupported" => StatusCode::NOT_IMPLEMENTED,
        "engine-error" | "protocol-error" => StatusCode::BAD_REQUEST,
        "point-unknown" => StatusCode::NOT_FOUND,
        "unauthorized" => StatusCode::UNAUTHORIZED,
        "not-found" => StatusCode::NOT_FOUND,
        "conflict" => StatusCode::CONFLICT,
        "bad-request" => StatusCode::BAD_REQUEST,
        _ => StatusCode::BAD_REQUEST,
    }
}
