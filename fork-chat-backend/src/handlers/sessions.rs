//! Session CRUD handlers.
//!
//! Sessions are the top-level container for conversations. Each session has:
//! - A `protocol` (openai or anthropic) locked at creation time
//! - An optional `system_prompt`
//! - A tree of turns (managed via the turns handlers)
//!
//! The list endpoint supports cursor-based pagination so the frontend can
//! efficiently load large session lists without offset-based queries (which
//! degrade on large tables).

use axum::{
    Json,
    extract::{Path, Query, State},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::{AppState, Protocol};
use crate::db::sessions::update_session_title;
use crate::db::{SessionSort, create_session, delete_session, get_session, list_sessions};
use crate::error::AppError;
use crate::models::Session;

/// Request body for `POST /api/sessions`.
#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    /// Wire protocol for the session.  Locked for the session's entire
    /// lifetime — all turns in the session will use this protocol.
    pub protocol: Protocol,
    /// Optional system prompt prepended to every turn's context.
    pub system_prompt: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateSessionResponse {
    pub session: Session,
}

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub session: Session,
}

/// Query parameters for `GET /api/sessions` (list sessions).
///
/// Pagination is cursor-based: the client passes `before_at` + `before_id`
/// from the previous page's `next_cursor` to fetch the next page.
#[derive(Debug, Deserialize)]
pub struct ListSessionsPageQuery {
    /// Maximum number of sessions to return. Clamped to [1, 100], defaults to 20.
    pub limit: Option<usize>,
    /// Cursor: only return sessions created/updated before this timestamp.
    pub before_at: Option<DateTime<Utc>>,
    /// Cursor: disambiguates sessions with the same timestamp.
    pub before_id: Option<Uuid>,
    /// Sort field: `updated_at` (default) or `created_at`.
    pub sort: Option<ListSessionsSort>,
    /// Optional title filter for search.  Empty/whitespace strings are ignored.
    pub filter: Option<String>,
}

/// Sort field for session listing.
#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ListSessionsSort {
    UpdatedAt,
    CreatedAt,
}

/// Cursor for the next page of results.
///
/// Uses a composite (timestamp, id) cursor instead of simple offset-based
/// pagination.  This avoids the "skipped rows" problem when new sessions are
/// created between paginated requests, and performs well on large tables
/// because it can use a compound index on `(sort_column, id)`.
#[derive(Debug, Serialize)]
pub struct SessionsPageCursor {
    pub before_at: DateTime<Utc>,
    pub before_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct SessionsPageResponse {
    pub sessions: Vec<Session>,
    /// `None` if there are no more results beyond this page.
    pub next_cursor: Option<SessionsPageCursor>,
}

/// `POST /api/sessions` — create a new session.
///
/// The `protocol` is locked at creation time and cannot be changed later.
/// All turns created within this session will use the same wire protocol.
pub async fn create_session_handler(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<Json<CreateSessionResponse>, AppError> {
    let session = create_session(&state.db, req.protocol, req.system_prompt.as_deref()).await?;
    Ok(Json(CreateSessionResponse { session }))
}

/// `GET /api/sessions/{id}` — fetch a single session by id.
///
/// Returns 404 via the `From<sqlx::Error>` conversion if the session does not
/// exist.
pub async fn get_session_handler(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<SessionResponse>, AppError> {
    let session = get_session(&state.db, id).await?;
    Ok(Json(SessionResponse { session }))
}

/// `GET /api/sessions` — list sessions with cursor-based pagination.
///
/// # Pagination strategy: fetch limit+1
///
/// We request `limit + 1` rows from the database. If we get back more than
/// `limit` rows, we know there's a next page. We truncate the extra row before
/// returning the response and use it to compute the `next_cursor`. This avoids
/// a separate `COUNT(*)` query and works correctly even when rows are inserted
/// or deleted between pages (unlike offset-based pagination).
pub async fn list_sessions_handler(
    State(state): State<AppState>,
    Query(query): Query<ListSessionsPageQuery>,
) -> Result<Json<SessionsPageResponse>, AppError> {
    const DEFAULT_LIMIT: usize = 20;
    const MAX_LIMIT: usize = 100;

    // Clamp the requested limit to a safe range.
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    // The cursor must be a (timestamp, id) pair — providing one without the
    // other is a client error.
    let cursor = match (query.before_at, query.before_id) {
        (Some(before_at), Some(id)) => Some((before_at, id)),
        (None, None) => None,
        _ => {
            return Err(AppError::BadRequest(
                "before_at and before_id must be provided together".to_string(),
            ));
        }
    };
    let sort = match query.sort.unwrap_or(ListSessionsSort::UpdatedAt) {
        ListSessionsSort::UpdatedAt => SessionSort::UpdatedAt,
        ListSessionsSort::CreatedAt => SessionSort::CreatedAt,
    };
    // Trim and ignore empty filter strings so the client can send
    // `?filter=` without it matching every session.
    let title_filter = query
        .filter
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    // Fetch limit + 1 rows to detect whether a next page exists.
    let mut sessions =
        list_sessions(&state.db, limit as i64 + 1, cursor, sort, title_filter).await?;
    // If we got more than `limit` rows, there's a next page. Truncate the
    // extra row before returning it to the client.
    let has_more = sessions.len() > limit;
    if has_more {
        sessions.truncate(limit);
    }

    // Build the next cursor from the last session in the truncated list.
    // The cursor uses the same sort column that was used for the query so
    // the next page picks up exactly where this one left off.
    let next_cursor = if has_more {
        sessions.last().map(|session| SessionsPageCursor {
            before_at: match sort {
                SessionSort::UpdatedAt => session.updated_at,
                SessionSort::CreatedAt => session.created_at,
            },
            before_id: session.id,
        })
    } else {
        None
    };

    Ok(Json(SessionsPageResponse {
        sessions,
        next_cursor,
    }))
}

/// `DELETE /api/sessions/{id}` — delete a session and all its turns.
///
/// Returns `{ "deleted": true }` on success.  Returns 404 if the session does
/// not exist (via the sqlx RowNotFound -> AppError::NotFound conversion).
pub async fn delete_session_handler(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    delete_session(&state.db, id).await?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[derive(Debug, Deserialize)]
pub struct UpdateSessionRequest {
    pub title: String,
}

/// `PATCH /api/sessions/{id}` — update a session's title.
///
/// After updating the title in the database, we re-fetch the session to return
/// the fully up-to-date object (with `updated_at` bumped by the database).
pub async fn update_session_handler(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateSessionRequest>,
) -> Result<Json<SessionResponse>, AppError> {
    update_session_title(&state.db, id, &req.title).await?;
    // Re-fetch to get the updated `updated_at` timestamp.
    let session = get_session(&state.db, id).await?;
    Ok(Json(SessionResponse { session }))
}
