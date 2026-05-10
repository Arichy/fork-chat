use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::error::Error;
use std::panic::Location;

/// Convenience alias so handlers can return `Result<Json<T>, AppError>` without
/// spelling out the full path every time.
pub type Result<T> = std::result::Result<T, AppError>;

/// Unified error type for all handler return values.
///
/// Each variant maps to a specific HTTP status code in the [`IntoResponse`]
/// implementation below.  Using a single enum rather than generic axum errors
/// ensures consistent JSON error bodies (`{ "error": "..." }`) across all
/// endpoints.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The requested resource (session, turn, etc.) does not exist.
    /// Maps to **404 Not Found**.
    #[error("Resource not found: {0}")]
    NotFound(String),

    /// The request payload or query parameters are semantically invalid
    /// (e.g. missing required fields, bad cursor format).
    /// Maps to **400 Bad Request**.
    #[error("Bad request: {0}")]
    BadRequest(String),

    /// The request conflicts with the current state (e.g. trying to approve
    /// a turn that is already completed, or creating a duplicate).
    /// Maps to **409 Conflict**.
    #[error("Conflict: {0}")]
    Conflict(String),

    /// The upstream LLM API returned an error (rate limit, auth failure,
    /// model overloaded, etc.).  This is not a bug in our code — it's the
    /// provider rejecting the request.
    /// Maps to **502 Bad Gateway** (we acted as a proxy and the upstream
    /// responded with an error).
    #[error("LLM API error: {0}")]
    LlmApiError(#[source] eyre::Report),

    /// An unexpected database error (constraint violation, connection lost,
    /// serialization failure, etc.).  These are internal issues.
    /// Maps to **500 Internal Server Error**.
    #[error("Database error: {0}")]
    DatabaseError(String),

    /// Catch-all for unexpected errors that don't fit the other variants.
    /// The internal message is NOT exposed to clients (to avoid leaking
    /// implementation details); instead a generic "Internal server error" is
    /// returned.
    /// Maps to **500 Internal Server Error**.
    #[error("Internal error: {0}")]
    Internal(#[from] eyre::Report),
}

impl AppError {
    /// Build an LLM API error while recording the call site that created it.
    #[track_caller]
    pub fn llm_api(message: impl Into<String>) -> Self {
        let location = Location::caller();
        let message = format_with_location(message.into(), location);
        AppError::LlmApiError(eyre::eyre!(message))
    }

    /// Build an LLM API error with a source error chain and creation location.
    #[track_caller]
    pub fn llm_api_with_source(
        message: impl Into<String>,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        let location = Location::caller();
        let message = format_with_location(message.into(), location);

        // The outer context carries our adapter/lifecycle location, while the
        // wrapped source keeps provider or parser details available to logs.
        AppError::LlmApiError(eyre::Report::new(source).wrap_err(message))
    }

    /// Return a Display-formatted source chain suitable for JSON diagnostics.
    pub fn diagnostic_chain(&self) -> Vec<String> {
        let mut chain = vec![self.to_string()];
        let mut source = self.source();

        // Walk the standard Error::source chain so failed turns can show which
        // provider/client/parser layer produced the final user-visible error.
        while let Some(error) = source {
            chain.push(error.to_string());
            source = error.source();
        }

        chain
    }
}

fn format_with_location(message: String, location: &'static Location<'static>) -> String {
    format!(
        "{message} (origin: {}:{})",
        location.file(),
        location.line()
    )
}

/// Convert sqlx errors into typed `AppError` variants.
///
/// The key mapping is `RowNotFound` -> `NotFound`: most handlers call
/// `get_session` / `get_turn` which return `RowNotFound` when the id doesn't
/// exist, and we want that to surface as a 404 rather than a generic 500.
/// All other sqlx errors (connection issues, constraint violations, etc.) are
/// treated as internal database errors.
impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        match err {
            // A SELECT ... WHERE id = $1 returned zero rows — the client asked
            // for something that doesn't exist, so 404 is the correct semantic.
            sqlx::Error::RowNotFound => AppError::NotFound("Resource not found".into()),
            // Everything else (connection errors, serialization issues, etc.) is
            // treated as an internal database error that the client cannot fix.
            _ => AppError::DatabaseError(err.to_string()),
        }
    }
}

/// Convert `AppError` into an axum HTTP response.
///
/// Each variant is mapped to its corresponding HTTP status code and the error
/// message is wrapped in a JSON body `{ "error": "<message>" }`.  The
/// `Internal` variant is special-cased to hide the real message from clients
/// (it may contain stack traces or other sensitive info) and instead returns a
/// generic "Internal server error" string.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            // 404: client requested a resource that does not exist.
            AppError::NotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
            // 400: client sent a malformed or invalid request.
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            // 409: request conflicts with current server state.
            AppError::Conflict(_) => (StatusCode::CONFLICT, self.to_string()),
            // 502: we proxied to an upstream LLM API and it returned an error.
            AppError::LlmApiError(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
            // 500: unexpected database error.  The message is included for
            // debuggability since database errors don't typically leak secrets.
            AppError::DatabaseError(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            // 500: catch-all.  The real error message is intentionally hidden
            // from the client to avoid leaking implementation details.
            AppError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal server error".into(),
            ),
        };

        // Always respond with a consistent JSON body so the frontend can
        // parse errors uniformly: `{ "error": "human-readable message" }`.
        (status, Json(json!({ "error": message }))).into_response()
    }
}
