use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use uuid::Uuid;

use crate::config::AppState;
use crate::db::{create_turn, get_session, get_session_tree, get_turn, session_has_root_turn, update_turn};
use crate::db::sessions::update_session_title;
use crate::error::AppError;
use crate::models::Turn;
use crate::openai::{OpenaiAdapter, build_input_for_turn, get_instructions};

#[derive(Debug, Deserialize)]
pub struct CreateTurnRequest {
    pub parent_turn_id: Option<Uuid>,
    pub user_text: String,
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Serialize)]
pub struct CreateTurnResponse {
    pub turn: Turn,
}

#[derive(Debug, Serialize)]
pub struct TurnResponse {
    pub turn: Turn,
}

#[derive(Debug, Serialize)]
pub struct TreeResponse {
    pub turns: Vec<Turn>,
}

pub async fn create_turn_handler(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<CreateTurnRequest>,
) -> Result<Json<CreateTurnResponse>, AppError> {
    info!(
        "Creating turn for session {}, model: {}",
        session_id, req.model
    );

    if req.provider != "openai" {
        return Err(AppError::UnsupportedProvider(req.provider));
    }

    // Check for single root node constraint
    if req.parent_turn_id.is_none() {
        let has_root = session_has_root_turn(&state.db, session_id).await?;
        if has_root {
            return Err(AppError::BadRequest(
                "Session already has a root turn. Use parent_turn_id to fork from an existing turn.".to_string()
            ));
        }
    }

    // Disallow creating children of failed turns
    if let Some(parent_id) = req.parent_turn_id {
        let parent = get_turn(&state.db, parent_id).await?;
        if parent.status == "failed" {
            return Err(AppError::BadRequest(
                "Cannot reply to a failed turn. Use retry instead.".to_string()
            ));
        }
    }

    let session = get_session(&state.db, session_id).await?;

    let turn = create_turn(
        &state.db,
        session_id,
        req.parent_turn_id,
        "running",
        &req.user_text,
    )
    .await?;

    let input = build_input_for_turn(&state.db, &session, req.parent_turn_id, &req.user_text)
        .await?;

    let instructions = get_instructions(&session);

    info!("Calling Responses API with model {}", req.model);

    let adapter = OpenaiAdapter::new(state.openai_client.clone());
    let result = adapter.send(input, &req.model, instructions).await;

    match result {
        Ok(response) => {
            info!(
                "API call successful, response_id: {}, tokens: {:?}",
                response.id, response.usage
            );

            let assistant_text = OpenaiAdapter::extract_assistant_text(&response);
            let (input_tokens, output_tokens) = OpenaiAdapter::extract_usage(&response);
            let raw_items = OpenaiAdapter::serialize_output(&response.output)?;
            let response_id = Some(response.id.as_str());

            let turn = update_turn(
                &state.db,
                turn.id,
                "completed",
                assistant_text.as_deref(),
                &raw_items,
                response_id,
                &req.provider,
                &req.model,
                input_tokens,
                output_tokens,
                None,
                None,
                None,
            )
            .await?;

            if session.title.is_none() {
                let title = if req.user_text.len() > 50 {
                    req.user_text[..50].to_string()
                } else {
                    req.user_text.clone()
                };
                update_session_title(&state.db, session_id, &title).await?;
            }

            Ok(Json(CreateTurnResponse { turn }))
        }
        Err(e) => {
            error!("API call failed: {}", e);
            let error_json = serde_json::json!({ "message": e.to_string() });
            let _ = update_turn(
                &state.db,
                turn.id,
                "failed",
                None,
                &serde_json::json!([]),
                None,
                &req.provider,
                &req.model,
                None,
                None,
                None,
                Some(&error_json),
                None,
            )
            .await;
            Err(e)
        }
    }
}

pub async fn get_turn_handler(
    State(state): State<AppState>,
    Path((_session_id, turn_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<TurnResponse>, AppError> {
    let turn = get_turn(&state.db, turn_id).await?;
    Ok(Json(TurnResponse { turn }))
}

pub async fn get_session_tree_handler(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<TreeResponse>, AppError> {
    // Ensure session exists, returns 404 if not
    get_session(&state.db, session_id).await?;
    let turns = get_session_tree(&state.db, session_id).await?;
    Ok(Json(TreeResponse { turns }))
}

#[derive(Debug, Deserialize)]
pub struct RetryTurnRequest {
    pub provider: String,
    pub model: String,
}

pub async fn retry_turn_handler(
    State(state): State<AppState>,
    Path((session_id, old_turn_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<RetryTurnRequest>,
) -> Result<Json<CreateTurnResponse>, AppError> {
    info!("Retrying turn {} in session {}", old_turn_id, session_id);

    if req.provider != "openai" {
        return Err(AppError::UnsupportedProvider(req.provider));
    }

    let old_turn = get_turn(&state.db, old_turn_id).await?;
    let session = get_session(&state.db, session_id).await?;

    let user_text = old_turn.user_text.clone().unwrap_or_default();

    // Create a new turn as the retry (same parent, same user_text)
    let new_turn = create_turn(
        &state.db,
        session_id,
        old_turn.parent_turn_id,
        "running",
        &user_text,
    )
    .await?;

    let input = build_input_for_turn(
        &state.db,
        &session,
        old_turn.parent_turn_id,
        &user_text,
    )
    .await?;

    let instructions = get_instructions(&session);

    info!("Calling Responses API for retry with model {}", req.model);

    let adapter = OpenaiAdapter::new(state.openai_client.clone());
    let result = adapter.send(input, &req.model, instructions).await;

    match result {
        Ok(response) => {
            info!(
                "Retry API call successful, response_id: {}",
                response.id
            );

            let assistant_text = OpenaiAdapter::extract_assistant_text(&response);
            let (input_tokens, output_tokens) = OpenaiAdapter::extract_usage(&response);
            let raw_items = OpenaiAdapter::serialize_output(&response.output)?;
            let response_id = Some(response.id.as_str());

            let new_turn = update_turn(
                &state.db,
                new_turn.id,
                "completed",
                assistant_text.as_deref(),
                &raw_items,
                response_id,
                &req.provider,
                &req.model,
                input_tokens,
                output_tokens,
                None,
                None,
                None,
            )
            .await?;

            // Link old failed turn to the new turn
            update_turn(
                &state.db,
                old_turn_id,
                old_turn.status.as_str(),
                old_turn.assistant_text.as_deref(),
                &old_turn.raw_items,
                old_turn.response_id.as_deref(),
                old_turn.provider.as_deref().unwrap_or(""),
                old_turn.model.as_deref().unwrap_or(""),
                old_turn.input_tokens,
                old_turn.output_tokens,
                old_turn.cached_tokens,
                old_turn.error.as_ref(),
                Some(new_turn.id),
            )
            .await?;

            Ok(Json(CreateTurnResponse { turn: new_turn }))
        }
        Err(e) => {
            error!("Retry API call failed: {}", e);
            let error_json = serde_json::json!({ "message": e.to_string() });
            let new_turn = update_turn(
                &state.db,
                new_turn.id,
                "failed",
                None,
                &serde_json::json!([]),
                None,
                &req.provider,
                &req.model,
                None,
                None,
                None,
                Some(&error_json),
                None,
            )
            .await?;

            // Link old failed turn to the new (also failed) turn
            update_turn(
                &state.db,
                old_turn_id,
                old_turn.status.as_str(),
                old_turn.assistant_text.as_deref(),
                &old_turn.raw_items,
                old_turn.response_id.as_deref(),
                old_turn.provider.as_deref().unwrap_or(""),
                old_turn.model.as_deref().unwrap_or(""),
                old_turn.input_tokens,
                old_turn.output_tokens,
                old_turn.cached_tokens,
                old_turn.error.as_ref(),
                Some(new_turn.id),
            )
            .await?;

            Err(e)
        }
    }
}
