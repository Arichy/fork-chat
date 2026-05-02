export const TURN_STATUS = {
  RUNNING: 'running',
  AWAITING_APPROVAL: 'awaiting_approval',
  COMPLETED: 'completed',
  FAILED: 'failed',
} as const;

export type TurnStatus = (typeof TURN_STATUS)[keyof typeof TURN_STATUS];

export const TURN_STREAM_EVENT = {
  TURN_SNAPSHOT: 'turn_snapshot',
  TURN_STARTED: 'turn_started',
  ROUND_STARTED: 'round_started',
  ASSISTANT_ENTRY_APPENDED: 'assistant_entry_appended',
  TOOL_RESULT_APPENDED: 'tool_result_appended',
  APPROVAL_NEEDED: 'approval_needed',
  TOOL_CALLS: 'tool_calls',
  TURN_COMPLETED: 'turn_completed',
  TURN_FAILED: 'turn_failed',
} as const;

export const TURN_RUNTIME_KEY = {
  PENDING_TOOL_CALLS: 'pending_tool_calls',
} as const;

export function isStreamingTurnStatus(
  status: TurnStatus | null | undefined,
): boolean {
  return (
    status === TURN_STATUS.RUNNING || status === TURN_STATUS.AWAITING_APPROVAL
  );
}

export function isTerminalTurnStatus(
  status: TurnStatus | null | undefined,
): boolean {
  return status === TURN_STATUS.COMPLETED || status === TURN_STATUS.FAILED;
}
