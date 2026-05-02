use std::collections::HashMap;
use std::collections::HashSet;

use serde_json::{Value as JsonValue, json};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use uuid::Uuid;

use crate::config::{AppState, Protocol};
use crate::db::sessions::{update_session_preferences, update_session_title};
use crate::db::{
    UpdateTurnParams, create_turn, get_path_to_turn_in_session, get_session, get_turn_in_session,
    session_has_root_turn, touch_session_updated_at, update_turn, update_turn_if_active,
};
use crate::error::AppError;
use crate::models::Turn;
use crate::tooling::{
    NormalizedToolCall, PendingToolCall, ToolExecutionResult, ToolPolicy, default_policy,
    derive_allow_rule, execute_tool_call, extract_tool_calls, match_allow_rule, tool_result_entry,
};
use crate::turn_runtime::{
    RecordedApprovalDecisionKind, TurnRuntimeState, session_preference_key, status as turn_status,
    stream_event,
};
use crate::turn_stream::TurnStreamEvent;

const MAX_ROUNDS: usize = 24;

/// User decision for a pending tool call approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecisionKind {
    /// Execute this call once and keep the default policy for future calls.
    Allow,
    /// Execute this call and persist an allow-rule for future matching calls.
    AllowAlways,
    /// Reject this call and send a synthetic error result back to the model.
    Deny,
}

/// One approval decision submitted by the user.
#[derive(Debug, Clone)]
pub struct ApproveDecision {
    /// The stable identifier of the pending call (`pcall_*`).
    pub pending_call_id: String,
    /// The decision kind selected by the user.
    pub decision: ApprovalDecisionKind,
}

/// Application service that owns turn lifecycle orchestration.
///
/// This service handles turn creation, multi-round model execution, approval
/// pause/resume, cancellation, and all runtime state persistence for the
/// background loop.
#[derive(Clone)]
pub struct TurnLifecycleService {
    state: AppState,
}

impl TurnLifecycleService {
    /// Build a lifecycle service from shared app state.
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// Create a new turn, persist the initial user transcript, and spawn the
    /// background loop.
    pub async fn create_turn(
        &self,
        session_id: Uuid,
        parent_turn_id: Option<Uuid>,
        user_text: &str,
        provider: &str,
        model: &str,
    ) -> Result<Turn, AppError> {
        create_turn_impl(
            &self.state,
            session_id,
            parent_turn_id,
            user_text,
            provider,
            model,
        )
        .await
    }

    /// Retry an existing turn as a sibling branch and spawn a new loop.
    pub async fn retry_turn(
        &self,
        session_id: Uuid,
        old_turn_id: Uuid,
        provider: &str,
        model: &str,
    ) -> Result<Turn, AppError> {
        retry_turn_impl(&self.state, session_id, old_turn_id, provider, model).await
    }

    /// Apply approval decisions and resume the loop when no more pending calls
    /// remain.
    pub async fn approve_turn(
        &self,
        session_id: Uuid,
        turn_id: Uuid,
        decisions: Vec<ApproveDecision>,
    ) -> Result<Turn, AppError> {
        approve_turn_impl(&self.state, session_id, turn_id, decisions).await
    }

    /// Cancel a running or awaiting-approval turn.
    pub async fn cancel_turn(&self, session_id: Uuid, turn_id: Uuid) -> Result<Turn, AppError> {
        cancel_turn_impl(&self.state, session_id, turn_id).await
    }
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

/// Builds the protocol-native user block for initial transcript entries.
fn user_message_content(protocol: Protocol, user_text: &str) -> JsonValue {
    match protocol {
        Protocol::Openai => json!([{ "role": "user", "content": user_text }]),
        Protocol::Anthropic => json!([{ "type": "text", "text": user_text }]),
    }
}

/// Wraps protocol-native user blocks into the common transcript entry shape.
fn build_user_entry(protocol: Protocol, user_text: &str) -> JsonValue {
    json!({
        "role": "user",
        "content": user_message_content(protocol, user_text),
    })
}

/// Builds the persisted assistant transcript entry from one adapter response.
fn build_assistant_entry(send: &crate::llm::SendResult) -> JsonValue {
    json!({
        "role": "assistant",
        "content": send.assistant_content.clone(),
        "response_id": send.response_id.clone(),
        "stop_reason": send.stop_reason.clone(),
        "usage": send.usage.clone(),
        "raw_response": send.raw_response.clone(),
    })
}

/// Creates an automatic session title from the first user prompt.
fn auto_title_from_user_text(user_text: &str) -> String {
    user_text.chars().take(50).collect()
}

/// Converts decision enum to persisted string form.
fn decision_kind(decision: ApprovalDecisionKind) -> RecordedApprovalDecisionKind {
    match decision {
        ApprovalDecisionKind::Allow => RecordedApprovalDecisionKind::Allow,
        ApprovalDecisionKind::AllowAlways => RecordedApprovalDecisionKind::AllowAlways,
        ApprovalDecisionKind::Deny => RecordedApprovalDecisionKind::Deny,
    }
}

/// Returns session-level tool allow-rules from preferences JSON.
fn allow_rules_from_preferences(preferences: &JsonValue) -> Vec<String> {
    preferences
        .get(session_preference_key::TOOL_ALLOW_RULES)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Adds new allow-rules into session preferences, de-duplicating entries.
fn with_added_allow_rules(preferences: &JsonValue, added: &[String]) -> JsonValue {
    let mut out = preferences.clone();
    if !out.is_object() {
        out = json!({});
    }
    let mut rules: Vec<String> = allow_rules_from_preferences(preferences);
    let mut set: HashSet<String> = rules.iter().cloned().collect();
    for rule in added {
        if set.insert(rule.clone()) {
            rules.push(rule.clone());
        }
    }
    out[session_preference_key::TOOL_ALLOW_RULES] = json!(rules);
    out
}

/// Parses persisted transcript entries from a turn row.
fn parse_transcript(turn: &Turn) -> Vec<JsonValue> {
    turn.turn_messages
        .as_array()
        .cloned()
        .unwrap_or_else(Vec::new)
}

/// Publishes one live stream event to SSE subscribers.
async fn publish_stream_event(
    state: &AppState,
    turn_id: Uuid,
    seq: u64,
    event: &str,
    payload: JsonValue,
) {
    state
        .turn_stream_hub
        .publish(
            turn_id,
            TurnStreamEvent {
                seq,
                event: event.to_string(),
                payload,
            },
        )
        .await;
}

/// Reload the current persisted row for a turn.
async fn reload_turn(state: &AppState, session_id: Uuid, turn_id: Uuid) -> Result<Turn, AppError> {
    get_turn_in_session(&state.db, session_id, turn_id).await
}

/// Appends executed tool results into transcript and persists guarded state.
async fn append_tool_results_and_persist(
    state: &AppState,
    turn: &Turn,
    status: &str,
    transcript: &mut Vec<JsonValue>,
    results: &[ToolExecutionResult],
    runtime_state: &TurnRuntimeState,
) -> Result<Option<Turn>, AppError> {
    let session = get_session(&state.db, turn.session_id).await?;
    let protocol = session.protocol;
    let entry = tool_result_entry(protocol, results);
    transcript.push(entry.clone());
    let payload = json!({
        "entry": entry,
    });
    let (next_runtime_state, seq) = runtime_state.bump_stream_seq();
    let Some(updated) = update_turn_if_active(
        &state.db,
        turn.id,
        UpdateTurnParams {
            status,
            assistant_text: turn.assistant_text.as_deref(),
            turn_messages: &json!(transcript),
            runtime_state: Some(&next_runtime_state),
            response_id: turn.response_id.as_deref(),
            provider: turn.provider.as_deref().unwrap_or_default(),
            model: turn.model.as_deref().unwrap_or_default(),
            input_tokens: turn.input_tokens,
            output_tokens: turn.output_tokens,
            cached_tokens: turn.cached_tokens,
            error: turn.error.as_ref(),
            retry_turn_id: turn.retry_turn_id,
        },
    )
    .await?
    else {
        return Ok(None);
    };
    publish_stream_event(
        state,
        turn.id,
        seq,
        stream_event::TOOL_RESULT_APPENDED,
        payload,
    )
    .await;
    Ok(Some(updated))
}

/// Executes model-emitted tool calls in parallel while preserving output order.
async fn execute_tool_calls_parallel(
    calls: Vec<NormalizedToolCall>,
    cancel_token: &CancellationToken,
) -> Vec<ToolExecutionResult> {
    if calls.is_empty() {
        return Vec::new();
    }

    let mut set = JoinSet::new();
    for (idx, call) in calls.into_iter().enumerate() {
        let token = cancel_token.clone();
        set.spawn(async move { (idx, execute_tool_call(&call, &token).await) });
    }

    let mut ordered: Vec<Option<ToolExecutionResult>> = vec![None; set.len()];
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((idx, result)) => {
                ordered[idx] = Some(result);
            }
            Err(err) => {
                error!("tool task join error: {err}");
            }
        }
    }

    ordered.into_iter().flatten().collect()
}

/// Runs the multi-round model loop until completion, approval pause, or error.
async fn continue_turn_loop(
    state: &AppState,
    mut turn: Turn,
    provider: &str,
    model: &str,
    cancel_token: CancellationToken,
) -> Result<Turn, AppError> {
    let session = get_session(&state.db, turn.session_id).await?;
    let protocol = session.protocol;
    let adapter = state.registry.get(protocol, provider).ok_or_else(|| {
        AppError::Internal(eyre::eyre!(
            "registry missing adapter for ({}, {})",
            protocol.as_str(),
            provider
        ))
    })?;

    let instructions = session.system_prompt.as_deref();
    for round_index in 0..MAX_ROUNDS {
        if cancel_token.is_cancelled() {
            return reload_turn(state, turn.session_id, turn.id).await;
        }
        let history =
            get_path_to_turn_in_session(&state.db, turn.session_id, Some(turn.id)).await?;
        let send = tokio::select! {
            _ = cancel_token.cancelled() => return reload_turn(state, turn.session_id, turn.id).await,
            result = adapter.send(&history, None, model, instructions) => result?,
        };

        let mut transcript = parse_transcript(&turn);
        let assistant_entry = build_assistant_entry(&send);
        transcript.push(assistant_entry.clone());
        let round_started_payload = json!({
            "round": round_index + 1,
        });
        let (runtime_state_after_round_start, round_started_seq) =
            turn.runtime_state.bump_stream_seq();
        let assistant_payload = json!({
            "entry": assistant_entry,
            "assistant_text": send.assistant_text,
            "input_tokens": send.input_tokens,
            "output_tokens": send.output_tokens,
            "cached_tokens": send.cached_tokens,
        });
        let (runtime_state_after_assistant_entry, assistant_seq) =
            runtime_state_after_round_start.bump_stream_seq();
        let Some(updated_turn) = update_turn_if_active(
            &state.db,
            turn.id,
            UpdateTurnParams {
                status: turn_status::RUNNING,
                assistant_text: send.assistant_text.as_deref(),
                turn_messages: &json!(transcript),
                runtime_state: Some(&runtime_state_after_assistant_entry.clear_pending()),
                response_id: send.response_id.as_deref(),
                provider,
                model,
                input_tokens: send.input_tokens,
                output_tokens: send.output_tokens,
                cached_tokens: send.cached_tokens,
                error: None,
                retry_turn_id: turn.retry_turn_id,
            },
        )
        .await?
        else {
            return reload_turn(state, turn.session_id, turn.id).await;
        };
        turn = updated_turn;
        publish_stream_event(
            state,
            turn.id,
            round_started_seq,
            stream_event::ROUND_STARTED,
            round_started_payload,
        )
        .await;
        publish_stream_event(
            state,
            turn.id,
            assistant_seq,
            stream_event::ASSISTANT_ENTRY_APPENDED,
            assistant_payload,
        )
        .await;

        let calls = extract_tool_calls(protocol, &send.assistant_content);
        if calls.is_empty() {
            let completed_payload = json!({
                "assistant_text": send.assistant_text,
                "input_tokens": send.input_tokens,
                "output_tokens": send.output_tokens,
                "cached_tokens": send.cached_tokens,
            });
            let (next_runtime_state, seq) = turn.runtime_state.bump_stream_seq();
            let Some(updated_turn) = update_turn_if_active(
                &state.db,
                turn.id,
                UpdateTurnParams {
                    status: turn_status::COMPLETED,
                    assistant_text: send.assistant_text.as_deref(),
                    turn_messages: &turn.turn_messages,
                    runtime_state: Some(&next_runtime_state.clear_pending()),
                    response_id: send.response_id.as_deref(),
                    provider,
                    model,
                    input_tokens: send.input_tokens,
                    output_tokens: send.output_tokens,
                    cached_tokens: send.cached_tokens,
                    error: None,
                    retry_turn_id: turn.retry_turn_id,
                },
            )
            .await?
            else {
                return reload_turn(state, turn.session_id, turn.id).await;
            };
            turn = updated_turn;
            publish_stream_event(
                state,
                turn.id,
                seq,
                stream_event::TURN_COMPLETED,
                completed_payload,
            )
            .await;
            return Ok(turn);
        }

        let allow_rules = allow_rules_from_preferences(&session.preferences);
        let mut auto_results: Vec<ToolExecutionResult> = Vec::new();
        let mut auto_calls: Vec<JsonValue> = Vec::new();
        let mut executable_auto_calls: Vec<NormalizedToolCall> = Vec::new();
        let mut pending: Vec<PendingToolCall> = Vec::new();

        for call in calls {
            let Some(policy) = default_policy(&call.name) else {
                auto_results.push(ToolExecutionResult {
                    call_id: call.call_id.clone(),
                    output: format!("Unknown tool '{}'", call.name),
                    is_error: true,
                    error_kind: Some("unknown_tool".to_string()),
                });
                continue;
            };

            let matched = allow_rules
                .iter()
                .any(|rule| match_allow_rule(rule, &call.name, &call.input));
            if matched || policy == ToolPolicy::Auto {
                auto_calls.push(json!({
                    "call_id": call.call_id.clone(),
                    "name": call.name.clone(),
                    "input": call.input.clone(),
                }));
                executable_auto_calls.push(call);
                continue;
            }

            pending.push(PendingToolCall {
                pending_call_id: format!("pcall_{}", Uuid::new_v4()),
                call_id: call.call_id,
                name: call.name,
                input: call.input,
            });
        }

        let mut transcript = parse_transcript(&turn);
        let mut runtime_state_for_auto = turn.runtime_state.clone();
        if !executable_auto_calls.is_empty() {
            let tool_calls_payload = json!({
                "calls": auto_calls,
            });
            let (next_runtime_state, seq) = runtime_state_for_auto.bump_stream_seq();
            runtime_state_for_auto = next_runtime_state;
            let Some(updated_turn) = update_turn_if_active(
                &state.db,
                turn.id,
                UpdateTurnParams {
                    status: turn_status::RUNNING,
                    assistant_text: turn.assistant_text.as_deref(),
                    turn_messages: &turn.turn_messages,
                    runtime_state: Some(&runtime_state_for_auto.clear_pending()),
                    response_id: turn.response_id.as_deref(),
                    provider,
                    model,
                    input_tokens: turn.input_tokens,
                    output_tokens: turn.output_tokens,
                    cached_tokens: turn.cached_tokens,
                    error: None,
                    retry_turn_id: turn.retry_turn_id,
                },
            )
            .await?
            else {
                return reload_turn(state, turn.session_id, turn.id).await;
            };
            turn = updated_turn;
            runtime_state_for_auto = turn.runtime_state.clone();
            publish_stream_event(
                state,
                turn.id,
                seq,
                stream_event::TOOL_CALLS,
                tool_calls_payload,
            )
            .await;
            auto_results
                .extend(execute_tool_calls_parallel(executable_auto_calls, &cancel_token).await);
        }

        if !auto_results.is_empty() {
            let Some(turn_with_auto) = append_tool_results_and_persist(
                state,
                &turn,
                turn_status::RUNNING,
                &mut transcript,
                &auto_results,
                &runtime_state_for_auto.clear_pending(),
            )
            .await?
            else {
                return reload_turn(state, turn.session_id, turn.id).await;
            };
            turn = turn_with_auto;
        }

        if !pending.is_empty() {
            let approval_payload = json!({
                "pending": pending.clone(),
            });
            let (next_runtime_state, seq) = turn.runtime_state.bump_stream_seq();
            let next_runtime_state = next_runtime_state.with_pending(pending.clone());
            let Some(updated_turn) = update_turn_if_active(
                &state.db,
                turn.id,
                UpdateTurnParams {
                    status: turn_status::AWAITING_APPROVAL,
                    assistant_text: turn.assistant_text.as_deref(),
                    turn_messages: &turn.turn_messages,
                    runtime_state: Some(&next_runtime_state),
                    response_id: turn.response_id.as_deref(),
                    provider,
                    model,
                    input_tokens: turn.input_tokens,
                    output_tokens: turn.output_tokens,
                    cached_tokens: turn.cached_tokens,
                    error: None,
                    retry_turn_id: turn.retry_turn_id,
                },
            )
            .await?
            else {
                return reload_turn(state, turn.session_id, turn.id).await;
            };
            turn = updated_turn;
            publish_stream_event(
                state,
                turn.id,
                seq,
                stream_event::APPROVAL_NEEDED,
                approval_payload,
            )
            .await;
            return Ok(turn);
        }
    }

    let error_json = json!({ "kind": "loop_limit_exceeded", "message": "max_rounds exceeded" });
    let failed_payload = json!({
        "error": error_json.clone(),
    });
    let (next_runtime_state, seq) = turn.runtime_state.bump_stream_seq();
    let Some(turn) = update_turn_if_active(
        &state.db,
        turn.id,
        UpdateTurnParams {
            status: turn_status::FAILED,
            assistant_text: turn.assistant_text.as_deref(),
            turn_messages: &turn.turn_messages,
            runtime_state: Some(&next_runtime_state.clear_pending()),
            response_id: turn.response_id.as_deref(),
            provider,
            model,
            input_tokens: turn.input_tokens,
            output_tokens: turn.output_tokens,
            cached_tokens: turn.cached_tokens,
            error: Some(&error_json),
            retry_turn_id: turn.retry_turn_id,
        },
    )
    .await?
    else {
        return reload_turn(state, turn.session_id, turn.id).await;
    };
    publish_stream_event(
        state,
        turn.id,
        seq,
        stream_event::TURN_FAILED,
        failed_payload,
    )
    .await;
    Ok(turn)
}

/// Register and spawn a background turn loop with cooperative cancellation.
async fn spawn_turn_loop(state: AppState, turn: Turn, provider: String, model: String) {
    let registration = state.turn_task_manager.register(turn.id).await;
    let generation = registration.generation;
    let cancel_token = registration.token.clone();

    tokio::spawn(async move {
        let loop_result = continue_turn_loop(
            &state,
            turn.clone(),
            &provider,
            &model,
            cancel_token.clone(),
        )
        .await;
        state.turn_task_manager.finish(turn.id, generation).await;

        if let Err(e) = loop_result {
            error!("Turn loop failed for {}: {}", turn.id, e);
            let latest = match reload_turn(&state, turn.session_id, turn.id).await {
                Ok(latest) => latest,
                Err(err) => {
                    error!(
                        "failed to reload turn {} after loop error: {}",
                        turn.id, err
                    );
                    return;
                }
            };
            if turn_status::is_terminal(&latest.status) {
                return;
            }
            let error_json = json!({ "kind": "loop_error", "message": e.to_string() });
            let failed_payload = json!({
                "error": error_json.clone(),
            });
            let (next_runtime_state, seq) = latest.runtime_state.bump_stream_seq();
            let Ok(Some(_)) = update_turn_if_active(
                &state.db,
                latest.id,
                UpdateTurnParams {
                    status: turn_status::FAILED,
                    assistant_text: latest.assistant_text.as_deref(),
                    turn_messages: &latest.turn_messages,
                    runtime_state: Some(&next_runtime_state.clear_pending()),
                    response_id: latest.response_id.as_deref(),
                    provider: &provider,
                    model: &model,
                    input_tokens: latest.input_tokens,
                    output_tokens: latest.output_tokens,
                    cached_tokens: latest.cached_tokens,
                    error: Some(&error_json),
                    retry_turn_id: latest.retry_turn_id,
                },
            )
            .await
            else {
                return;
            };
            publish_stream_event(
                &state,
                latest.id,
                seq,
                stream_event::TURN_FAILED,
                failed_payload,
            )
            .await;
        }
    });
}

/// Creates a turn and starts its background loop.
async fn create_turn_impl(
    state: &AppState,
    session_id: Uuid,
    parent_turn_id: Option<Uuid>,
    user_text: &str,
    provider: &str,
    model: &str,
) -> Result<Turn, AppError> {
    info!(
        "Creating turn for session {}, provider: {}, model: {}",
        session_id, provider, model
    );

    if parent_turn_id.is_none() {
        let has_root = session_has_root_turn(&state.db, session_id).await?;
        if has_root {
            return Err(AppError::BadRequest(
                "Session already has a root turn. Use parent_turn_id to fork from an existing turn.".to_string(),
            ));
        }
    }

    if let Some(parent_id) = parent_turn_id {
        let parent = get_turn_in_session(&state.db, session_id, parent_id).await?;
        if parent.status == turn_status::FAILED {
            return Err(AppError::BadRequest(
                "Cannot reply to a failed turn. Use retry instead.".to_string(),
            ));
        }
    }

    let session = get_session(&state.db, session_id).await?;
    let auto_title = session
        .title
        .is_none()
        .then(|| auto_title_from_user_text(user_text));

    let protocol = validate_dispatch(state, session.protocol, provider, model)?;
    let _ = state.registry.get(protocol, provider).ok_or_else(|| {
        AppError::Internal(eyre::eyre!(
            "registry missing adapter for ({}, {})",
            protocol.as_str(),
            provider
        ))
    })?;

    let turn = create_turn(
        &state.db,
        session_id,
        parent_turn_id,
        turn_status::RUNNING,
        user_text,
    )
    .await?;

    let init_messages = json!([build_user_entry(protocol, user_text)]);
    let (next_runtime_state, seq) = turn.runtime_state.bump_stream_seq();
    let turn = update_turn(
        &state.db,
        turn.id,
        UpdateTurnParams {
            status: turn_status::RUNNING,
            assistant_text: None,
            turn_messages: &init_messages,
            runtime_state: Some(&next_runtime_state.clear_pending()),
            response_id: None,
            provider,
            model,
            input_tokens: None,
            output_tokens: None,
            cached_tokens: None,
            error: None,
            retry_turn_id: None,
        },
    )
    .await?;
    publish_stream_event(state, turn.id, seq, stream_event::TURN_STARTED, json!({})).await;

    let _ = touch_session_updated_at(&state.db, session_id).await;
    if let Some(title) = &auto_title {
        update_session_title(&state.db, session_id, title).await?;
    }
    spawn_turn_loop(
        state.clone(),
        turn.clone(),
        provider.to_string(),
        model.to_string(),
    )
    .await;

    Ok(turn)
}

/// Retries an existing turn as a sibling branch and starts a fresh loop.
async fn retry_turn_impl(
    state: &AppState,
    session_id: Uuid,
    old_turn_id: Uuid,
    provider: &str,
    model: &str,
) -> Result<Turn, AppError> {
    let session = get_session(&state.db, session_id).await?;
    let old_turn = get_turn_in_session(&state.db, session_id, old_turn_id).await?;
    let protocol = validate_dispatch(state, session.protocol, provider, model)?;
    let _ = state.registry.get(protocol, provider).ok_or_else(|| {
        AppError::Internal(eyre::eyre!(
            "registry missing adapter for ({}, {})",
            protocol.as_str(),
            provider
        ))
    })?;
    let user_text = old_turn.user_text.clone().unwrap_or_default();

    let new_turn = create_turn(
        &state.db,
        session_id,
        old_turn.parent_turn_id,
        turn_status::RUNNING,
        &user_text,
    )
    .await?;
    let init_messages = json!([build_user_entry(protocol, &user_text)]);
    let (next_runtime_state, seq) = new_turn.runtime_state.bump_stream_seq();
    let new_turn = update_turn(
        &state.db,
        new_turn.id,
        UpdateTurnParams {
            status: turn_status::RUNNING,
            assistant_text: None,
            turn_messages: &init_messages,
            runtime_state: Some(&next_runtime_state.clear_pending()),
            response_id: None,
            provider,
            model,
            input_tokens: None,
            output_tokens: None,
            cached_tokens: None,
            error: None,
            retry_turn_id: None,
        },
    )
    .await?;
    publish_stream_event(
        state,
        new_turn.id,
        seq,
        stream_event::TURN_STARTED,
        json!({}),
    )
    .await;
    let _ = touch_session_updated_at(&state.db, session_id).await;

    update_turn(
        &state.db,
        old_turn_id,
        UpdateTurnParams {
            status: old_turn.status.as_str(),
            assistant_text: old_turn.assistant_text.as_deref(),
            turn_messages: &old_turn.turn_messages,
            runtime_state: Some(&old_turn.runtime_state),
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

    spawn_turn_loop(
        state.clone(),
        new_turn.clone(),
        provider.to_string(),
        model.to_string(),
    )
    .await;

    Ok(new_turn)
}

/// Processes tool approval decisions and resumes loop execution when possible.
async fn approve_turn_impl(
    state: &AppState,
    session_id: Uuid,
    turn_id: Uuid,
    decisions: Vec<ApproveDecision>,
) -> Result<Turn, AppError> {
    let mut turn = get_turn_in_session(&state.db, session_id, turn_id).await?;
    let mut recorded_decisions = turn.runtime_state.approval_decisions.clone();
    if turn.status != turn_status::AWAITING_APPROVAL {
        let all_idempotent = decisions.iter().all(|d| {
            recorded_decisions
                .get(&d.pending_call_id)
                .is_some_and(|v| *v == decision_kind(d.decision))
        });
        if all_idempotent {
            return Ok(turn);
        }
        return Err(AppError::Conflict(format!(
            "turn {} is not awaiting approval",
            turn.id
        )));
    }

    let session = get_session(&state.db, session_id).await?;
    let pending = turn.runtime_state.pending_tool_calls.clone();
    let pending_ids: HashSet<&str> = pending.iter().map(|p| p.pending_call_id.as_str()).collect();
    for decision in &decisions {
        if pending_ids.contains(decision.pending_call_id.as_str()) {
            continue;
        }
        let idempotent = recorded_decisions
            .get(&decision.pending_call_id)
            .is_some_and(|v| *v == decision_kind(decision.decision));
        if !idempotent {
            return Err(AppError::Conflict(format!(
                "pending call id '{}' is not active",
                decision.pending_call_id
            )));
        }
    }

    let mut decisions_map: HashMap<String, ApprovalDecisionKind> = HashMap::new();
    for decision in decisions {
        decisions_map.insert(decision.pending_call_id, decision.decision);
    }

    let mut remaining = Vec::new();
    let mut results = Vec::new();
    let mut add_rules: Vec<String> = Vec::new();
    let mut execute_calls: Vec<NormalizedToolCall> = Vec::new();
    for pending_call in pending {
        let Some(decision) = decisions_map.get(&pending_call.pending_call_id).copied() else {
            remaining.push(pending_call);
            continue;
        };
        recorded_decisions.insert(
            pending_call.pending_call_id.clone(),
            decision_kind(decision),
        );
        match decision {
            ApprovalDecisionKind::Allow => execute_calls.push(NormalizedToolCall {
                call_id: pending_call.call_id.clone(),
                name: pending_call.name.clone(),
                input: pending_call.input.clone(),
            }),
            ApprovalDecisionKind::AllowAlways => {
                add_rules.push(derive_allow_rule(&pending_call.name, &pending_call.input));
                execute_calls.push(NormalizedToolCall {
                    call_id: pending_call.call_id.clone(),
                    name: pending_call.name.clone(),
                    input: pending_call.input.clone(),
                });
            }
            ApprovalDecisionKind::Deny => results.push(ToolExecutionResult {
                call_id: pending_call.call_id.clone(),
                output: "Denied by user".to_string(),
                is_error: true,
                error_kind: Some("denied_by_user".to_string()),
            }),
        }
    }
    if !add_rules.is_empty() {
        let updated_preferences = with_added_allow_rules(&session.preferences, &add_rules);
        update_session_preferences(&state.db, session_id, &updated_preferences).await?;
    }

    let mut runtime_state_with_decisions = turn
        .runtime_state
        .with_pending(remaining.clone())
        .with_approval_decisions(recorded_decisions.clone());
    if !execute_calls.is_empty() {
        let approval_registration = state.turn_task_manager.register(turn.id).await;
        let tool_calls_payload = json!({
            "calls": execute_calls.iter().map(|call| {
                json!({
                    "call_id": call.call_id.clone(),
                    "name": call.name.clone(),
                    "input": call.input.clone(),
                })
            }).collect::<Vec<_>>(),
        });
        let (next_runtime_state, seq) = runtime_state_with_decisions.bump_stream_seq();
        runtime_state_with_decisions = next_runtime_state;
        let Some(updated_turn) = update_turn_if_active(
            &state.db,
            turn.id,
            UpdateTurnParams {
                status: turn_status::AWAITING_APPROVAL,
                assistant_text: turn.assistant_text.as_deref(),
                turn_messages: &turn.turn_messages,
                runtime_state: Some(&runtime_state_with_decisions),
                response_id: turn.response_id.as_deref(),
                provider: turn.provider.as_deref().unwrap_or_default(),
                model: turn.model.as_deref().unwrap_or_default(),
                input_tokens: turn.input_tokens,
                output_tokens: turn.output_tokens,
                cached_tokens: turn.cached_tokens,
                error: turn.error.as_ref(),
                retry_turn_id: turn.retry_turn_id,
            },
        )
        .await?
        else {
            state
                .turn_task_manager
                .finish(turn.id, approval_registration.generation)
                .await;
            return reload_turn(state, session_id, turn.id).await;
        };
        turn = updated_turn;
        runtime_state_with_decisions = turn.runtime_state.clone();
        publish_stream_event(
            state,
            turn.id,
            seq,
            stream_event::TOOL_CALLS,
            tool_calls_payload,
        )
        .await;
        results
            .extend(execute_tool_calls_parallel(execute_calls, &approval_registration.token).await);
        state
            .turn_task_manager
            .finish(turn.id, approval_registration.generation)
            .await;
    }

    if !results.is_empty() {
        let mut transcript = parse_transcript(&turn);
        let Some(updated_turn) = append_tool_results_and_persist(
            state,
            &turn,
            if remaining.is_empty() {
                turn_status::RUNNING
            } else {
                turn_status::AWAITING_APPROVAL
            },
            &mut transcript,
            &results,
            &runtime_state_with_decisions,
        )
        .await?
        else {
            return reload_turn(state, session_id, turn.id).await;
        };
        turn = updated_turn;
    } else {
        let Some(updated_turn) = update_turn_if_active(
            &state.db,
            turn.id,
            UpdateTurnParams {
                status: if remaining.is_empty() {
                    turn_status::RUNNING
                } else {
                    turn_status::AWAITING_APPROVAL
                },
                assistant_text: turn.assistant_text.as_deref(),
                turn_messages: &turn.turn_messages,
                runtime_state: Some(&runtime_state_with_decisions),
                response_id: turn.response_id.as_deref(),
                provider: turn.provider.as_deref().unwrap_or_default(),
                model: turn.model.as_deref().unwrap_or_default(),
                input_tokens: turn.input_tokens,
                output_tokens: turn.output_tokens,
                cached_tokens: turn.cached_tokens,
                error: turn.error.as_ref(),
                retry_turn_id: turn.retry_turn_id,
            },
        )
        .await?
        else {
            return reload_turn(state, session_id, turn.id).await;
        };
        turn = updated_turn;
    }

    if turn.status == turn_status::RUNNING {
        let provider = turn.provider.clone().unwrap_or_default();
        let model = turn.model.clone().unwrap_or_default();
        spawn_turn_loop(state.clone(), turn.clone(), provider, model).await;
    }

    Ok(turn)
}

/// Marks a non-terminal turn as cancelled and emits a terminal failure event.
async fn cancel_turn_impl(
    state: &AppState,
    session_id: Uuid,
    turn_id: Uuid,
) -> Result<Turn, AppError> {
    let turn = get_turn_in_session(&state.db, session_id, turn_id).await?;
    if turn_status::is_terminal(&turn.status) {
        return Ok(turn);
    }
    let _ = state.turn_task_manager.cancel(turn_id).await;
    let error_json = json!({ "kind": "cancelled", "message": "Turn cancelled by user" });
    let failed_payload = json!({
        "error": error_json.clone(),
    });
    let (next_runtime_state, seq) = turn.runtime_state.bump_stream_seq();
    let Some(turn) = update_turn_if_active(
        &state.db,
        turn.id,
        UpdateTurnParams {
            status: turn_status::FAILED,
            assistant_text: turn.assistant_text.as_deref(),
            turn_messages: &turn.turn_messages,
            runtime_state: Some(&next_runtime_state.clear_pending()),
            response_id: turn.response_id.as_deref(),
            provider: turn.provider.as_deref().unwrap_or_default(),
            model: turn.model.as_deref().unwrap_or_default(),
            input_tokens: turn.input_tokens,
            output_tokens: turn.output_tokens,
            cached_tokens: turn.cached_tokens,
            error: Some(&error_json),
            retry_turn_id: turn.retry_turn_id,
        },
    )
    .await?
    else {
        return reload_turn(state, session_id, turn_id).await;
    };
    publish_stream_event(
        state,
        turn.id,
        seq,
        stream_event::TURN_FAILED,
        failed_payload,
    )
    .await;
    Ok(turn)
}
