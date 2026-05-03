//! Turn lifecycle orchestration: the core multi-round LLM loop.
//!
//! This module implements the full lifecycle of a conversation turn:
//!
//! ```text
//! create_turn / retry_turn
//!     │
//!     ▼
//! spawn_turn_loop ──────────────────────────────────────────┐
//!     │                                                      │
//!     ▼                                                      │
//! continue_turn_loop (round 0..MAX_ROUNDS)                   │
//!     │                                                      │
//!     ├── Call LLM adapter                                   │
//!     │       │                                              │
//!     │       ▼                                              │
//!     ├── Persist assistant entry + bump stream_seq          │
//!     ├── Publish round_started + assistant_entry_appended   │
//!     │                                                      │
//!     ├── Tool calls?                                        │
//!     │     ├── No: persist COMPLETED, emit turn_completed   │
//!     │     │    return                                      │
//!     │     │                                                │
//!     │     ├── Classify each call (3-layer resolution):     │
//!     │     │     1. Tool existence check (unknown → error)  │
//!     │     │     2. Session allow-rules matching            │
//!     │     │     3. Default tool policy (Auto/RequireApp.)  │
//!     │     │                                                │
//!     │     ├── Execute auto-approved calls in parallel      │
//!     │     │    Persist results, emit tool_result_appended  │
//!     │     │                                                │
//!     │     └── Any pending calls?                           │
//!     │           ├── Yes: persist AWAITING_APPROVAL,        │
//!     │           │    emit approval_needed, return          │
//!     │           │    (loop pauses, waits for POST /approve)│
//!     │           └── No: continue to next round ────────────┘
//!     │
//!     └── MAX_ROUNDS exceeded: persist FAILED, emit turn_failed
//! ```
//!
//! # Key design decisions
//!
//! - **Separate creation and streaming**: `POST /turns` returns immediately
//!   with the turn row. A separate `GET /turns/:id/stream` subscribes to SSE.
//!   This decouples the HTTP request lifecycle from the background loop.
//!
//! - **Approval as a pause/resume mechanism**: When tool calls need approval,
//!   the loop does NOT block. It persists `pending_tool_calls` in
//!   `runtime_state`, publishes an `APPROVAL_NEEDED` event, and returns.
//!   A later `POST /approve` call reloads the turn, processes decisions, and
//!   potentially respawns the loop.
//!
//! - **CAS-based concurrency control**: `update_turn_if_active` acts as a
//!   compare-and-swap guard. If the turn has already reached a terminal state
//!   (e.g. cancelled by another request while the loop was running), the
//!   update is rejected. This prevents stale writes from overwriting the
//!   true terminal state.
//!
//! - **Write-before-publish invariant**: Every SSE event is published AFTER
//!   the corresponding DB write (with bumped `stream_seq`) completes. This
//!   guarantees that any event a subscriber receives refers to state that is
//!   already durable.

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

/// Maximum number of LLM call rounds per turn.
///
/// This is a safety guard against infinite loops caused by the model
/// repeatedly making tool calls that produce more tool calls. At 24 rounds,
/// the turn is forcefully terminated with a `loop_limit_exceeded` error.
///
/// Why 24? A reasonable upper bound for complex tool-use chains is ~10 rounds.
/// 24 gives ample headroom for legitimate use while still bounding resource
/// usage. Each round makes at least one LLM API call, so 24 rounds means at
/// most 24 API calls per turn.
const MAX_ROUNDS: usize = 24;

/// User decision for a pending tool call approval request.
///
/// This enum is the internal representation used by the lifecycle service.
/// The HTTP layer has its own `DecisionKind` that maps to these values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecisionKind {
    /// Execute this call once and keep the default policy for future calls.
    Allow,
    /// Execute this call and persist an allow-rule for future matching calls.
    ///
    /// The derived rule is stored in `session.preferences.tool_allow_rules`
    /// so future calls to the same tool with matching inputs are auto-approved.
    AllowAlways,
    /// Reject this call and send a synthetic error result back to the model.
    ///
    /// The model receives `"Denied by user"` as the tool output with
    /// `is_error: true`, giving it a chance to adjust its behavior.
    Deny,
}

/// One approval decision submitted by the user via `POST /approve`.
///
/// Each decision targets a specific pending tool call identified by its
/// `pending_call_id` (the `pcall_*` prefixed ID), not the provider's `call_id`.
#[derive(Debug, Clone)]
pub struct ApproveDecision {
    /// The stable identifier of the pending call (`pcall_*`).
    pub pending_call_id: String,
    /// The decision kind selected by the user.
    pub decision: ApprovalDecisionKind,
}

/// Application service that owns turn lifecycle orchestration.
///
/// This is a thin service-layer wrapper around `AppState`. It provides a
/// clean API for the HTTP handlers to call (create_turn, retry_turn,
/// approve_turn, cancel_turn) without exposing the implementation details
/// of the background loop, state persistence, and event publishing.
///
/// The service is stateless itself -- it borrows `AppState` and delegates
/// all real work to the `*_impl` free functions. This makes testing easier
/// (you can call the impl functions directly) and keeps the service methods
/// as simple delegation points.
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
    ///
    /// Returns immediately with the turn row. The actual LLM call happens in
    /// a spawned background task.
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
    ///
    /// The retry creates a NEW turn with the same parent as the old one (making
    /// them siblings in the tree), not a child. The old turn is linked via
    /// `retry_turn_id`.
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
    ///
    /// If all pending calls are resolved by this request, the loop is respawned.
    /// If some calls remain unresolved, the turn stays in `awaiting_approval`.
    pub async fn approve_turn(
        &self,
        session_id: Uuid,
        turn_id: Uuid,
        decisions: Vec<ApproveDecision>,
    ) -> Result<Turn, AppError> {
        approve_turn_impl(&self.state, session_id, turn_id, decisions).await
    }

    /// Cancel a running or awaiting-approval turn.
    ///
    /// Sets the turn status to FAILED with a "cancelled" error. Uses
    /// cooperative cancellation via `CancellationToken` for in-progress loops.
    pub async fn cancel_turn(&self, session_id: Uuid, turn_id: Uuid) -> Result<Turn, AppError> {
        cancel_turn_impl(&self.state, session_id, turn_id).await
    }
}

/// Validate that (session.protocol, provider, model) form a valid combination
/// according to the current config. Returns the resolved protocol on success.
///
/// This performs three checks:
/// 1. The provider name exists in the config
/// 2. The provider supports the session's protocol (e.g. OpenAI or Anthropic)
/// 3. The requested model is exposed by that provider
///
/// Called during turn creation and retry to fail fast before any DB writes.
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

    // Check that the provider has a binding for the session's protocol.
    // A provider might support OpenAI but not Anthropic, or vice versa.
    if provider.binding(protocol).is_none() {
        return Err(AppError::BadRequest(format!(
            "provider '{}' is not configured for protocol '{}'",
            provider_name,
            protocol.as_str()
        )));
    }

    // Check that the model ID is in the provider's model list.
    // This prevents sending invalid model names to the API.
    if !provider.has_model(model) {
        return Err(AppError::BadRequest(format!(
            "model '{}' is not exposed by provider '{}'",
            model, provider_name
        )));
    }

    Ok(protocol)
}

/// Builds the protocol-native user content block for transcript entries.
///
/// Each LLM protocol has its own format for user messages:
/// - OpenAI: `{ "role": "user", "content": "text" }`
/// - Anthropic: `{ "type": "text", "text": "text" }`
///
/// This function produces the inner content array that gets wrapped by
/// `build_user_entry`.
fn user_message_content(protocol: Protocol, user_text: &str) -> JsonValue {
    match protocol {
        Protocol::Openai => json!([{ "role": "user", "content": user_text }]),
        Protocol::Anthropic => json!([{ "type": "text", "text": user_text }]),
    }
}

/// Wraps protocol-native user blocks into the common transcript entry shape.
///
/// Transcript entries have a standard envelope: `{ "role": "user", "content": [...] }`.
/// The inner content array is protocol-specific (see `user_message_content`).
fn build_user_entry(protocol: Protocol, user_text: &str) -> JsonValue {
    json!({
        "role": "user",
        "content": user_message_content(protocol, user_text),
    })
}

/// Builds the persisted assistant transcript entry from one adapter response.
///
/// Stores everything we might need later: the content blocks (which may include
/// text, tool calls, reasoning), the response ID (for Anthropic's message
/// continuation), stop reason, usage stats, and the raw response for debugging.
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
///
/// Takes the first 50 characters of the user's text. This is only applied
/// when the session has no title yet (i.e. it's the first turn).
fn auto_title_from_user_text(user_text: &str) -> String {
    user_text.chars().take(50).collect()
}

/// Converts the internal decision enum to the persisted string form.
///
/// This maps between the lifecycle service's `ApprovalDecisionKind` and the
/// runtime state's `RecordedApprovalDecisionKind`. They're separate types
/// because the lifecycle version is a pure Rust enum while the persisted
/// version has serde attributes for JSON serialization.
fn decision_kind(decision: ApprovalDecisionKind) -> RecordedApprovalDecisionKind {
    match decision {
        ApprovalDecisionKind::Allow => RecordedApprovalDecisionKind::Allow,
        ApprovalDecisionKind::AllowAlways => RecordedApprovalDecisionKind::AllowAlways,
        ApprovalDecisionKind::Deny => RecordedApprovalDecisionKind::Deny,
    }
}

/// Returns session-level tool allow-rules from preferences JSON.
///
/// These rules were persisted by previous "AllowAlways" decisions. Each rule
/// is a string like `"bash(cargo check *)"` or `"read"` that is matched
/// against incoming tool calls during the three-layer classification.
///
/// Returns an empty vec if no rules are stored or if the preferences JSON
/// is malformed (defensive parsing).
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
///
/// Called when the user chooses "AllowAlways" for a tool call. The derived
/// rule string is added to the existing rules list if it's not already there.
/// Uses a HashSet for O(1) duplicate detection.
fn with_added_allow_rules(preferences: &JsonValue, added: &[String]) -> JsonValue {
    let mut out = preferences.clone();
    // Guard against malformed preferences (shouldn't happen but be safe)
    if !out.is_object() {
        out = json!({});
    }
    let mut rules: Vec<String> = allow_rules_from_preferences(preferences);
    // Use a HashSet for efficient duplicate detection
    let mut set: HashSet<String> = rules.iter().cloned().collect();
    for rule in added {
        // Only add if not already present (AllowAlways may be pressed multiple
        // times for similar calls)
        if set.insert(rule.clone()) {
            rules.push(rule.clone());
        }
    }
    out[session_preference_key::TOOL_ALLOW_RULES] = json!(rules);
    out
}

/// Parses persisted transcript entries from a turn row.
///
/// The `turn_messages` column stores a JSON array of transcript entries.
/// This function extracts it as a mutable Vec for appending new entries.
/// Returns an empty vec if the column is null or not an array (shouldn't
/// happen in practice but defensive).
fn parse_transcript(turn: &Turn) -> Vec<JsonValue> {
    turn.turn_messages
        .as_array()
        .cloned()
        .unwrap_or_else(Vec::new)
}

/// Publishes one live stream event to SSE subscribers via the hub.
///
/// This is the single point where lifecycle events are pushed to the
/// in-memory broadcast channel. Every event goes through this function,
/// ensuring the write-before-publish invariant is maintained (the caller
/// must persist the DB state BEFORE calling this).
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
///
/// Used when the CAS guard (`update_turn_if_active`) rejects an update,
/// meaning the turn was modified by another concurrent operation (e.g.
/// cancellation). Instead of returning an error, we reload the latest
/// state from DB so the caller can see what happened.
async fn reload_turn(state: &AppState, session_id: Uuid, turn_id: Uuid) -> Result<Turn, AppError> {
    get_turn_in_session(&state.db, session_id, turn_id).await
}

/// Appends executed tool results into the transcript and persists the turn state.
///
/// This helper centralizes the "write tool results to DB + publish SSE event"
/// pattern that is used both in the main loop (for auto-approved calls) and in
/// the approval handler (for user-approved calls).
///
/// # Parameters
/// - `status`: The target turn status after this update (usually `RUNNING` or
///   `AWAITING_APPROVAL` if some calls are still pending)
/// - `transcript`: Mutable reference to the in-memory transcript, the new entry
///   is appended in-place
/// - `results`: All tool execution results (both successful and error results)
/// - `runtime_state`: The runtime state to persist (should already have pending
///   calls cleared/updated before calling this)
///
/// # Returns
/// - `Ok(Some(updated_turn))` if the CAS guard accepted the update
/// - `Ok(None)` if the turn was already terminal (CAS rejected)
///
/// # Protocol awareness
///
/// Tool results are formatted differently per protocol:
/// - Anthropic: `{ "type": "tool_result", "tool_use_id": ..., "content": ... }`
/// - OpenAI: `{ "type": "function_call_output", "call_id": ..., "output": ... }`
///
/// The `tool_result_entry` function in `tooling.rs` handles this.
async fn append_tool_results_and_persist(
    state: &AppState,
    turn: &Turn,
    status: &str,
    transcript: &mut Vec<JsonValue>,
    results: &[ToolExecutionResult],
    runtime_state: &TurnRuntimeState,
) -> Result<Option<Turn>, AppError> {
    // We need the session's protocol to format the tool result entry correctly
    let session = get_session(&state.db, turn.session_id).await?;
    let protocol = session.protocol;

    // Build the protocol-native tool result transcript entry
    let entry = tool_result_entry(protocol, results);
    transcript.push(entry.clone());

    // Prepare the SSE payload with the full entry for frontend consumption
    let payload = json!({
        "entry": entry,
    });

    // Bump stream_seq to establish a new ordering boundary
    let (next_runtime_state, seq) = runtime_state.bump_stream_seq();

    // Persist the updated transcript and runtime state with CAS guard.
    // If the turn was cancelled or completed by another concurrent operation,
    // this returns None and the caller should reload the turn.
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

    // Publish the SSE event AFTER the DB write completes (write-before-publish)
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
///
/// # Why parallel execution?
///
/// Tool calls within one round are independent -- they don't depend on each
/// other's output. Executing them in parallel with `JoinSet` reduces latency
/// significantly when multiple calls are present (e.g. reading several files).
///
/// # Why preserve order?
///
/// The LLM expects tool results in the same order as the tool calls it made.
/// We track the original index of each call and place results back into a
/// position-indexed vector. This means:
/// - Calls execute concurrently (no ordering guarantee of completion)
/// - Results are reassembled into the original call order
///
/// # Cancellation
///
/// Each spawned task receives a clone of the `CancellationToken`. If the turn
/// is cancelled while tools are executing, individual tool implementations
/// (bash, read, write) check the token cooperively and return early.
async fn execute_tool_calls_parallel(
    calls: Vec<NormalizedToolCall>,
    cancel_token: &CancellationToken,
) -> Vec<ToolExecutionResult> {
    if calls.is_empty() {
        return Vec::new();
    }

    let mut set = JoinSet::new();
    // Spawn one task per tool call, tagging each with its original index.
    // JoinSet executes all tasks concurrently.
    for (idx, call) in calls.into_iter().enumerate() {
        let token = cancel_token.clone();
        set.spawn(async move { (idx, execute_tool_call(&call, &token).await) });
    }

    // Pre-allocate a slot for each result. `None` means the task hasn't
    // completed yet. We use Option here because tasks may complete in any
    // order and we need to place them at the correct index.
    let mut ordered: Vec<Option<ToolExecutionResult>> = vec![None; set.len()];
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((idx, result)) => {
                // Place the result at the original call index to preserve order
                ordered[idx] = Some(result);
            }
            Err(err) => {
                // JoinError means the task panicked or was cancelled.
                // We log it but don't have a result to place. The None slot
                // will be skipped by the flatten() below. This is acceptable
                // because a panicking tool executor is a bug, not a normal case.
                error!("tool task join error: {err}");
            }
        }
    }

    // Flatten: convert Vec<Option<T>> to Vec<T>, skipping any failed tasks.
    // In normal operation, all slots are filled.
    ordered.into_iter().flatten().collect()
}

/// THE core function: runs the multi-round model loop until completion,
/// approval pause, or error.
///
/// This function is called from a spawned background task (see `spawn_turn_loop`)
/// and may also be called indirectly from `approve_turn_impl` when resuming
/// after an approval pause.
///
/// # Loop invariant
///
/// At the top of each round, `turn` reflects the latest persisted DB state.
/// This is maintained by:
/// 1. Reading the turn at loop entry (passed as parameter)
/// 2. Updating `turn` after every successful `update_turn_if_active`
/// 3. Returning early (with a reload) when the CAS guard rejects
///
/// # Termination conditions
///
/// The loop exits when:
/// - The model returns no tool calls (natural completion)
/// - Tool calls need approval (pause and return)
/// - The cancellation token is fired (user cancelled)
/// - MAX_ROUNDS is exceeded (safety guard)
/// - `update_turn_if_active` rejects the write (turn was externally modified)
/// - The LLM adapter returns an error (propagated)
///
/// # Why we reload history each round
///
/// `get_path_to_turn_in_session` re-reads the full conversation path from DB
/// on every round rather than maintaining an in-memory transcript. This is
/// intentional: between rounds, new turns may have been added to the tree path
/// by other requests (e.g. forking), and we want the LLM to see the latest
/// history. The DB is the source of truth, not our in-memory state.
async fn continue_turn_loop(
    state: &AppState,
    mut turn: Turn,
    provider: &str,
    model: &str,
    cancel_token: CancellationToken,
) -> Result<Turn, AppError> {
    // Load session once at the start for protocol and system prompt.
    // Note: session.preferences may change between rounds (e.g. new allow-rules
    // from AllowAlways decisions), so we re-read allow_rules inside the loop.
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

    // Main multi-round loop. Each iteration:
    // 1. Loads fresh history from DB
    // 2. Calls the LLM
    // 3. Persists the assistant response
    // 4. Classifies and executes tool calls (or completes)
    for round_index in 0..MAX_ROUNDS {
        // Check for cooperative cancellation before starting a new round.
        // If the user cancelled while we were between rounds, return the
        // latest persisted state (which should already be FAILED from the
        // cancel handler).
        if cancel_token.is_cancelled() {
            return reload_turn(state, turn.session_id, turn.id).await;
        }

        // Re-read the full conversation path from DB on every round.
        // This ensures the LLM sees any new turns that may have been added
        // to the tree between rounds (e.g. from forking).
        let history =
            get_path_to_turn_in_session(&state.db, turn.session_id, Some(turn.id)).await?;

        // Call the LLM adapter with cooperative cancellation.
        // If cancelled during the API call, we return early instead of
        // processing the response.
        let send = tokio::select! {
            _ = cancel_token.cancelled() => return reload_turn(state, turn.session_id, turn.id).await,
            result = adapter.send(&history, None, model, instructions) => result?,
        };

        // --- Persist the assistant response entry ---

        // Build the transcript with the new assistant entry appended
        let mut transcript = parse_transcript(&turn);
        let assistant_entry = build_assistant_entry(&send);
        transcript.push(assistant_entry.clone());

        // Prepare SSE payloads for this round (we need the seq numbers before
        // the DB write so we can publish after it)
        let round_started_payload = json!({
            "round": round_index + 1,
        });
        // Bump seq for the round_started event
        let (runtime_state_after_round_start, round_started_seq) =
            turn.runtime_state.bump_stream_seq();
        let assistant_payload = json!({
            "entry": assistant_entry,
            "assistant_text": send.assistant_text,
            "input_tokens": send.input_tokens,
            "output_tokens": send.output_tokens,
            "cached_tokens": send.cached_tokens,
        });
        // Bump seq again for the assistant_entry_appended event
        let (runtime_state_after_assistant_entry, assistant_seq) =
            runtime_state_after_round_start.bump_stream_seq();

        // Persist the updated transcript, assistant text, token counts, and
        // runtime state (with cleared pending calls from any previous round).
        // Uses CAS guard: if the turn was cancelled/completed externally, the
        // update is rejected and we return the latest DB state.
        let Some(updated_turn) = update_turn_if_active(
            &state.db,
            turn.id,
            UpdateTurnParams {
                status: turn_status::RUNNING,
                assistant_text: send.assistant_text.as_deref(),
                turn_messages: &json!(transcript),
                // Clear any stale pending calls from a previous approval pause
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

        // Now publish the SSE events (write-before-publish invariant maintained)
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

        // --- Extract and classify tool calls ---

        // Parse tool calls from the protocol-native assistant content.
        // Returns an empty vec if the model didn't make any tool calls.
        let calls = extract_tool_calls(protocol, &send.assistant_content);

        // If no tool calls, the turn is complete -- persist COMPLETED and return.
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

        // --- Three-layer tool call classification ---
        //
        // For each tool call the model made, we decide whether to:
        // 1. Synthesize an "unknown tool" error (tool doesn't exist)
        // 2. Auto-execute (matched by allow-rule or default policy is Auto)
        // 3. Queue for user approval (default policy is RequireApproval and
        //    no allow-rule matches)

        // Re-read allow_rules from session preferences each round because
        // they may have been updated by an AllowAlways decision in the
        // approval handler since the last round.
        let allow_rules = allow_rules_from_preferences(&session.preferences);

        // Results for calls that produce immediate output (unknown tools get
        // a synthetic error result; auto-approved tools get real results)
        let mut auto_results: Vec<ToolExecutionResult> = Vec::new();
        // JSON descriptors for the tool_calls SSE event (auto-approved calls only)
        let mut auto_calls: Vec<JsonValue> = Vec::new();
        // Actual executable tool calls for auto-approved tools
        let mut executable_auto_calls: Vec<NormalizedToolCall> = Vec::new();
        // Tool calls that need user approval
        let mut pending: Vec<PendingToolCall> = Vec::new();

        for call in calls {
            // Layer 1: Tool existence check
            // If `default_policy` returns None, the tool name is not in our
            // registry. Synthesize an error result so the model knows.
            let Some(policy) = default_policy(&call.name) else {
                auto_results.push(ToolExecutionResult {
                    call_id: call.call_id.clone(),
                    output: format!("Unknown tool '{}'", call.name),
                    is_error: true,
                    error_kind: Some("unknown_tool".to_string()),
                });
                continue;
            };

            // Layer 2: Session allow-rules matching
            // Check if any previously-saved allow-rule matches this call.
            // Rules are strings like "bash(cargo check *)" or "read".
            let matched = allow_rules
                .iter()
                .any(|rule| match_allow_rule(rule, &call.name, &call.input));

            // Layer 3: Default tool policy
            // If an allow-rule matched OR the tool's default policy is Auto,
            // execute without asking the user.
            if matched || policy == ToolPolicy::Auto {
                auto_calls.push(json!({
                    "call_id": call.call_id.clone(),
                    "name": call.name.clone(),
                    "input": call.input.clone(),
                }));
                executable_auto_calls.push(call);
                continue;
            }

            // Neither allow-rules nor default policy allows auto-execution.
            // Queue for user approval with a `pcall_*` prefixed ID.
            pending.push(PendingToolCall {
                // The `pcall_` prefix disambiguates our stable ID from the
                // provider's `call_id`. The provider's call_id is an opaque
                // string like "call_abc123" that may vary between providers.
                // Our pending_call_id needs to be stable across the
                // approval round-trip (persist -> approve -> execute) and
                // identifiable by the frontend for rendering the approval UI.
                pending_call_id: format!("pcall_{}", Uuid::new_v4()),
                call_id: call.call_id,
                name: call.name,
                input: call.input,
            });
        }

        // --- Execute auto-approved tool calls ---

        let mut transcript = parse_transcript(&turn);
        let mut runtime_state_for_auto = turn.runtime_state.clone();

        if !executable_auto_calls.is_empty() {
            // Publish the tool_calls event to show "executing..." in the UI
            let tool_calls_payload = json!({
                "calls": auto_calls,
            });
            let (next_runtime_state, seq) = runtime_state_for_auto.bump_stream_seq();
            runtime_state_for_auto = next_runtime_state;

            // Persist state before executing (so the CAS guard protects us)
            let Some(updated_turn) = update_turn_if_active(
                &state.db,
                turn.id,
                UpdateTurnParams {
                    status: turn_status::RUNNING,
                    assistant_text: turn.assistant_text.as_deref(),
                    turn_messages: &turn.turn_messages,
                    // Clear pending in case there's stale data from a previous round
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
            // Re-read runtime state from the persisted turn (it was updated by
            // update_turn_if_active with the cleared pending state)
            runtime_state_for_auto = turn.runtime_state.clone();

            // Publish the tool_calls SSE event (write-before-publish)
            publish_stream_event(
                state,
                turn.id,
                seq,
                stream_event::TOOL_CALLS,
                tool_calls_payload,
            )
            .await;

            // Execute all auto-approved calls in parallel via JoinSet.
            // Results maintain original call order despite concurrent execution.
            auto_results
                .extend(execute_tool_calls_parallel(executable_auto_calls, &cancel_token).await);
        }

        // Persist auto-approved tool results (both real outputs and unknown-tool errors)
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

        // --- Approval pause mechanism ---
        //
        // If any calls need user approval, we:
        // 1. Persist the pending calls in `runtime_state.pending_tool_calls`
        // 2. Transition turn status to `AWAITING_APPROVAL`
        // 3. Publish `APPROVAL_NEEDED` event
        // 4. Return early from the loop
        //
        // The loop is later resumed by `approve_turn_impl` which:
        // - Processes user decisions
        // - Executes approved calls
        // - Re-spawns the loop via `spawn_turn_loop`
        //
        // This pause/resume design means the loop does NOT hold a long-lived
        // stack frame while waiting for approval. It persists its state and
        // exits, allowing the process to handle other requests.
        if !pending.is_empty() {
            let approval_payload = json!({
                "pending": pending.clone(),
            });
            let (next_runtime_state, seq) = turn.runtime_state.bump_stream_seq();
            // Store the pending calls in runtime_state so they survive
            // process restarts and can be shown to the user on page refresh
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
            // Return early -- the loop pauses here and waits for POST /approve
            return Ok(turn);
        }

        // All tool calls were auto-approved and executed. Continue to the next
        // round where the LLM will see the tool results and respond.
    }

    // --- Loop limit guard ---
    //
    // If we've exhausted MAX_ROUNDS iterations, force-fail the turn.
    // This prevents infinite loops from models that keep making tool calls
    // that produce more tool calls.
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
///
/// This function:
/// 1. Registers the turn with `TurnTaskManager` to get a CancellationToken and
///    a generation number
/// 2. Spawns a tokio task that runs `continue_turn_loop`
/// 3. On completion, marks the task as finished with the manager
///
/// # Cooperative cancellation
///
/// The `CancellationToken` is shared between the spawned task and the task
/// manager. When `cancel_turn_impl` calls `state.turn_task_manager.cancel(turn_id)`,
/// it signals the token, and the loop checks `cancel_token.is_cancelled()` at
/// every round boundary and via `tokio::select!` during LLM calls.
///
/// # Generation guard
///
/// The `generation` number prevents stale finish calls. If a turn was
/// cancelled and a new loop was started (e.g. from approve after cancel),
/// the old loop's finish call is for a different generation and is ignored
/// by the task manager. This prevents the new loop's registration from being
/// incorrectly marked as finished.
///
/// # Error handling
///
/// If `continue_turn_loop` returns an error (e.g. LLM provider failure), the
/// spawned task attempts to mark the turn as FAILED. It uses the CAS guard
/// to avoid overwriting a terminal state that may have been set by a concurrent
/// cancellation.
async fn spawn_turn_loop(state: AppState, turn: Turn, provider: String, model: String) {
    // Register with the task manager to get a cancellation token and generation
    let registration = state.turn_task_manager.register(turn.id).await;
    let generation = registration.generation;
    let cancel_token = registration.token.clone();

    tokio::spawn(async move {
        // Run the multi-round loop. This may take seconds to minutes.
        let loop_result = continue_turn_loop(
            &state,
            turn.clone(),
            &provider,
            &model,
            cancel_token.clone(),
        )
        .await;

        // Always mark the task as finished, even if the loop failed.
        // The generation number ensures this only marks the current
        // registration as finished, not a newer one.
        state.turn_task_manager.finish(turn.id, generation).await;

        // If the loop returned an error, attempt to persist a FAILED state.
        // This handles cases like LLM provider errors that are not caught
        // inside the loop itself.
        if let Err(e) = loop_result {
            error!("Turn loop failed for {}: {}", turn.id, e);

            // Reload the turn to check its current state. It may have been
            // cancelled or completed by a concurrent operation while the
            // loop was running.
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

            // If the turn already reached a terminal state (e.g. cancelled
            // while the loop was erroring), don't overwrite it.
            if turn_status::is_terminal(&latest.status) {
                return;
            }

            // Persist the error as a FAILED state
            let error_json = json!({ "kind": "loop_error", "message": e.to_string() });
            let failed_payload = json!({
                "error": error_json.clone(),
            });
            let (next_runtime_state, seq) = latest.runtime_state.bump_stream_seq();
            // Use CAS guard: if the turn was externally modified since our
            // reload, don't overwrite
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
///
/// This is the implementation behind `TurnLifecycleService::create_turn`.
/// It performs the following steps:
///
/// 1. **Validation**: checks root turn uniqueness, failed parent rejection,
///    and protocol/provider/model validity
/// 2. **Turn creation**: inserts a new turn row via `create_turn`
/// 3. **Transcript initialization**: appends the user message as the first
///    transcript entry
/// 4. **Event emission**: publishes `turn_started` SSE event
/// 5. **Session metadata**: auto-titles the session if this is the first turn,
///    touches the session's updated_at timestamp
/// 6. **Background loop**: spawns `spawn_turn_loop` to start the multi-round
///    LLM conversation
///
/// # Root turn uniqueness
///
/// A session can only have ONE root turn (a turn with no parent). All
/// subsequent turns must fork from an existing turn by providing a
/// `parent_turn_id`. This enforces the tree structure.
///
/// # Failed parent rejection
///
/// You cannot create a child turn from a failed parent. This is because
/// a failed turn may have incomplete state. Use retry instead, which creates
/// a sibling with the same parent.
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

    // Enforce root turn uniqueness: if no parent is specified, this is the
    // root turn. A session can only have one root turn -- all other turns
    // must fork from an existing turn.
    if parent_turn_id.is_none() {
        let has_root = session_has_root_turn(&state.db, session_id).await?;
        if has_root {
            return Err(AppError::BadRequest(
                "Session already has a root turn. Use parent_turn_id to fork from an existing turn.".to_string(),
            ));
        }
    }

    // Reject creation from a failed parent. Failed turns may have incomplete
    // state (partial transcript, errors) that makes them unsuitable as
    // conversation context. The user should use retry instead.
    if let Some(parent_id) = parent_turn_id {
        let parent = get_turn_in_session(&state.db, session_id, parent_id).await?;
        if parent.status == turn_status::FAILED {
            return Err(AppError::BadRequest(
                "Cannot reply to a failed turn. Use retry instead.".to_string(),
            ));
        }
    }

    let session = get_session(&state.db, session_id).await?;

    // Auto-title: if the session has no title yet, derive one from the user text.
    // This provides a human-readable title in the session list without requiring
    // the user to set one explicitly.
    let auto_title = session
        .title
        .is_none()
        .then(|| auto_title_from_user_text(user_text));

    // Validate the protocol/provider/model combination before doing any DB writes
    let protocol = validate_dispatch(state, session.protocol, provider, model)?;
    // Pre-check that the adapter is available in the registry. This catches
    // misconfiguration early rather than failing inside the spawned task.
    let _ = state.registry.get(protocol, provider).ok_or_else(|| {
        AppError::Internal(eyre::eyre!(
            "registry missing adapter for ({}, {})",
            protocol.as_str(),
            provider
        ))
    })?;

    // Insert the bare turn row with RUNNING status
    let turn = create_turn(
        &state.db,
        session_id,
        parent_turn_id,
        turn_status::RUNNING,
        user_text,
    )
    .await?;

    // Build the initial transcript with the user message as the first entry
    let init_messages = json!([build_user_entry(protocol, user_text)]);
    let (next_runtime_state, seq) = turn.runtime_state.bump_stream_seq();

    // Update the turn with the initial transcript and runtime state.
    // Uses `update_turn` (not `update_turn_if_active`) because this is the
    // first write after creation -- there's no race to guard against.
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

    // Publish turn_started event AFTER the DB write (write-before-publish)
    publish_stream_event(state, turn.id, seq, stream_event::TURN_STARTED, json!({})).await;

    // Touch session updated_at so the session appears at the top of the list
    let _ = touch_session_updated_at(&state.db, session_id).await;
    // Apply auto-title if this is the first turn
    if let Some(title) = &auto_title {
        update_session_title(&state.db, session_id, title).await?;
    }

    // Spawn the background loop to start the multi-round LLM conversation.
    // This returns immediately -- the HTTP response is already on its way.
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
///
/// # Why retry creates a SIBLING (same parent), not a child
///
/// Retry is meant to "re-run the same prompt with different parameters or a
/// different model". Creating a child would mean the new turn inherits the
/// old turn's conversation history as context, which is wrong -- the retry
/// should start from the same point as the original turn.
///
/// By using the same `parent_turn_id` as the old turn, both turns become
/// siblings in the tree:
///
/// ```text
///   parent
///     ├── old_turn (FAILED or user wants to retry)
///     └── new_turn (retry, same user_text, new provider/model)
/// ```
///
/// The old turn is linked to the new one via `retry_turn_id`, so the frontend
/// can show "this turn was retried as [new_turn]".
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

    // Reuse the original user text from the old turn
    let user_text = old_turn.user_text.clone().unwrap_or_default();

    // Create a NEW turn as a SIBLING of the old turn (same parent).
    // This is the key difference from creating a child turn.
    let new_turn = create_turn(
        &state.db,
        session_id,
        // Use the OLD turn's parent, not the old turn itself.
        // This makes the new turn a sibling, not a child.
        old_turn.parent_turn_id,
        turn_status::RUNNING,
        &user_text,
    )
    .await?;

    // Initialize the new turn's transcript with the same user message
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

    // Publish turn_started for the new turn
    publish_stream_event(
        state,
        new_turn.id,
        seq,
        stream_event::TURN_STARTED,
        json!({}),
    )
    .await;
    let _ = touch_session_updated_at(&state.db, session_id).await;

    // Link the old turn to the new one via `retry_turn_id`.
    // This is a no-status-change update: we preserve all of the old turn's
    // existing state and only add the `retry_turn_id` link.
    // The frontend can use this to show "retried as turn X" or navigate
    // from the old turn to the new one.
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
            // Link old turn -> new turn
            retry_turn_id: Some(new_turn.id),
        },
    )
    .await?;

    // Spawn the background loop for the new turn
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
///
/// This function handles the `POST /approve` endpoint. It:
/// 1. Validates the approval request (idempotency, conflict detection)
/// 2. Processes each decision: Allow, AllowAlways, or Deny
/// 3. Persists AllowAlways rules to session preferences
/// 4. Executes approved tool calls in parallel
/// 5. Persists results and updates turn status
/// 6. Respawns the loop if all pending calls are resolved
///
/// # Idempotency
///
/// If the frontend retries an approval request (e.g. network flakiness), we
/// check `runtime_state.approval_decisions` for each decision. If every
/// decision in the request matches a previously recorded one, we return the
/// current turn state without error. This makes approval safe to retry.
///
/// # Partial approvals
///
/// The user can approve some calls while leaving others pending. In this case:
/// - Approved/Denied calls are processed immediately
/// - Remaining calls stay in `pending_tool_calls`
/// - Turn status stays `AWAITING_APPROVAL` (not respawned)
/// - The user can submit another approval request for the remaining calls
///
/// # AllowAlways rule persistence
///
/// When the user chooses AllowAlways, a rule is derived from the tool call
/// (e.g. `"bash(cargo check)"`) and persisted to `session.preferences`.
/// Future calls to the same tool with matching inputs will be auto-approved
/// without user interaction.
async fn approve_turn_impl(
    state: &AppState,
    session_id: Uuid,
    turn_id: Uuid,
    decisions: Vec<ApproveDecision>,
) -> Result<Turn, AppError> {
    let mut turn = get_turn_in_session(&state.db, session_id, turn_id).await?;
    let mut recorded_decisions = turn.runtime_state.approval_decisions.clone();

    // --- Idempotency and status check ---
    //
    // If the turn is not in AWAITING_APPROVAL status, we still check if
    // all decisions are idempotent (exact match of previously recorded
    // decisions). This handles the case where the frontend retries an
    // approval request that was already processed.
    if turn.status != turn_status::AWAITING_APPROVAL {
        let all_idempotent = decisions.iter().all(|d| {
            recorded_decisions
                .get(&d.pending_call_id)
                .is_some_and(|v| *v == decision_kind(d.decision))
        });
        if all_idempotent {
            // All decisions match previously recorded ones -- safe duplicate
            return Ok(turn);
        }
        // At least one decision is new or different, but the turn is not
        // awaiting approval. This is a real conflict.
        return Err(AppError::Conflict(format!(
            "turn {} is not awaiting approval",
            turn.id
        )));
    }

    let session = get_session(&state.db, session_id).await?;
    let pending = turn.runtime_state.pending_tool_calls.clone();

    // Build a set of currently pending call IDs for fast lookup
    let pending_ids: HashSet<&str> = pending.iter().map(|p| p.pending_call_id.as_str()).collect();

    // Validate each decision in the request:
    // - If the pending_call_id is in the current pending list, it's valid
    // - If it's NOT in the pending list, check if it was already decided
    //   (idempotent retry)
    // - If it's neither pending nor previously decided, it's a conflict
    for decision in &decisions {
        if pending_ids.contains(decision.pending_call_id.as_str()) {
            // Valid: this call is currently pending
            continue;
        }
        // Not in pending list -- check if it was already decided (idempotent)
        let idempotent = recorded_decisions
            .get(&decision.pending_call_id)
            .is_some_and(|v| *v == decision_kind(decision.decision));
        if !idempotent {
            // Neither pending nor previously decided with same value -- conflict
            return Err(AppError::Conflict(format!(
                "pending call id '{}' is not active",
                decision.pending_call_id
            )));
        }
    }

    // Build a lookup map from pending_call_id -> decision kind
    let mut decisions_map: HashMap<String, ApprovalDecisionKind> = HashMap::new();
    for decision in decisions {
        decisions_map.insert(decision.pending_call_id, decision.decision);
    }

    // --- Process each pending call ---
    let mut remaining = Vec::new();
    let mut results = Vec::new();
    let mut add_rules: Vec<String> = Vec::new();
    let mut execute_calls: Vec<NormalizedToolCall> = Vec::new();

    for pending_call in pending {
        let Some(decision) = decisions_map.get(&pending_call.pending_call_id).copied() else {
            // No decision provided for this call -- it remains pending
            // (partial approval scenario)
            remaining.push(pending_call);
            continue;
        };

        // Record the decision in the approval history for idempotency
        recorded_decisions.insert(
            pending_call.pending_call_id.clone(),
            decision_kind(decision),
        );

        match decision {
            // Allow: execute the call once, no rule persisted
            ApprovalDecisionKind::Allow => execute_calls.push(NormalizedToolCall {
                call_id: pending_call.call_id.clone(),
                name: pending_call.name.clone(),
                input: pending_call.input.clone(),
            }),
            // AllowAlways: execute the call AND persist an allow-rule
            ApprovalDecisionKind::AllowAlways => {
                // Derive the allow-rule string (e.g. "bash(cargo check)" or "read")
                add_rules.push(derive_allow_rule(&pending_call.name, &pending_call.input));
                execute_calls.push(NormalizedToolCall {
                    call_id: pending_call.call_id.clone(),
                    name: pending_call.name.clone(),
                    input: pending_call.input.clone(),
                });
            }
            // Deny: synthesize an error result that the model will see
            ApprovalDecisionKind::Deny => results.push(ToolExecutionResult {
                call_id: pending_call.call_id.clone(),
                output: "Denied by user".to_string(),
                is_error: true,
                error_kind: Some("denied_by_user".to_string()),
            }),
        }
    }

    // Persist AllowAlways rules to session preferences (if any)
    if !add_rules.is_empty() {
        let updated_preferences = with_added_allow_rules(&session.preferences, &add_rules);
        update_session_preferences(&state.db, session_id, &updated_preferences).await?;
    }

    // --- Execute approved calls ---
    //
    // We register with the task manager to get a CancellationToken, which
    // enables cooperative cancellation during tool execution. This is
    // important because tool calls (especially bash) can take a long time.
    let mut runtime_state_with_decisions = turn
        .runtime_state
        .with_pending(remaining.clone())
        .with_approval_decisions(recorded_decisions.clone());

    if !execute_calls.is_empty() {
        // Register with task manager for cancellation support during execution
        let approval_registration = state.turn_task_manager.register(turn.id).await;

        // Build the tool_calls SSE payload
        let tool_calls_payload = json!({
            "calls": execute_calls.iter().map(|call| {
                json!({
                    "call_id": call.call_id.clone(),
                    "name": call.name.clone(),
                    "input": call.input.clone(),
                })
            }).collect::<Vec<_>>(),
        });

        // Bump stream_seq and persist state before executing
        let (next_runtime_state, seq) = runtime_state_with_decisions.bump_stream_seq();
        runtime_state_with_decisions = next_runtime_state;

        let Some(updated_turn) = update_turn_if_active(
            &state.db,
            turn.id,
            UpdateTurnParams {
                // Keep AWAITING_APPROVAL status during execution; it will be
                // changed to RUNNING or stay AWAITING_APPROVAL after results
                // are persisted (depending on whether calls remain pending)
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
            // CAS guard rejected -- turn was externally modified (e.g. cancelled)
            state
                .turn_task_manager
                .finish(turn.id, approval_registration.generation)
                .await;
            return reload_turn(state, session_id, turn.id).await;
        };
        turn = updated_turn;
        runtime_state_with_decisions = turn.runtime_state.clone();

        // Publish tool_calls event (write-before-publish)
        publish_stream_event(
            state,
            turn.id,
            seq,
            stream_event::TOOL_CALLS,
            tool_calls_payload,
        )
        .await;

        // Execute approved tool calls in parallel with cancellation support
        results
            .extend(execute_tool_calls_parallel(execute_calls, &approval_registration.token).await);

        // Mark the approval registration as finished
        state
            .turn_task_manager
            .finish(turn.id, approval_registration.generation)
            .await;
    }

    // --- Persist results and update status ---
    if !results.is_empty() {
        // There are results to persist (from executed calls or denied calls)
        let mut transcript = parse_transcript(&turn);
        // Determine status based on whether all pending calls are resolved
        let target_status = if remaining.is_empty() {
            turn_status::RUNNING
        } else {
            turn_status::AWAITING_APPROVAL
        };
        let Some(updated_turn) = append_tool_results_and_persist(
            state,
            &turn,
            target_status,
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
        // No results to persist (all decisions were for calls not in the
        // current pending list -- they were already processed). Just update
        // the runtime state and status.
        let target_status = if remaining.is_empty() {
            turn_status::RUNNING
        } else {
            turn_status::AWAITING_APPROVAL
        };
        let Some(updated_turn) = update_turn_if_active(
            &state.db,
            turn.id,
            UpdateTurnParams {
                status: target_status,
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

    // --- Resume the loop if all calls are resolved ---
    //
    // If the turn is back in RUNNING status (all pending calls resolved),
    // respawn the background loop to continue the multi-round conversation.
    // The loop will call the LLM again with the tool results appended to
    // the transcript.
    if turn.status == turn_status::RUNNING {
        let provider = turn.provider.clone().unwrap_or_default();
        let model = turn.model.clone().unwrap_or_default();
        spawn_turn_loop(state.clone(), turn.clone(), provider, model).await;
    }

    Ok(turn)
}

/// Marks a non-terminal turn as cancelled and emits a terminal failure event.
///
/// Cancellation is cooperative and two-pronged:
/// 1. **CancellationToken**: signals the background loop to stop at the next
///    cancellation check (round boundary or `tokio::select!` during LLM call)
/// 2. **CAS status guard**: persists `FAILED` with a "cancelled" error via
///    `update_turn_if_active`. If the loop has already persisted a terminal
///    state, the update is rejected and we return whatever state was written.
///
/// This ensures that:
/// - The loop stops promptly (via the cancellation token)
/// - The DB state is consistent (the CAS guard prevents stale writes)
/// - If the loop already completed, we don't overwrite its final state
async fn cancel_turn_impl(
    state: &AppState,
    session_id: Uuid,
    turn_id: Uuid,
) -> Result<Turn, AppError> {
    let turn = get_turn_in_session(&state.db, session_id, turn_id).await?;

    // If already terminal, return as-is -- nothing to cancel
    if turn_status::is_terminal(&turn.status) {
        return Ok(turn);
    }

    // Signal the background loop to stop cooperatively.
    // The cancellation token is shared with the spawned task, so
    // `cancel()` notifies all holders. We don't await the loop's exit
    // here -- it will stop on its own at the next cancellation check.
    let _ = state.turn_task_manager.cancel(turn_id).await;

    let error_json = json!({ "kind": "cancelled", "message": "Turn cancelled by user" });
    let failed_payload = json!({
        "error": error_json.clone(),
    });
    let (next_runtime_state, seq) = turn.runtime_state.bump_stream_seq();

    // Persist FAILED with the cancellation error. Uses CAS guard: if the
    // loop has already persisted a terminal state (e.g. it completed
    // between our initial read and this write), the update is rejected.
    let Some(turn) = update_turn_if_active(
        &state.db,
        turn.id,
        UpdateTurnParams {
            status: turn_status::FAILED,
            assistant_text: turn.assistant_text.as_deref(),
            turn_messages: &turn.turn_messages,
            // Clear pending calls since the turn is now terminal
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
        // CAS rejected -- the turn was already in a terminal state.
        // Reload to return the actual final state.
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
