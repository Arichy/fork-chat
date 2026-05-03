//! HTTP handlers for the turns API.
//!
//! This module implements the request/response layer for turn operations:
//! - `POST /sessions/:id/turns` -- create a new turn
//! - `GET /sessions/:id/turns/:id` -- get a single turn
//! - `GET /sessions/:id/turns/tree` -- get all turns for tree rendering
//! - `POST /sessions/:id/turns/:id/retry` -- retry a turn as a sibling
//! - `POST /sessions/:id/turns/:id/approve` -- submit approval decisions
//! - `POST /sessions/:id/turns/:id/cancel` -- cancel a running turn
//! - `GET /sessions/:id/turns/:id/stream` -- SSE stream for live updates
//!
//! Most handlers are thin delegation layers that parse the request, call
//! `TurnLifecycleService`, and return the response. The exception is
//! `stream_turn_handler`, which implements the Snapshot + Live SSE protocol.

use std::convert::Infallible;

use async_stream::stream;
use axum::{
    Json,
    extract::{Path, State},
    response::sse::{Event, KeepAlive, Sse},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::config::AppState;
use crate::db::{get_session, get_session_tree, get_turn_in_session};
use crate::error::AppError;
use crate::models::Turn;
use crate::turn_lifecycle::{ApprovalDecisionKind, ApproveDecision, TurnLifecycleService};
use crate::turn_runtime::{status as turn_status, stream_event};

/// HTTP payload for creating a new turn in a session.
#[derive(Debug, Deserialize)]
pub struct CreateTurnRequest {
    /// Optional parent turn id used for branching/forking.
    ///
    /// If None, this creates the root turn for the session (only one allowed).
    /// If Some, this creates a child turn of the specified parent, forming a
    /// branch in the conversation tree.
    pub parent_turn_id: Option<Uuid>,
    /// User prompt text for the new turn.
    pub user_text: String,
    /// Selected provider name (must exist in server config).
    pub provider: String,
    /// Selected model id (must be exposed by the provider).
    pub model: String,
}

/// HTTP payload for retrying an existing turn.
///
/// Retry creates a new turn as a sibling of the old one (same parent)
/// with the same user text but potentially different provider/model.
#[derive(Debug, Deserialize)]
pub struct RetryTurnRequest {
    /// Selected provider name.
    pub provider: String,
    /// Selected model id.
    pub model: String,
}

/// HTTP payload for approval submission.
///
/// Contains one or more decisions for pending tool calls. The user can
/// submit partial approvals (not all pending calls need to be decided).
#[derive(Debug, Deserialize)]
pub struct ApproveTurnRequest {
    /// One or more decisions for pending tool calls.
    pub decisions: Vec<ApproveDecisionRequest>,
}

/// One user decision for one pending call id.
#[derive(Debug, Deserialize)]
pub struct ApproveDecisionRequest {
    /// Pending call id (`pcall_*`) displayed in UI.
    ///
    /// This is our internal stable ID, not the provider's call_id.
    /// The `pcall_` prefix makes it easy to distinguish in the frontend.
    pub pending_call_id: String,
    /// Decision kind chosen by the user.
    pub decision: DecisionKind,
}

/// Serializable decision kinds accepted by the API.
///
/// Maps to `ApprovalDecisionKind` in the lifecycle service. Uses snake_case
/// JSON serialization for the REST API.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    /// Execute once -- no rule is persisted.
    Allow,
    /// Execute and persist an allow-rule for future matching calls.
    AllowAlways,
    /// Reject and synthesize an error result back to the model.
    Deny,
}

impl From<DecisionKind> for ApprovalDecisionKind {
    fn from(value: DecisionKind) -> Self {
        match value {
            DecisionKind::Allow => ApprovalDecisionKind::Allow,
            DecisionKind::AllowAlways => ApprovalDecisionKind::AllowAlways,
            DecisionKind::Deny => ApprovalDecisionKind::Deny,
        }
    }
}

/// Standard turn response wrapper.
///
/// Used for endpoints that return a single updated turn (get, approve, cancel).
#[derive(Debug, Serialize)]
pub struct TurnResponse {
    /// Updated turn row.
    pub turn: Turn,
}

/// Response payload for creation-style endpoints.
///
/// Used for create and retry endpoints that return a newly created turn.
#[derive(Debug, Serialize)]
pub struct CreateTurnResponse {
    /// Newly created turn row.
    pub turn: Turn,
}

/// Response payload for tree endpoint.
///
/// Returns all turns in a session, ordered by creation time, for rendering
/// the conversation tree in the frontend.
#[derive(Debug, Serialize)]
pub struct TreeResponse {
    /// All turns belonging to a session ordered by creation time.
    pub turns: Vec<Turn>,
}

/// Builds the full turn snapshot payload sent on every SSE subscription.
///
/// This payload contains ALL state the frontend needs to render a turn:
/// status, transcript, runtime state (pending approvals), assistant text,
/// token usage, and any error. It is the authoritative "current truth" for
/// the turn at the moment of subscription.
///
/// The `seq` field is critical: it tells the frontend the baseline sequence
/// number, so it can ignore any live events that are already reflected in
/// this snapshot.
fn snapshot_payload(turn: &Turn) -> serde_json::Value {
    json!({
        "seq": turn.runtime_state.stream_seq,
        "status": turn.status,
        "turn_messages": turn.turn_messages,
        "runtime_state": turn.runtime_state,
        "assistant_text": turn.assistant_text,
        "input_tokens": turn.input_tokens,
        "output_tokens": turn.output_tokens,
        "cached_tokens": turn.cached_tokens,
        "error": turn.error,
    })
}

/// Builds the payload wrapper used by all live SSE events.
///
/// Live events are wrapped in `{ "seq": N, "payload": {...} }` so the
/// frontend can check the sequence number for ordering.
fn live_event_payload(seq: u64, payload: serde_json::Value) -> serde_json::Value {
    json!({
        "seq": seq,
        "payload": payload,
    })
}

/// Returns whether one live event should be forwarded after a snapshot whose
/// state already reflects `baseline_seq`.
///
/// Events with `seq <= baseline_seq` are already incorporated in the
/// snapshot, so forwarding them would cause duplicate state application.
fn should_forward_live_event(seq: u64, baseline_seq: u64) -> bool {
    seq > baseline_seq
}

/// Creates a turn and starts the background lifecycle loop.
///
/// Delegates to `TurnLifecycleService::create_turn`. Returns immediately
/// with the newly created turn row -- the actual LLM call happens in a
/// background task.
pub async fn create_turn_handler(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<CreateTurnRequest>,
) -> Result<Json<CreateTurnResponse>, AppError> {
    let service = TurnLifecycleService::new(state);
    let turn = service
        .create_turn(
            session_id,
            req.parent_turn_id,
            &req.user_text,
            &req.provider,
            &req.model,
        )
        .await?;
    Ok(Json(CreateTurnResponse { turn }))
}

/// Returns one turn by session and id.
///
/// Simple DB read -- no lifecycle involvement.
pub async fn get_turn_handler(
    State(state): State<AppState>,
    Path((session_id, turn_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<TurnResponse>, AppError> {
    let turn = get_turn_in_session(&state.db, session_id, turn_id).await?;
    Ok(Json(TurnResponse { turn }))
}

/// Returns all turns in one session for tree rendering.
///
/// The frontend uses this to build the conversation tree visualization.
/// All turns are returned flat; the tree structure is derived from
/// `parent_turn_id` relationships.
pub async fn get_session_tree_handler(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<TreeResponse>, AppError> {
    // Verify the session exists first (returns 404 if not)
    get_session(&state.db, session_id).await?;
    let turns = get_session_tree(&state.db, session_id).await?;
    Ok(Json(TreeResponse { turns }))
}

/// Retries a turn by creating a new sibling branch and restarting execution.
///
/// Delegates to `TurnLifecycleService::retry_turn`. The new turn is a
/// sibling of the old one (same parent), not a child.
pub async fn retry_turn_handler(
    State(state): State<AppState>,
    Path((session_id, old_turn_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<RetryTurnRequest>,
) -> Result<Json<CreateTurnResponse>, AppError> {
    let service = TurnLifecycleService::new(state);
    let turn = service
        .retry_turn(session_id, old_turn_id, &req.provider, &req.model)
        .await?;
    Ok(Json(CreateTurnResponse { turn }))
}

/// Applies approval decisions and potentially resumes loop execution.
///
/// Delegates to `TurnLifecycleService::approve_turn`. Converts the HTTP
/// request types to the lifecycle service's internal types.
pub async fn approve_turn_handler(
    State(state): State<AppState>,
    Path((session_id, turn_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<ApproveTurnRequest>,
) -> Result<Json<TurnResponse>, AppError> {
    // Convert HTTP-layer types to lifecycle-layer types
    let decisions = req
        .decisions
        .into_iter()
        .map(|decision| ApproveDecision {
            pending_call_id: decision.pending_call_id,
            decision: decision.decision.into(),
        })
        .collect();
    let service = TurnLifecycleService::new(state);
    let turn = service.approve_turn(session_id, turn_id, decisions).await?;
    Ok(Json(TurnResponse { turn }))
}

/// Cancels a running or awaiting-approval turn.
///
/// Delegates to `TurnLifecycleService::cancel_turn`. Uses cooperative
/// cancellation -- signals the background loop to stop and persists a
/// FAILED state.
pub async fn cancel_turn_handler(
    State(state): State<AppState>,
    Path((session_id, turn_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<TurnResponse>, AppError> {
    let service = TurnLifecycleService::new(state);
    let turn = service.cancel_turn(session_id, turn_id).await?;
    Ok(Json(TurnResponse { turn }))
}

/// Streams one turn's latest snapshot followed by live updates over SSE.
///
/// This handler implements the "Snapshot + Live SSE" protocol:
///
/// 1. **First read** (initial_turn): load the turn to check if it's terminal
/// 2. **Subscribe to live events** (if active): BEFORE reading the snapshot,
///    subscribe to the broadcast channel. This ordering is critical to avoid
///    the race condition where an event is emitted between reading the snapshot
///    and subscribing to live events.
/// 3. **Second read** (turn): load the turn again as the snapshot source.
///    Because we subscribed before this read, any events emitted between steps
///    2 and 3 will arrive in the live channel.
/// 4. **Emit snapshot**: send the full turn state as a `turn_snapshot` event
/// 5. **Forward live events**: iterate the broadcast receiver, forwarding only
///    events with `seq > baseline_seq` (the snapshot's stream_seq)
/// 6. **Close on terminal**: break the loop when a terminal event is received
///
/// # Why subscribe FIRST, then read snapshot?
///
/// This ordering closes the race window:
///
/// ```text
///    subscribe to live events   <-- events after this point are captured
///    read snapshot from DB      <-- includes events that happened before this
///    emit snapshot              <-- baseline_seq = snapshot.stream_seq
///    forward live events with seq > baseline_seq
/// ```
///
/// If we read the snapshot first and then subscribed:
/// - An event could be published between the read and the subscribe
/// - That event would NOT be in the snapshot AND NOT in the live receiver
/// - The subscriber would miss it entirely
///
/// # Error handling
///
/// - `Lagged` errors are silently continued. This happens when the SSE
///   consumer is slow and the broadcast buffer fills up. The subscriber
///   may miss some intermediate events but will get the next snapshot
///   on reconnect.
/// - `Closed` errors break the loop. This means the broadcast channel was
///   dropped (all senders gone), which happens after a terminal event or
///   when the hub cleans up the channel.
pub async fn stream_turn_handler(
    State(state): State<AppState>,
    Path((session_id, turn_id)): Path<(Uuid, Uuid)>,
) -> Result<Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>>, AppError> {
    // Step 1: Initial read to determine if the turn is terminal.
    // For terminal turns, we just emit a snapshot and close.
    let initial_turn = get_turn_in_session(&state.db, session_id, turn_id).await?;

    let mut live_rx = None;
    let turn = if turn_status::is_terminal(&initial_turn.status) {
        // Terminal turn: no live events needed. Just emit snapshot and close.
        initial_turn
    } else {
        // Active turn: subscribe to live events FIRST (before reading snapshot)
        // to close the race window. See method-level doc for explanation.
        live_rx = Some(state.turn_stream_hub.subscribe(turn_id).await);
        // Read the snapshot AFTER subscribing, so any events emitted between
        // subscribe and this read will arrive in the live channel.
        get_turn_in_session(&state.db, session_id, turn_id).await?
    };

    // Check terminal again: the turn may have completed between our two reads
    let turn_is_terminal = turn_status::is_terminal(&turn.status);
    // The baseline sequence is the snapshot's stream_seq. Any live event with
    // seq > baseline_seq is newer than the snapshot and should be forwarded.
    let baseline_seq = turn.runtime_state.stream_seq;
    let payload = snapshot_payload(&turn);

    let s = stream! {
        // Step 4: Emit the snapshot as the first SSE event.
        // This is the authoritative current state of the turn.
        yield Ok(Event::default().event(stream_event::TURN_SNAPSHOT).data(payload.to_string()));

        // Step 5 & 6: Forward live events for active turns.
        if !turn_is_terminal {
            let mut rx = live_rx.expect("active turn stream must have a receiver");
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        // Skip events that are already reflected in the snapshot.
                        // This can happen when events were buffered in the
                        // broadcast channel before we read the snapshot.
                        if !should_forward_live_event(ev.seq, baseline_seq) {
                            continue;
                        }
                        let out = live_event_payload(ev.seq, ev.payload);
                        yield Ok(Event::default().event(ev.event.clone()).data(out.to_string()));
                        // Terminal event (turn_completed or turn_failed):
                        // no more events will follow, so close the stream.
                        if stream_event::is_terminal(&ev.event) {
                            break;
                        }
                    }
                    // Lagged: the consumer was too slow and some events were
                    // dropped from the broadcast buffer. This is acceptable:
                    // the subscriber may miss intermediate events but will
                    // remain connected. On reconnect, it will get a fresh
                    // snapshot that includes whatever was missed.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    // Closed: the broadcast sender was dropped (hub cleaned up
                    // the channel after a terminal event or zero subscribers).
                    // No more events will come, so close the stream.
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    };

    // Wrap the stream in SSE with keep-alive to prevent connection timeouts
    Ok(Sse::new(s).keep_alive(KeepAlive::default()))
}
