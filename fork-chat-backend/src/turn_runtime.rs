use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::tooling::PendingToolCall;

/// JSON keys used under `sessions.preferences`.
pub mod session_preference_key {
    /// User-approved tool allow-rules (for example `Bash(cargo check *)`).
    pub const TOOL_ALLOW_RULES: &str = "tool_allow_rules";
}

/// Canonical turn status values persisted in `turns.status`.
pub mod status {
    pub const AWAITING_APPROVAL: &str = "awaiting_approval";
    pub const COMPLETED: &str = "completed";
    pub const FAILED: &str = "failed";
    pub const RUNNING: &str = "running";

    /// Returns whether the status is terminal (no more loop progress expected).
    pub fn is_terminal(value: &str) -> bool {
        matches!(value, COMPLETED | FAILED)
    }
}

/// Canonical SSE event names emitted by the backend.
pub mod stream_event {
    /// Emitted when a loop round discovers pending tool calls that require user
    /// decisions; the turn status is transitioned to `awaiting_approval`.
    pub const APPROVAL_NEEDED: &str = "approval_needed";
    /// Emitted right after one assistant response entry is persisted into
    /// `turn_messages` for the current round.
    pub const ASSISTANT_ENTRY_APPENDED: &str = "assistant_entry_appended";
    /// Emitted at the start of each LLM call round inside the background loop.
    /// Payload currently includes `{ "round": N }`.
    pub const ROUND_STARTED: &str = "round_started";
    /// Emitted after tool calls are selected for execution (auto-allowed or
    /// user-approved), but before their results are appended.
    pub const TOOL_CALLS: &str = "tool_calls";
    /// Emitted after one tool-result transcript entry is persisted (including
    /// both real tool output and synthetic denied/unknown-tool outputs).
    pub const TOOL_RESULT_APPENDED: &str = "tool_result_appended";
    /// Emitted when the turn finishes successfully and status is persisted as
    /// `completed`.
    pub const TURN_COMPLETED: &str = "turn_completed";
    /// Emitted when the turn enters a terminal failed state (provider error,
    /// cancellation, loop-limit, or other lifecycle failure).
    pub const TURN_FAILED: &str = "turn_failed";
    /// Emitted exactly once when a turn row is initialized (create/retry path)
    /// and the background loop is about to start.
    pub const TURN_STARTED: &str = "turn_started";
    /// Emitted by the SSE handler immediately after subscribe with the latest
    /// persisted full turn snapshot.
    pub const TURN_SNAPSHOT: &str = "turn_snapshot";

    /// Returns whether the stream event closes the live stream lifecycle.
    pub fn is_terminal(value: &str) -> bool {
        matches!(value, TURN_COMPLETED | TURN_FAILED)
    }
}

/// Persisted decision kind for one previously approved/rejected pending call.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecordedApprovalDecisionKind {
    /// Execute once.
    Allow,
    /// Execute and persist allow-rule.
    AllowAlways,
    /// Reject call and synthesize denied output.
    Deny,
}

/// Strongly-typed in-flight runtime state persisted per turn.
///
/// This struct is serialized into the existing `turns.runtime_state` jsonb
/// column and replaces ad-hoc key/value access across the codebase. It only
/// stores durable execution state that must survive reconnects or approval
/// round-trips; transient SSE history stays in memory inside `TurnStreamHub`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct TurnRuntimeState {
    /// Historical approval decisions keyed by `pending_call_id`.
    pub approval_decisions: HashMap<String, RecordedApprovalDecisionKind>,
    /// Pending tool calls currently waiting for user approval.
    pub pending_tool_calls: Vec<PendingToolCall>,
    /// Last issued stream sequence id for this turn.
    pub stream_seq: u64,
}

impl TurnRuntimeState {
    /// Returns a copy with replaced pending tool-call list.
    pub fn with_pending(&self, pending_tool_calls: Vec<PendingToolCall>) -> Self {
        let mut out = self.clone();
        out.pending_tool_calls = pending_tool_calls;
        out
    }

    /// Returns a copy with all pending tool calls removed.
    pub fn clear_pending(&self) -> Self {
        self.with_pending(Vec::new())
    }

    /// Returns a copy with replaced approval decision map.
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
    /// The sequence is persisted so a reconnecting SSE client can accept a
    /// fresh snapshot, then ignore any live event whose `seq` is already
    /// reflected in that snapshot.
    pub fn bump_stream_seq(&self) -> (Self, u64) {
        let mut out = self.clone();
        let next_seq = out.stream_seq + 1;
        out.stream_seq = next_seq;
        (out, next_seq)
    }
}
