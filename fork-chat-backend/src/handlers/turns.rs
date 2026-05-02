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
    pub parent_turn_id: Option<Uuid>,
    /// User prompt text for the new turn.
    pub user_text: String,
    /// Selected provider name.
    pub provider: String,
    /// Selected model id.
    pub model: String,
}

/// HTTP payload for retrying an existing turn.
#[derive(Debug, Deserialize)]
pub struct RetryTurnRequest {
    /// Selected provider name.
    pub provider: String,
    /// Selected model id.
    pub model: String,
}

/// HTTP payload for approval submission.
#[derive(Debug, Deserialize)]
pub struct ApproveTurnRequest {
    /// One or more decisions for pending tool calls.
    pub decisions: Vec<ApproveDecisionRequest>,
}

/// One user decision for one pending call id.
#[derive(Debug, Deserialize)]
pub struct ApproveDecisionRequest {
    /// Pending call id (`pcall_*`) displayed in UI.
    pub pending_call_id: String,
    /// Decision kind chosen by the user.
    pub decision: DecisionKind,
}

/// Serializable decision kinds accepted by the API.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    /// Execute once.
    Allow,
    /// Execute and persist an allow-rule.
    AllowAlways,
    /// Reject and synthesize an error result.
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
#[derive(Debug, Serialize)]
pub struct TurnResponse {
    /// Updated turn row.
    pub turn: Turn,
}

/// Response payload for creation-style endpoints.
#[derive(Debug, Serialize)]
pub struct CreateTurnResponse {
    /// Newly created turn row.
    pub turn: Turn,
}

/// Response payload for tree endpoint.
#[derive(Debug, Serialize)]
pub struct TreeResponse {
    /// All turns belonging to a session ordered by creation time.
    pub turns: Vec<Turn>,
}

/// Builds the full turn snapshot payload sent on every SSE subscription.
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
fn live_event_payload(seq: u64, payload: serde_json::Value) -> serde_json::Value {
    json!({
        "seq": seq,
        "payload": payload,
    })
}

/// Returns whether one live event should be forwarded after a snapshot whose
/// state already reflects `baseline_seq`.
fn should_forward_live_event(seq: u64, baseline_seq: u64) -> bool {
    seq > baseline_seq
}

/// Creates a turn and starts the background lifecycle loop.
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
pub async fn get_turn_handler(
    State(state): State<AppState>,
    Path((session_id, turn_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<TurnResponse>, AppError> {
    let turn = get_turn_in_session(&state.db, session_id, turn_id).await?;
    Ok(Json(TurnResponse { turn }))
}

/// Returns all turns in one session for tree rendering.
pub async fn get_session_tree_handler(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<TreeResponse>, AppError> {
    get_session(&state.db, session_id).await?;
    let turns = get_session_tree(&state.db, session_id).await?;
    Ok(Json(TreeResponse { turns }))
}

/// Retries a turn by creating a new sibling branch and restarting execution.
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
pub async fn approve_turn_handler(
    State(state): State<AppState>,
    Path((session_id, turn_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<ApproveTurnRequest>,
) -> Result<Json<TurnResponse>, AppError> {
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
pub async fn cancel_turn_handler(
    State(state): State<AppState>,
    Path((session_id, turn_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<TurnResponse>, AppError> {
    let service = TurnLifecycleService::new(state);
    let turn = service.cancel_turn(session_id, turn_id).await?;
    Ok(Json(TurnResponse { turn }))
}

/// Streams one turn's latest snapshot followed by live updates over SSE.
pub async fn stream_turn_handler(
    State(state): State<AppState>,
    Path((session_id, turn_id)): Path<(Uuid, Uuid)>,
) -> Result<Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>>, AppError> {
    let initial_turn = get_turn_in_session(&state.db, session_id, turn_id).await?;
    let mut live_rx = None;
    let turn = if turn_status::is_terminal(&initial_turn.status) {
        initial_turn
    } else {
        live_rx = Some(state.turn_stream_hub.subscribe(turn_id).await);
        get_turn_in_session(&state.db, session_id, turn_id).await?
    };
    let turn_is_terminal = turn_status::is_terminal(&turn.status);
    let baseline_seq = turn.runtime_state.stream_seq;
    let payload = snapshot_payload(&turn);

    let s = stream! {
        yield Ok(Event::default().event(stream_event::TURN_SNAPSHOT).data(payload.to_string()));
        if !turn_is_terminal {
            let mut rx = live_rx.expect("active turn stream must have a receiver");
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        if !should_forward_live_event(ev.seq, baseline_seq) {
                            continue;
                        }
                        let out = live_event_payload(ev.seq, ev.payload);
                        yield Ok(Event::default().event(ev.event.clone()).data(out.to_string()));
                        if stream_event::is_terminal(&ev.event) {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    };

    Ok(Sse::new(s).keep_alive(KeepAlive::default()))
}
