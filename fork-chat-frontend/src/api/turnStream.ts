/**
 * Turn status constants and SSE event type definitions.
 *
 * These constants are shared across the frontend to avoid stringly-typed
 * comparisons. The values must match the backend's Rust enum serialization
 * exactly (both are lowercase snake_case).
 */

/**
 * All possible states a turn can be in during its lifecycle.
 *
 * State machine:
 *   running -> awaiting_approval -> running -> ... -> completed
 *   running -> failed
 *   awaiting_approval -> failed
 */
export const TURN_STATUS = {
  /** The turn loop is actively running (making LLM API calls, executing tools, etc.). */
  RUNNING: 'running',
  /** The turn loop paused because a tool requires human approval before execution. */
  AWAITING_APPROVAL: 'awaiting_approval',
  /** The turn finished successfully. Terminal state. */
  COMPLETED: 'completed',
  /** The turn failed due to an error (LLM API error, timeout, etc.). Terminal state. */
  FAILED: 'failed',
} as const;

/** Union type derived from the TURN_STATUS constant values. */
export type TurnStatus = (typeof TURN_STATUS)[keyof typeof TURN_STATUS];

/**
 * SSE event type names sent by the backend during turn streaming.
 *
 * The server pushes these events over the SSE connection established at
 * `/api/sessions/{id}/turns/{turn_id}/stream`.  Each event carries a JSON
 * payload with incremental turn data.
 */
export const TURN_STREAM_EVENT = {
  /** Full snapshot of the turn state. Sent on initial connect and after
   *  reconnections so the client can catch up from any prior state. */
  TURN_SNAPSHOT: 'turn_snapshot',
  /** The turn loop has started (initial event after creating a turn). */
  TURN_STARTED: 'turn_started',
  /** A new LLM round has begun within the turn (the turn loop may execute
   *  multiple rounds if tools are called and their results are fed back). */
  ROUND_STARTED: 'round_started',
  /** A new assistant message entry has been appended to the transcript.
   *  Contains the raw protocol-native entry and an optional extracted
   *  assistant_text string. */
  ASSISTANT_ENTRY_APPENDED: 'assistant_entry_appended',
  /** A new tool result entry has been appended to the transcript. */
  TOOL_RESULT_APPENDED: 'tool_result_appended',
  /** The turn loop paused because one or more tool calls require human
   *  approval. The payload contains the pending tool call details. */
  APPROVAL_NEEDED: 'approval_needed',
  /** Intermediate tool call information (tool name, arguments) streamed
   *  before the tool is actually executed. Used for showing "calling tool..."
   *  indicators in the UI. */
  TOOL_CALLS: 'tool_calls',
  /** The turn completed successfully. Terminal event — closes the SSE stream. */
  TURN_COMPLETED: 'turn_completed',
  /** The turn failed with an error. Terminal event — closes the SSE stream. */
  TURN_FAILED: 'turn_failed',
} as const;

/**
 * Well-known keys inside the `turn.runtime_state` JSON object.
 *
 * `runtime_state` is a flexible JSON blob that the backend populates during
 * the turn lifecycle. These constants avoid typos when accessing specific keys.
 */
export const TURN_RUNTIME_KEY = {
  /** Key for the array of pending tool calls awaiting human approval.
   *  Each entry has: pending_call_id, call_id, name, input. */
  PENDING_TOOL_CALLS: 'pending_tool_calls',
} as const;

/**
 * Returns true if the turn is in an active (non-terminal) state where the SSE
 * stream should remain open.
 *
 * Both `running` and `awaiting_approval` are considered "streaming" because:
 * - In `running` state, the backend is actively pushing incremental events.
 * - In `awaiting_approval` state, the SSE connection must stay open so that
 *   when the user approves and the turn resumes, the client receives the
 *   subsequent events without needing to re-establish the connection.
 *
 * This distinction is important: the SSE hub on the backend is keyed by
 * `turn_id` and spans the entire turn lifecycle (including approval pauses),
 * so the client should NOT tear down the connection on `awaiting_approval`.
 */
export function isStreamingTurnStatus(
  status: TurnStatus | null | undefined,
): boolean {
  return (
    status === TURN_STATUS.RUNNING || status === TURN_STATUS.AWAITING_APPROVAL
  );
}

/**
 * Returns true if the turn has reached a terminal state where the SSE stream
 * should be closed.
 *
 * `completed` and `failed` are terminal because the backend will not send any
 * more events for this turn. The SSE connection should be closed to free
 * resources.
 */
export function isTerminalTurnStatus(
  status: TurnStatus | null | undefined,
): boolean {
  return status === TURN_STATUS.COMPLETED || status === TURN_STATUS.FAILED;
}
