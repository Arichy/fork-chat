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

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub protocol: Protocol,
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

#[derive(Debug, Deserialize)]
pub struct ListSessionsPageQuery {
    pub limit: Option<usize>,
    pub before_at: Option<DateTime<Utc>>,
    pub before_id: Option<Uuid>,
    pub sort: Option<ListSessionsSort>,
    pub filter: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ListSessionsSort {
    UpdatedAt,
    CreatedAt,
}

#[derive(Debug, Serialize)]
pub struct SessionsPageCursor {
    pub before_at: DateTime<Utc>,
    pub before_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct SessionsPageResponse {
    pub sessions: Vec<Session>,
    pub next_cursor: Option<SessionsPageCursor>,
}

pub async fn create_session_handler(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<Json<CreateSessionResponse>, AppError> {
    let session = create_session(&state.db, req.protocol, req.system_prompt.as_deref()).await?;
    Ok(Json(CreateSessionResponse { session }))
}

pub async fn get_session_handler(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<SessionResponse>, AppError> {
    let session = get_session(&state.db, id).await?;
    Ok(Json(SessionResponse { session }))
}

pub async fn list_sessions_handler(
    State(state): State<AppState>,
    Query(query): Query<ListSessionsPageQuery>,
) -> Result<Json<SessionsPageResponse>, AppError> {
    const DEFAULT_LIMIT: usize = 20;
    const MAX_LIMIT: usize = 100;

    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
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
    let title_filter = query
        .filter
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let mut sessions =
        list_sessions(&state.db, limit as i64 + 1, cursor, sort, title_filter).await?;
    let has_more = sessions.len() > limit;
    if has_more {
        sessions.truncate(limit);
    }

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

pub async fn update_session_handler(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateSessionRequest>,
) -> Result<Json<SessionResponse>, AppError> {
    update_session_title(&state.db, id, &req.title).await?;
    let session = get_session(&state.db, id).await?;
    Ok(Json(SessionResponse { session }))
}
