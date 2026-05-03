//! Persisted runtime control state for the turn lifecycle.
//!
//! This module defines the strongly-typed state machine that drives the
//! multi-round turn loop, the canonical status values stored in `turns.status`,
//! and the nine SSE event names emitted by the backend.
//!
//! # Why this exists as a separate module
//!
//! The turn lifecycle spans multiple async boundaries:
//! - `POST /turns` creates the turn and returns immediately
//! - a spawned background task runs the multi-round LLM loop
//! - the loop may pause for user approval (a separate `POST /approve` resumes it)
//! - SSE subscribers connect and disconnect at arbitrary times
//!
//! Each of these boundaries needs a shared, durable view of "where is this turn
//! right now?" That is what `TurnRuntimeState` provides. It is serialized into
//! the `turns.runtime_state` JSONB column and loaded on every lifecycle
//! transition.
//!
//! # What goes here vs. what does NOT
//!
//! **Stored here:** execution control state that the backend needs to safely
//! continue, resume, or reconnect a turn (sequence counter, pending tool calls,
//! approval decision history).
//!
//! **NOT stored here:** user-facing transcript content (that lives in
//! `turn_messages`), token usage, assistant text, or errors (those are top-level
//! turn columns). SSE event history is ephemeral and lives in memory inside
//! `TurnStreamHub` -- it is never persisted.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::tooling::PendingToolCall;

/// JSON keys used under `sessions.preferences`.
///
/// Session preferences is a JSONB column on the `sessions` table that stores
/// user-level configuration. The tool-approval system uses it to persist
/// allow-rules when the user chooses "AllowAlways".
pub mod session_preference_key {
    /// User-approved tool allow-rules (for example `Bash(cargo check *)`).
    ///
    /// Stored as a JSON array of rule strings. The `derive_allow_rule` and
    /// `match_allow_rule` functions in `tooling.rs` handle rule format and
    /// matching.
    pub const TOOL_ALLOW_RULES: &str = "tool_allow_rules";
}

/// Canonical turn status values persisted in `turns.status`.
///
/// These four values form a simple state machine:
///
/// ```text
///   RUNNING ──► COMPLETED
///        │           ▲
///        │           │
///        ▼           │
///   AWAITING_APPROVAL
///        │
///        ▼
///      FAILED
/// ```
///
/// Transitions:
/// - `RUNNING` -> `RUNNING`: each round of the loop stays in RUNNING
/// - `RUNNING` -> `AWAITING_APPROVAL`: tool calls need user decision
/// - `RUNNING` -> `COMPLETED`: model returns no tool calls (done)
/// - `RUNNING` -> `FAILED`: provider error, cancellation, loop-limit
/// - `AWAITING_APPROVAL` -> `RUNNING`: all pending calls resolved, loop resumes
/// - `AWAITING_APPROVAL` -> `AWAITING_APPROVAL`: partial approval (some calls
///   resolved, others still pending)
/// - `AWAITING_APPROVAL` -> `FAILED`: cancelled while awaiting
///
/// `COMPLETED` and `FAILED` are terminal -- once reached, no further
/// transitions occur and the background loop stops.
pub mod status {
    /// Turn is paused waiting for the user to approve or deny tool calls.
    ///
    /// In this state `runtime_state.pending_tool_calls` is non-empty and the
    /// background loop has returned. A `POST /approve` request is needed to
    /// resume.
    pub const AWAITING_APPROVAL: &str = "awaiting_approval";
    /// Turn finished successfully. Terminal state.
    ///
    /// The last assistant response had no tool calls, so the loop ended.
    /// `turn_messages` contains the full transcript.
    pub const COMPLETED: &str = "completed";
    /// Turn ended in failure. Terminal state.
    ///
    /// The `error` column contains a structured JSON object with `kind` and
    /// `message`. Causes include: provider error, user cancellation,
    /// loop-limit exhaustion, and tool-execution failures that surface as
    /// loop errors.
    pub const FAILED: &str = "failed";
    /// Turn is actively being processed by the background loop.
    ///
    /// The loop is either waiting for the LLM response, executing tool calls,
    /// or about to start the next round. This is the initial status set when
    /// a turn is created or retried.
    pub const RUNNING: &str = "running";

    /// Returns whether the status is terminal (no more loop progress expected).
    ///
    /// Terminal statuses are used in two places:
    /// 1. The SSE handler uses this to decide whether to subscribe to live
    ///    events (terminal turns just emit a snapshot and close)
    /// 2. The lifecycle uses this as a CAS guard -- `update_turn_if_active`
    ///    will reject updates if the turn has already reached a terminal state
    #[inline]
    pub fn is_terminal(value: &str) -> bool {
        matches!(value, COMPLETED | FAILED)
    }
}

/// Canonical SSE event names emitted by the backend.
///
/// Nine event types cover the full turn lifecycle from creation to termination.
/// They are designed so the frontend can reconstruct the current turn state
/// from the initial `turn_snapshot` and then apply live deltas for any events
/// with `seq > baseline_seq`.
///
/// # Emission ordering invariant
///
/// Every live event is published **after** the corresponding database state has
/// been persisted. This means:
/// 1. The DB write (with bumped `stream_seq`) completes first
/// 2. Then the SSE event is published through `TurnStreamHub`
///
/// This ordering is critical: it guarantees that any event a subscriber
/// receives refers to state that is already durable. A subscriber that reads
/// the turn snapshot after subscribing will always see a `stream_seq` >= the
/// `seq` of any event it may have just missed.
///
/// # Not every event is a transcript entry
///
/// Some events represent lifecycle transitions (e.g. `turn_started`,
/// `round_started`, `approval_needed`) rather than new transcript content.
/// The frontend should use these for UI state changes (spinner, approval modal)
/// but should not try to append them to the transcript.
pub mod stream_event {
    /// Emitted when a loop round discovers pending tool calls that require user
    /// decisions; the turn status is transitioned to `awaiting_approval`.
    ///
    /// Payload: `{ "pending": [PendingToolCall, ...] }`
    ///
    /// After this event, the background loop returns and waits for a
    /// `POST /approve` call. The pending calls are persisted in
    /// `runtime_state.pending_tool_calls` so they survive process restarts.
    pub const APPROVAL_NEEDED: &str = "approval_needed";
    /// Emitted right after one assistant response entry is persisted into
    /// `turn_messages` for the current round.
    ///
    /// Payload: `{ "entry": ..., "assistant_text": ..., "input_tokens": ..., ... }`
    ///
    /// The `entry` field contains the full protocol-native assistant transcript
    /// entry, which may include plain text, reasoning/thinking blocks, and tool
    /// call blocks. The additional fields let the frontend update UI state
    /// (assistant text display, token counters) without parsing the entry.
    pub const ASSISTANT_ENTRY_APPENDED: &str = "assistant_entry_appended";
    /// Emitted at the start of each LLM call round inside the background loop.
    ///
    /// Payload: `{ "round": N }` (1-indexed round number)
    ///
    /// Note: in the current implementation this is published *after* the DB
    /// write that stores the assistant entry, so subscribers usually see it
    /// immediately before `assistant_entry_appended`. The name reflects intent
    /// ("round is starting") but the actual emission point is co-located with
    /// the persistence step for atomicity.
    pub const ROUND_STARTED: &str = "round_started";
    /// Emitted after tool calls are selected for execution (auto-allowed or
    /// user-approved), but before their results are appended.
    ///
    /// Payload: `{ "calls": [{ "call_id": ..., "name": ..., "input": ... }, ...] }`
    ///
    /// This lets the frontend show "executing tools..." state before results
    /// arrive.
    pub const TOOL_CALLS: &str = "tool_calls";
    /// Emitted after one tool-result transcript entry is persisted (including
    /// both real tool output and synthetic denied/unknown-tool outputs).
    ///
    /// Payload: `{ "entry": ... }` (the full protocol-native tool result entry)
    ///
    /// The entry contains results for ALL tool calls that were executed in this
    /// batch, not just one. The frontend should replace the tool-execution
    /// spinner with the results.
    pub const TOOL_RESULT_APPENDED: &str = "tool_result_appended";
    /// Emitted when the turn finishes successfully and status is persisted as
    /// `completed`. Terminal event -- the SSE stream closes after this.
    ///
    /// Payload: `{ "assistant_text": ..., "input_tokens": ..., ... }`
    pub const TURN_COMPLETED: &str = "turn_completed";
    /// Emitted when the turn enters a terminal failed state (provider error,
    /// cancellation, loop-limit, or other lifecycle failure). Terminal event --
    /// the SSE stream closes after this.
    ///
    /// Payload: `{ "error": { "kind": ..., "message": ... } }`
    pub const TURN_FAILED: &str = "turn_failed";
    /// Emitted exactly once when a turn row is initialized (create/retry path)
    /// and the background loop is about to start.
    ///
    /// Payload: `{}` (empty)
    ///
    /// This is emitted after the initial user transcript entry has been
    /// persisted and before the background loop makes its first LLM call.
    pub const TURN_STARTED: &str = "turn_started";
    /// Emitted by the SSE handler immediately after subscribe with the latest
    /// persisted full turn snapshot.
    ///
    /// Payload: the full `snapshot_payload` (see `handlers/turns.rs`)
    ///
    /// This is NOT emitted by the background loop. It is synthesized by the
    /// SSE handler to give the subscriber a complete starting state. All
    /// subsequent live events have `seq > snapshot.stream_seq`.
    pub const TURN_SNAPSHOT: &str = "turn_snapshot";

    /// Returns whether the stream event closes the live stream lifecycle.
    ///
    /// When a terminal event is forwarded to the SSE subscriber, the stream
    /// loop breaks and the connection closes. The `TurnStreamHub` actor also
    /// uses this to clean up the per-turn broadcast channel.
    #[inline]
    pub fn is_terminal(value: &str) -> bool {
        matches!(value, TURN_COMPLETED | TURN_FAILED)
    }
}

/// Persisted decision kind for one previously approved/rejected pending call.
///
/// These are recorded in `runtime_state.approval_decisions` keyed by
/// `pending_call_id`. They serve as an idempotency log: if the frontend
/// retries an approval request, the backend can check whether the exact same
/// decision was already recorded and respond safely instead of rejecting it
/// as a conflict.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecordedApprovalDecisionKind {
    /// Execute once -- no rule is persisted, the default policy remains unchanged.
    Allow,
    /// Execute and persist an allow-rule -- future matching calls are auto-approved.
    ///
    /// The rule is derived from the tool name and input (see `derive_allow_rule`
    /// in `tooling.rs`) and stored in `session.preferences.tool_allow_rules`.
    AllowAlways,
    /// Reject call -- a synthetic error result is sent back to the model.
    ///
    /// The model sees `"Denied by user"` as the tool output with `is_error: true`.
    /// This gives the model a chance to adjust its behavior in subsequent rounds.
    Deny,
}

/// Strongly-typed in-flight runtime state persisted per turn.
///
/// This struct is serialized into the `turns.runtime_state` JSONB column. It
/// only stores durable execution state that must survive reconnects or approval
/// round-trips; transient SSE history stays in memory inside `TurnStreamHub`.
///
/// # Design rationale
///
/// We use a single typed JSONB blob rather than separate columns because:
/// - these fields are always loaded together with the turn row
/// - they evolve with the lifecycle implementation, not with query/reporting needs
/// - we do not need SQL-level filtering on individual runtime fields
///
/// # Relationship to the turn row
///
/// Top-level turn columns store the user-facing product state:
/// `status`, `turn_messages`, `assistant_text`, token usage, `error`.
///
/// `runtime_state` stores the extra runtime control state the backend needs to
/// safely continue, resume, or reconnect the turn while it is in flight:
/// `stream_seq`, `pending_tool_calls`, `approval_decisions`.
///
/// It is NOT a second transcript and NOT a replay log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct TurnRuntimeState {
    /// Historical approval decisions keyed by `pending_call_id`.
    ///
    /// This map accumulates entries over the lifetime of a turn and is never
    /// pruned. Its purpose is **idempotency**: if the frontend retries an
    /// approval request (e.g. due to network flakiness), the backend can check
    /// whether the exact same decision was already recorded and return the
    /// current turn state instead of rejecting the request as a conflict.
    ///
    /// Example: `{ "pcall_abc123": "allow", "pcall_def456": "deny" }`
    pub approval_decisions: HashMap<String, RecordedApprovalDecisionKind>,
    /// Pending tool calls currently waiting for user approval.
    ///
    /// Filled when the loop discovers tool calls that require approval and
    /// transitions the turn to `awaiting_approval`. Cleared when those calls
    /// are resolved, or when the turn reaches a terminal state.
    ///
    /// This is persisted (rather than kept only in memory) because approval is
    /// not handled inside one long-running stack frame. The loop persists the
    /// pending calls, returns, and later a separate `POST /approve` request
    /// reloads the row and continues from those exact calls. This means:
    /// - if the server restarts between the approval request and the response,
    ///   the pending calls are still available
    /// - if the user refreshes the page, the pending calls can be shown again
    pub pending_tool_calls: Vec<PendingToolCall>,
    /// Monotonic stream sequence counter -- the core of the snapshot/live
    /// ordering mechanism.
    ///
    /// Every time the backend makes a stream-visible state transition durable
    /// (persisting to the DB), it bumps this counter first. The SSE handler
    /// reads the turn snapshot to get the current `stream_seq` (the "baseline"),
    /// then forwards only live events with `seq > baseline_seq`.
    ///
    /// This solves the fundamental race condition: what if an event is emitted
    /// between reading the snapshot and subscribing to live events?
    /// - If the event landed BEFORE the snapshot read: the snapshot includes it
    ///   (because the DB write completed before the read)
    /// - If the event landed AFTER the snapshot read: its `seq > baseline_seq`
    ///   and it is forwarded live
    /// - If the event landed exactly during the read: the subscribe-first
    ///   ordering in the SSE handler ensures it arrives in the live channel
    ///
    /// Important: `stream_seq` does NOT count transcript messages. It counts
    /// backend lifecycle transitions that matter to the stream. A turn with
    /// only 4 transcript entries can easily end with `stream_seq = 9` because
    /// each round emits multiple events (round_started, assistant_entry_appended,
    /// tool_calls, tool_result_appended, etc.).
    pub stream_seq: u64,
}

impl TurnRuntimeState {
    /// Returns a copy with replaced pending tool-call list.
    ///
    /// Used when transitioning to `awaiting_approval` (to set the pending list)
    /// and when clearing pending calls after approval or completion.
    pub fn with_pending(&self, pending_tool_calls: Vec<PendingToolCall>) -> Self {
        let mut out = self.clone();
        out.pending_tool_calls = pending_tool_calls;
        out
    }

    /// Returns a copy with all pending tool calls removed.
    ///
    /// Called when the turn reaches a terminal state (completed/failed) or
    /// when auto-approved calls are being processed and we want to clear any
    /// stale pending list from a previous approval pause.
    pub fn clear_pending(&self) -> Self {
        self.with_pending(Vec::new())
    }

    /// Returns a copy with replaced approval decision map.
    ///
    /// Called during approval processing to merge new decisions into the
    /// accumulated history.
    pub fn with_approval_decisions(
        &self,
        approval_decisions: HashMap<String, RecordedApprovalDecisionKind>,
    ) -> Self {
        let mut out = self.clone();
        out.approval_decisions = approval_decisions;
        out
    }

    /// Issues the next monotonic stream sequence number.
    ///
    /// Returns a new `TurnRuntimeState` with `stream_seq` incremented by 1,
    /// along with the new sequence value.
    ///
    /// # Why the sequence is persisted (not just in-memory)
    ///
    /// A reconnecting SSE client needs a durable baseline. The flow is:
    /// 1. Client subscribes to live events (in-memory broadcast channel)
    /// 2. Client reads the turn snapshot from DB (gets `stream_seq` as baseline)
    /// 3. Client emits the snapshot event
    /// 4. Client forwards only live events with `seq > baseline_seq`
    ///
    /// If `stream_seq` were only in memory, step 2 would have no way to know
    /// which events were already reflected in the snapshot. By persisting it
    /// in the same DB transaction as the state update, we get an atomic
    /// boundary between "in the snapshot" and "must be delivered live".
    ///
    /// # Usage pattern
    ///
    /// Every persistent state transition in the lifecycle follows this pattern:
    /// ```ignore
    /// let (next_state, new_seq) = runtime_state.bump_stream_seq();
    /// update_turn_if_active(..., runtime_state: Some(&next_state), ...).await?;
    /// publish_stream_event(state, turn_id, new_seq, EVENT_NAME, payload).await;
    /// ```
    /// The DB write happens BEFORE the event is published, ensuring the
    /// write-before-publish invariant.
    pub fn bump_stream_seq(&self) -> (Self, u64) {
        let mut out = self.clone();
        let next_seq = out.stream_seq + 1;
        out.stream_seq = next_seq;
        (out, next_seq)
    }
}
