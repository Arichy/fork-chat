use serde_json::Value as JsonValue;
use sqlx::PgPool;
use sqlx::types::Json;
use uuid::Uuid;

use crate::error::{AppError, Result};
use crate::models::Turn;
use crate::turn_runtime::TurnRuntimeState;
use crate::turn_runtime::status as turn_status;

/// Check whether a session already has a root turn.
///
/// A session should have at most one root turn (parent_turn_id IS NULL).
/// This check is used during turn creation to enforce that invariant --
/// the first turn in a session becomes the root, and all subsequent turns
/// must specify a parent.
pub async fn session_has_root_turn(db: &PgPool, session_id: Uuid) -> Result<bool> {
    let result = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM turns WHERE session_id = $1 AND parent_turn_id IS NULL)",
    )
    .bind(session_id)
    .fetch_one(db)
    .await
    .map_err(|e| AppError::DatabaseError(format!("Failed to check root turn: {}", e)))?;
    Ok(result)
}

/// Create a new turn node in the conversation tree.
///
/// The turn is inserted as a child of `parent_turn_id` (or as the root if
/// `parent_turn_id` is `None`). `turn_messages` is initialized to an empty
/// JSONB array (`'[]'::jsonb`) because the transcript is built incrementally:
/// the user message is appended when the turn starts, and assistant/tool
/// messages are appended as the LLM interaction progresses. Starting empty
/// rather than pre-populating keeps the INSERT simple and lets the streaming
/// lifecycle populate messages in a separate UPDATE.
pub async fn create_turn(
    db: &PgPool,
    session_id: Uuid,
    parent_turn_id: Option<Uuid>,
    status: &str,
    user_text: &str,
) -> Result<Turn> {
    sqlx::query_as::<_, Turn>(
        r#"
        INSERT INTO turns (session_id, parent_turn_id, status, user_text, turn_messages)
        VALUES ($1, $2, $3, $4, '[]'::jsonb)
        RETURNING *
        "#,
    )
    .bind(session_id)
    .bind(parent_turn_id)
    .bind(status)
    .bind(user_text)
    .fetch_one(db)
    .await
    .map_err(|e| AppError::DatabaseError(format!("Failed to create turn: {}", e)))
}

/// Parameters for updating an existing turn.
///
/// All fields are provided on every update call, but some are optional to
/// allow partial updates:
/// - `runtime_state`: `None` means "don't touch it" (preserved via COALESCE).
///   This is critical during streaming updates where the caller may only want
///   to update `turn_messages` without overwriting `pending_tool_calls` or
///   `stream_seq` that were set by a different code path.
/// - `assistant_text`, `response_id`, `input_tokens`, `output_tokens`,
///   `cached_tokens`, `error`, `retry_turn_id`: genuinely nullable DB columns.
pub struct UpdateTurnParams<'a> {
    /// New lifecycle status for the turn.
    pub status: &'a str,
    /// Final assistant text response. `None` means the turn hasn't produced text yet.
    pub assistant_text: Option<&'a str>,
    /// Full protocol-native transcript to replace the existing one.
    /// Always provided (not optional) because the transcript is rebuilt from
    /// scratch on each streaming update -- incremental appends would require
    /// JSONB array manipulation that's harder to reason about.
    pub turn_messages: &'a JsonValue,
    /// Updated runtime control state. `None` means "keep existing" (COALESCE).
    pub runtime_state: Option<&'a TurnRuntimeState>,
    /// OpenAI Responses API response.id. `None` for non-OpenAI sessions or
    /// turns that haven't received a response yet.
    pub response_id: Option<&'a str>,
    /// LLM provider name (e.g. "openai", "anthropic").
    pub provider: &'a str,
    /// Model identifier (e.g. "gpt-4o", "claude-sonnet-4-20250514").
    pub model: &'a str,
    /// Input token count for this turn's LLM call.
    pub input_tokens: Option<i32>,
    /// Output token count for this turn's LLM call.
    pub output_tokens: Option<i32>,
    /// Cached token count (provider-specific prompt cache savings).
    pub cached_tokens: Option<i32>,
    /// Structured error info when the turn failed.
    pub error: Option<&'a JsonValue>,
    /// Set on the *original* turn to point to the retry replacement.
    pub retry_turn_id: Option<Uuid>,
}

/// Unconditionally update a turn's fields.
///
/// Use this when you have exclusive ownership of the turn (e.g. the initial
/// turn creation flow). For concurrent scenarios (streaming, approval
/// round-trips), prefer `update_turn_if_active` which adds a CAS-style guard.
///
/// The `COALESCE($5, runtime_state)` pattern allows partial updates: when
/// `runtime_state` is `None`, the existing DB value is preserved. This is
/// essential because multiple code paths update different aspects of the turn
/// during streaming (e.g. one path updates `turn_messages` while another
/// updates `pending_tool_calls` in `runtime_state`), and we don't want a
/// later update to accidentally wipe runtime state set by an earlier one.
///
/// The `completed_at` column is automatically set to `now()` when status
/// transitions to a terminal state (`completed` or `failed`), and cleared
/// (set to NULL) otherwise. This ensures `completed_at` is always consistent
/// with the status column without requiring the caller to manage it.
pub async fn update_turn(db: &PgPool, id: Uuid, params: UpdateTurnParams<'_>) -> Result<Turn> {
    sqlx::query_as::<_, Turn>(
        r#"
        UPDATE turns SET
            status = $2,
            assistant_text = $3,
            turn_messages = $4,
            runtime_state = COALESCE($5, runtime_state),
            response_id = $6,
            provider = $7,
            model = $8,
            input_tokens = $9,
            output_tokens = $10,
            cached_tokens = $11,
            error = $12,
            retry_turn_id = $13,
            completed_at = CASE WHEN $2 = $14 OR $2 = $15 THEN now() ELSE NULL END
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(id)
    .bind(params.status)
    .bind(params.assistant_text)
    .bind(params.turn_messages)
    // Wrap in sqlx::types::Json so sqlx serializes the Rust struct to JSONB.
    // COALESCE on the SQL side means: if $5 is NULL, keep the existing value.
    .bind(params.runtime_state.cloned().map(Json))
    .bind(params.response_id)
    .bind(params.provider)
    .bind(params.model)
    .bind(params.input_tokens)
    .bind(params.output_tokens)
    .bind(params.cached_tokens)
    .bind(params.error)
    .bind(params.retry_turn_id)
    .bind(turn_status::COMPLETED)
    .bind(turn_status::FAILED)
    .fetch_one(db)
    .await
    .map_err(|e| AppError::DatabaseError(format!("Failed to update turn: {}", e)))
}

/// Conditionally update a turn only when it is still in an active status
/// (`running` or `awaiting_approval`).
///
/// This is a **compare-and-swap (CAS) guard** that prevents stale background
/// workers from overwriting terminal rows after cancellation or concurrent
/// lifecycle transitions. Without this guard, the following race condition
/// could occur:
///
/// 1. Worker A starts streaming, turn is `running`.
/// 2. User cancels the request; Worker B sets turn to `failed`.
/// 3. Worker A (still running, unaware of cancellation) finishes and tries
///    to set turn to `completed`.
/// 4. Without the guard, Worker A would overwrite Worker B's `failed` status,
///    making the turn appear successful despite the user's cancellation.
///
/// The `WHERE status IN ('running', 'awaiting_approval')` clause ensures the
/// UPDATE is a no-op if the turn has already reached a terminal state. Returns
/// `None` when the guard rejected the update, signaling the caller that it
/// lost the race.
///
/// This is critical because background tasks can be cancelled at any time
/// (client disconnect, server shutdown, user-initiated abort), and the
/// cancellation handler may have already transitioned the turn to `failed`.
/// The streaming worker must not be able to resurrect a dead turn.
pub async fn update_turn_if_active(
    db: &PgPool,
    id: Uuid,
    params: UpdateTurnParams<'_>,
) -> Result<Option<Turn>> {
    sqlx::query_as::<_, Turn>(
        r#"
        UPDATE turns SET
            status = $2,
            assistant_text = $3,
            turn_messages = $4,
            runtime_state = COALESCE($5, runtime_state),
            response_id = $6,
            provider = $7,
            model = $8,
            input_tokens = $9,
            output_tokens = $10,
            cached_tokens = $11,
            error = $12,
            retry_turn_id = $13,
            completed_at = CASE WHEN $2 = $14 OR $2 = $15 THEN now() ELSE NULL END
        WHERE id = $1
          AND status IN ($16, $17)
        RETURNING *
        "#,
    )
    .bind(id)
    .bind(params.status)
    .bind(params.assistant_text)
    .bind(params.turn_messages)
    .bind(params.runtime_state.cloned().map(Json))
    .bind(params.response_id)
    .bind(params.provider)
    .bind(params.model)
    .bind(params.input_tokens)
    .bind(params.output_tokens)
    .bind(params.cached_tokens)
    .bind(params.error)
    .bind(params.retry_turn_id)
    .bind(turn_status::COMPLETED)
    .bind(turn_status::FAILED)
    // These are the "active" statuses that allow the update to proceed.
    .bind(turn_status::RUNNING)
    .bind(turn_status::AWAITING_APPROVAL)
    // fetch_optional returns None when the WHERE clause didn't match (i.e.
    // the turn was already in a terminal state), signaling a lost CAS race.
    .fetch_optional(db)
    .await
    .map_err(|e| AppError::DatabaseError(format!("Failed to update turn: {}", e)))
}

/// Fetch a single turn by ID, scoped to a specific session.
///
/// The `session_id` check prevents cross-session data leakage -- a caller
/// with a turn ID from one session cannot use it to read turns from another.
pub async fn get_turn_in_session(db: &PgPool, session_id: Uuid, id: Uuid) -> Result<Turn> {
    sqlx::query_as::<_, Turn>("SELECT * FROM turns WHERE id = $1 AND session_id = $2")
        .bind(id)
        .bind(session_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Turn not found in session: {}", id)))
}

/// Reconstruct the full path from root to a given turn node.
///
/// This is THE core tree query. It returns every turn from the root of the
/// conversation tree down to the specified turn, ordered chronologically.
/// The returned path is used as the LLM's conversation context -- the adapter
/// collects `turn_messages` from each turn to build the full message history.
///
/// Uses a **recursive CTE** that walks **upward** from the target turn to the
/// root using `parent_turn_id`:
///
/// ```sql
/// WITH RECURSIVE path AS (
///     -- Base case: start at the target turn (the leaf we're interested in).
///     -- The session_id filter prevents cross-session leakage.
///     SELECT * FROM turns WHERE id = $1 AND session_id = $2
///   UNION ALL
///     -- Recursive step: for each turn already in `path`, find its parent
///     -- by joining ON parent_turn_id. This walks one level up per iteration
///     -- until we reach the root (where parent_turn_id IS NULL and the JOIN
///     -- produces no more rows).
///     SELECT t.* FROM turns t
///     JOIN path p ON t.id = p.parent_turn_id
///     WHERE t.session_id = $2
/// )
/// -- Order by created_at so the path goes root -> ... -> leaf chronologically.
/// -- The LLM needs messages in this order to maintain conversation continuity.
/// SELECT * FROM path ORDER BY created_at ASC
/// ```
///
/// Complexity is O(depth) where depth is the number of turns from root to the
/// target. For typical conversations this is well under 100, making this
/// approach efficient without needing materialized paths or the ltree extension.
///
/// Returns an empty vector when `turn_id` is `None` (e.g. the session has no
/// turns yet, or the caller is requesting context for a new root turn).
pub async fn get_path_to_turn_in_session(
    db: &PgPool,
    session_id: Uuid,
    turn_id: Option<Uuid>,
) -> Result<Vec<Turn>> {
    if let Some(id) = turn_id {
        sqlx::query_as::<_, Turn>(
            r#"
            WITH RECURSIVE path AS (
                SELECT * FROM turns WHERE id = $1 AND session_id = $2
                UNION ALL
                SELECT t.* FROM turns t
                JOIN path p ON t.id = p.parent_turn_id
                WHERE t.session_id = $2
            )
            SELECT * FROM path ORDER BY created_at ASC
            "#,
        )
        .bind(id)
        .bind(session_id)
        .fetch_all(db)
        .await
        .map_err(|e| AppError::DatabaseError(format!("Failed to get path to turn: {}", e)))
    } else {
        // No target turn specified -- return empty context (e.g. new session
        // with no turns yet, or the caller is about to create a root turn).
        Ok(vec![])
    }
}

/// Fetch ALL turns in a session for tree rendering on the frontend.
///
/// Returns every turn node ordered by `created_at` so the frontend can
/// reconstruct the tree structure client-side using `parent_turn_id`.
/// The frontend uses this to render the full conversation tree (showing all
/// branches, forks, and retry chains) rather than just a single path.
///
/// This is a flat query (no CTE needed) because we want ALL nodes, not a
/// specific path. The frontend is responsible for building the tree graph
/// from the flat list using the `parent_turn_id` relationships.
pub async fn get_session_tree(db: &PgPool, session_id: Uuid) -> Result<Vec<Turn>> {
    sqlx::query_as::<_, Turn>("SELECT * FROM turns WHERE session_id = $1 ORDER BY created_at")
        .bind(session_id)
        .fetch_all(db)
        .await
        .map_err(|e| AppError::DatabaseError(format!("Failed to get session tree: {}", e)))
}

/// Mark all turns left in active states as failed.
///
/// Runs **at server startup** to clean up turns that were interrupted by a
/// backend crash or restart. When the server terminates unexpectedly, turns
/// that were `running` (mid-streaming) or `awaiting_approval` (waiting for
/// user action on tool calls) are left dangling in a non-terminal state.
/// Without cleanup, these turns would appear stuck forever in the UI.
///
/// The error is set to `{ kind: "abandoned", message: "..." }` so the UI can
/// distinguish abandoned turns from regular failures and potentially offer a
/// retry action. `completed_at` is set so duration calculations work correctly.
///
/// Returns the number of turns that were transitioned to `failed`.
pub async fn fail_abandoned_turns(db: &PgPool) -> Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE turns
        SET
            status = $1,
            error = jsonb_build_object('kind', 'abandoned', 'message', 'Turn abandoned after backend restart'),
            completed_at = now()
        WHERE status IN ($2, $3)
        "#,
    )
    .bind(turn_status::FAILED)
    // These are the "active" statuses that indicate an in-progress turn
    // was interrupted by the server going down.
    .bind(turn_status::RUNNING)
    .bind(turn_status::AWAITING_APPROVAL)
    .execute(db)
    .await
    .map_err(|e| AppError::DatabaseError(format!("Failed to fail abandoned turns: {}", e)))?;
    Ok(result.rows_affected())
}
