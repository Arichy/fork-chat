use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use tracing::{error, info};
use uuid::Uuid;

use crate::config::{AppState, Protocol};
use crate::db::sessions::update_session_title;
use crate::db::{
    UpdateTurnParams, create_turn, get_path_to_turn_in_session, get_session, get_session_tree,
    get_turn_in_session, session_has_root_turn, update_turn,
};
use crate::error::AppError;
use crate::llm::SendResult;
use crate::models::Turn;

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

/// Validate that (session.protocol, provider, model) form a valid combination
/// according to the current config. Returns the resolved protocol on success.
fn validate_dispatch(
    state: &AppState,
    protocol: Protocol,
    provider_name: &str,
    model: &str,
) -> Result<Protocol, AppError> {
    let provider = state
        .config
        .provider(provider_name)
        .ok_or_else(|| AppError::BadRequest(format!("unknown provider '{provider_name}'")))?;

    if provider.binding(protocol).is_none() {
        return Err(AppError::BadRequest(format!(
            "provider '{}' is not configured for protocol '{}'",
            provider_name,
            protocol.as_str()
        )));
    }

    if !provider.has_model(model) {
        return Err(AppError::BadRequest(format!(
            "model '{}' is not exposed by provider '{}'",
            model, provider_name
        )));
    }

    Ok(protocol)
}

fn user_message_content(protocol: Protocol, user_text: &str) -> JsonValue {
    match protocol {
        Protocol::Openai => json!([{ "role": "user", "content": user_text }]),
        Protocol::Anthropic => json!([{ "type": "text", "text": user_text }]),
    }
}

fn build_turn_messages(protocol: Protocol, user_text: &str, send: &SendResult) -> JsonValue {
    json!([
        {
            "role": "user",
            "content": user_message_content(protocol, user_text),
        },
        {
            "role": "assistant",
            "content": send.assistant_content.clone(),
            "response_id": send.response_id.clone(),
            "stop_reason": send.stop_reason.clone(),
            "usage": send.usage.clone(),
            "raw_response": send.raw_response.clone(),
        }
    ])
}

fn build_failed_turn_messages(protocol: Protocol, user_text: &str) -> JsonValue {
    json!([
        {
            "role": "user",
            "content": user_message_content(protocol, user_text),
        }
    ])
}

pub async fn create_turn_handler(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<CreateTurnRequest>,
) -> Result<Json<CreateTurnResponse>, AppError> {
    info!(
        "Creating turn for session {}, provider: {}, model: {}",
        session_id, req.provider, req.model
    );

    // Check for single root node constraint.
    if req.parent_turn_id.is_none() {
        let has_root = session_has_root_turn(&state.db, session_id).await?;
        if has_root {
            return Err(AppError::BadRequest(
                "Session already has a root turn. Use parent_turn_id to fork from an existing turn.".to_string()
            ));
        }
    }

    // Disallow creating children of failed turns.
    if let Some(parent_id) = req.parent_turn_id {
        let parent = get_turn_in_session(&state.db, session_id, parent_id).await?;
        if parent.status == "failed" {
            return Err(AppError::BadRequest(
                "Cannot reply to a failed turn. Use retry instead.".to_string(),
            ));
        }
    }

    let session = get_session(&state.db, session_id).await?;

    let protocol = validate_dispatch(&state, session.protocol, &req.provider, &req.model)?;
    let adapter = state.registry.get(protocol, &req.provider).ok_or_else(|| {
        AppError::Internal(eyre::eyre!(
            "registry missing adapter for ({}, {})",
            protocol.as_str(),
            req.provider
        ))
    })?;

    let turn = create_turn(
        &state.db,
        session_id,
        req.parent_turn_id,
        "running",
        &req.user_text,
    )
    .await?;

    let history = get_path_to_turn_in_session(&state.db, session_id, req.parent_turn_id).await?;
    let instructions = session.system_prompt.as_deref();

    info!(
        "Calling {} adapter with model {}",
        protocol.as_str(),
        req.model
    );

    let result = adapter
        .send(&history, &req.user_text, &req.model, instructions)
        .await;

    match result {
        Ok(send) => {
            info!(
                "API call successful, response_id: {:?}, tokens: ({:?}, {:?})",
                send.response_id, send.input_tokens, send.output_tokens
            );
            let turn_messages = build_turn_messages(protocol, &req.user_text, &send);

            let turn = update_turn(
                &state.db,
                turn.id,
                UpdateTurnParams {
                    status: "completed",
                    assistant_text: send.assistant_text.as_deref(),
                    turn_messages: &turn_messages,
                    response_id: send.response_id.as_deref(),
                    provider: &req.provider,
                    model: &req.model,
                    input_tokens: send.input_tokens,
                    output_tokens: send.output_tokens,
                    cached_tokens: send.cached_tokens,
                    error: None,
                    retry_turn_id: None,
                },
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
            let turn_messages = build_failed_turn_messages(protocol, &req.user_text);
            let _ = update_turn(
                &state.db,
                turn.id,
                UpdateTurnParams {
                    status: "failed",
                    assistant_text: None,
                    turn_messages: &turn_messages,
                    response_id: None,
                    provider: &req.provider,
                    model: &req.model,
                    input_tokens: None,
                    output_tokens: None,
                    cached_tokens: None,
                    error: Some(&error_json),
                    retry_turn_id: None,
                },
            )
            .await;
            Err(e)
        }
    }
}

pub async fn get_turn_handler(
    State(state): State<AppState>,
    Path((session_id, turn_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<TurnResponse>, AppError> {
    let turn = get_turn_in_session(&state.db, session_id, turn_id).await?;
    Ok(Json(TurnResponse { turn }))
}

pub async fn get_session_tree_handler(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<TreeResponse>, AppError> {
    // Ensure session exists, returns 404 if not.
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

    let session = get_session(&state.db, session_id).await?;
    let old_turn = get_turn_in_session(&state.db, session_id, old_turn_id).await?;

    let protocol = validate_dispatch(&state, session.protocol, &req.provider, &req.model)?;
    let adapter = state.registry.get(protocol, &req.provider).ok_or_else(|| {
        AppError::Internal(eyre::eyre!(
            "registry missing adapter for ({}, {})",
            protocol.as_str(),
            req.provider
        ))
    })?;

    let user_text = old_turn.user_text.clone().unwrap_or_default();

    // Create a new turn as the retry (same parent, same user_text).
    let new_turn = create_turn(
        &state.db,
        session_id,
        old_turn.parent_turn_id,
        "running",
        &user_text,
    )
    .await?;

    let history =
        get_path_to_turn_in_session(&state.db, session_id, old_turn.parent_turn_id).await?;
    let instructions = session.system_prompt.as_deref();

    info!(
        "Calling {} adapter for retry with model {}",
        protocol.as_str(),
        req.model
    );

    let result = adapter
        .send(&history, &user_text, &req.model, instructions)
        .await;

    match result {
        Ok(send) => {
            info!(
                "Retry API call successful, response_id: {:?}",
                send.response_id
            );
            let turn_messages = build_turn_messages(protocol, &user_text, &send);

            let new_turn = update_turn(
                &state.db,
                new_turn.id,
                UpdateTurnParams {
                    status: "completed",
                    assistant_text: send.assistant_text.as_deref(),
                    turn_messages: &turn_messages,
                    response_id: send.response_id.as_deref(),
                    provider: &req.provider,
                    model: &req.model,
                    input_tokens: send.input_tokens,
                    output_tokens: send.output_tokens,
                    cached_tokens: send.cached_tokens,
                    error: None,
                    retry_turn_id: None,
                },
            )
            .await?;

            // Link old failed turn to the new turn.
            update_turn(
                &state.db,
                old_turn_id,
                UpdateTurnParams {
                    status: old_turn.status.as_str(),
                    assistant_text: old_turn.assistant_text.as_deref(),
                    turn_messages: &old_turn.turn_messages,
                    response_id: old_turn.response_id.as_deref(),
                    provider: old_turn.provider.as_deref().unwrap_or(""),
                    model: old_turn.model.as_deref().unwrap_or(""),
                    input_tokens: old_turn.input_tokens,
                    output_tokens: old_turn.output_tokens,
                    cached_tokens: old_turn.cached_tokens,
                    error: old_turn.error.as_ref(),
                    retry_turn_id: Some(new_turn.id),
                },
            )
            .await?;

            Ok(Json(CreateTurnResponse { turn: new_turn }))
        }
        Err(e) => {
            error!("Retry API call failed: {}", e);
            let error_json = serde_json::json!({ "message": e.to_string() });
            let turn_messages = build_failed_turn_messages(protocol, &user_text);
            let new_turn = update_turn(
                &state.db,
                new_turn.id,
                UpdateTurnParams {
                    status: "failed",
                    assistant_text: None,
                    turn_messages: &turn_messages,
                    response_id: None,
                    provider: &req.provider,
                    model: &req.model,
                    input_tokens: None,
                    output_tokens: None,
                    cached_tokens: None,
                    error: Some(&error_json),
                    retry_turn_id: None,
                },
            )
            .await?;

            // Link old failed turn to the new (also failed) turn.
            update_turn(
                &state.db,
                old_turn_id,
                UpdateTurnParams {
                    status: old_turn.status.as_str(),
                    assistant_text: old_turn.assistant_text.as_deref(),
                    turn_messages: &old_turn.turn_messages,
                    response_id: old_turn.response_id.as_deref(),
                    provider: old_turn.provider.as_deref().unwrap_or(""),
                    model: old_turn.model.as_deref().unwrap_or(""),
                    input_tokens: old_turn.input_tokens,
                    output_tokens: old_turn.output_tokens,
                    cached_tokens: old_turn.cached_tokens,
                    error: old_turn.error.as_ref(),
                    retry_turn_id: Some(new_turn.id),
                },
            )
            .await?;

            Err(e)
        }
    }
}
