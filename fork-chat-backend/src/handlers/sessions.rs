use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::AppState;
use crate::db::{create_session, delete_session, get_session, list_sessions};
use crate::db::sessions::update_session_title;
use crate::error::AppError;
use crate::models::Session;

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
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

pub async fn create_session_handler(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<Json<CreateSessionResponse>, AppError> {
    let session = create_session(&state.db, req.system_prompt.as_deref()).await?;
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
) -> Result<Json<Vec<Session>>, AppError> {
    let sessions = list_sessions(&state.db).await?;
    Ok(Json(sessions))
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
