/**
 * `useTurnStream` — React hook for subscribing to a running turn's SSE stream.
 *
 * This hook is the bridge between the backend's real-time turn events and the
 * frontend's React Query cache. It:
 *
 * 1. Opens an `EventSource` SSE connection when the turn is in a streaming
 *    state (`running` or `awaiting_approval`).
 * 2. Incrementally patches the turn data in the React Query `['tree', sessionId]`
 *    cache on each event, avoiding full refetches.
 * 3. Tracks a per-turn sequence cursor (`streamSeqRef`) to prevent duplicate
 *    application of events on browser-native EventSource reconnection.
 * 4. Closes the connection when the turn reaches a terminal state.
 *
 * The hook is designed so that the `running` -> `awaiting_approval` ->
 * `running` lifecycle transitions do NOT tear down and recreate the
 * EventSource. The backend's SSE hub is keyed by `turn_id` and spans the
 * entire lifecycle over one connection.
 */

import type { QueryClient } from '@tanstack/react-query';
import { useEffect, useRef } from 'react';
import { api } from '../api';
import {
  isStreamingTurnStatus,
  TURN_RUNTIME_KEY,
  TURN_STATUS,
  TURN_STREAM_EVENT,
} from '../api/turnStream';
import type { TreeResponse, Turn } from '../api/types';

/** Payload for the `turn_snapshot` SSE event. */
type TurnSnapshotPayload = {
  /** Monotonically increasing sequence number for ordering. */
  seq: number;
  status: Turn['status'];
  turn_messages: unknown[];
  runtime_state: Record<string, unknown>;
  assistant_text?: string | null;
  input_tokens?: number | null;
  output_tokens?: number | null;
  cached_tokens?: number | null;
  error?: Record<string, unknown> | null;
};

/** Payload for the `assistant_entry_appended` SSE event. */
type AssistantEntryPayload = {
  seq?: number;
  payload?: {
    /** The raw protocol-native transcript entry to append. */
    entry?: unknown;
    /** Extracted assistant text (may differ from the raw entry). */
    assistant_text?: string | null;
    input_tokens?: number | null;
    output_tokens?: number | null;
    cached_tokens?: number | null;
  };
};

/** Payload for the `tool_result_appended` SSE event. */
type ToolResultPayload = {
  seq?: number;
  payload?: { entry?: unknown };
};

/** Payload for the `approval_needed` SSE event. */
type ApprovalNeededPayload = {
  seq?: number;
  payload?: { pending?: unknown[] };
};

/** Payload for the `turn_completed` SSE event. */
type TurnCompletedPayload = {
  seq?: number;
  payload?: {
    /** Final assistant text (may override the accumulated text). */
    assistant_text?: string | null;
    input_tokens?: number | null;
    output_tokens?: number | null;
    cached_tokens?: number | null;
  };
};

/** Payload for the `turn_failed` SSE event. */
type TurnFailedPayload = {
  seq?: number;
  payload?: {
    /** Error details from the backend. */
    error?: Record<string, unknown> | null;
  };
};

interface UseTurnStreamParams {
  sessionId: string;
  /** The turn to stream. Null when no turn is selected. */
  turnId: string | null;
  /** Current status of the turn. Used to determine when to open/close the SSE connection. */
  turnStatus: Turn['status'] | null;
  /** React Query client for cache patching. */
  queryClient: QueryClient;
}

/**
 * Extracts assistant plain text from one transcript entry.
 *
 * OpenAI-style assistant messages can appear in two formats:
 * 1. **Direct text block**: `{ role: "assistant", content: [{ type: "text", text: "..." }] }`
 * 2. **Nested message block**: `{ role: "assistant", content: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "..." }] }] }`
 *
 * The second format appears when the backend wraps the assistant's response in
 * an additional message layer. We handle both by iterating content blocks and
 * collecting text from either format.
 */
function extractAssistantTextFromEntry(entry: unknown): string | null {
  if (!entry || typeof entry !== 'object') return null;
  const row = entry as Record<string, unknown>;
  if (row.role !== 'assistant') return null;
  const content = Array.isArray(row.content) ? row.content : [];
  const parts: string[] = [];

  for (const block of content) {
    if (!block || typeof block !== 'object') continue;
    const b = block as Record<string, unknown>;
    // Format 1: direct text block
    if (b.type === 'text' && typeof b.text === 'string') {
      parts.push(b.text);
      continue;
    }
    // Format 2: nested message -> output_text block
    if (
      b.type === 'message' &&
      b.role === 'assistant' &&
      Array.isArray(b.content)
    ) {
      for (const piece of b.content) {
        if (!piece || typeof piece !== 'object') continue;
        const p = piece as Record<string, unknown>;
        if (p.type === 'output_text' && typeof p.text === 'string') {
          parts.push(p.text);
        }
      }
    }
  }

  if (parts.length === 0) return null;
  return parts.join('\n');
}

/**
 * Rebuilds the latest assistant text from a full transcript.
 *
 * This is used for **snapshot recovery**: when the SSE connection sends a
 * `turn_snapshot` event (e.g. on initial connect or after reconnection), the
 * `assistant_text` field in the payload may be stale or absent. By scanning
 * all transcript entries and extracting the last assistant text, we ensure the
 * UI shows the most recent text even if the snapshot's `assistant_text` field
 * lagged behind.
 */
function inferAssistantTextFromTranscript(
  turnMessages: unknown[],
): string | null {
  // Walk the entire transcript and keep overwriting with the latest assistant
  // text found. The last one wins, which is correct since transcript entries
  // are ordered chronologically.
  let latest: string | null = null;
  for (const entry of turnMessages) {
    const text = extractAssistantTextFromEntry(entry);
    if (text) latest = text;
  }
  return latest;
}

/**
 * Applies one immutable update to a target turn in the React Query tree cache.
 *
 * This is the core cache patching pattern: instead of invalidating and
 * refetching the entire tree query on every SSE event (which would cause
 * visible flicker and unnecessary network requests), we use
 * `queryClient.setQueryData` to produce a new immutable tree object where
 * only the target turn is replaced. React Query's structural sharing then
 * ensures only the changed turn triggers a re-render.
 *
 * @param queryClient - The React Query client instance
 * @param sessionId - Session ID (part of the query key)
 * @param turnId - The turn to patch
 * @param apply - Pure function that receives the current turn and returns a new turn object
 */
function updateTurnInTree(
  queryClient: QueryClient,
  sessionId: string,
  turnId: string,
  apply: (turn: Turn) => Turn,
) {
  queryClient.setQueryData<TreeResponse | undefined>(
    ['tree', sessionId],
    (prev) => {
      if (!prev) return prev;
      // Produce a new TreeResponse with the target turn replaced.
      // The `map` ensures all other turns are referentially equal (structural sharing).
      return {
        ...prev,
        turns: prev.turns.map((turn) =>
          turn.id === turnId ? apply(turn) : turn,
        ),
      };
    },
  );
}

/**
 * Subscribes to one running turn's SSE stream and incrementally patches React
 * Query cache with ordered events.
 *
 * ## Sequence cursor (`streamSeqRef`)
 *
 * Each SSE event carries a `seq` number. The hook tracks the highest seq
 * applied per turn and rejects events with seq <= the cursor. This prevents
 * duplicate application when the browser's built-in EventSource reconnection
 * replays events that were already processed.
 *
 * ## `isStreaming` collapse
 *
 * The `turnStatus` prop is collapsed to a boolean `isStreaming` via
 * `isStreamingTurnStatus`. This means `running` and `awaiting_approval` both
 * map to `true`. Without this collapse, a transition from `running` to
 * `awaiting_approval` (and back after approval) would trigger the useEffect
 * cleanup and re-run, tearing down and recreating the EventSource even though
 * the backend's SSE hub spans the entire lifecycle over one connection.
 */
export function useTurnStream({
  sessionId,
  turnId,
  turnStatus,
  queryClient,
}: UseTurnStreamParams) {
  // Per-turn sequence cursor: Map<turnId, highestAppliedSeq>.
  // Persists across re-renders so we don't re-apply events after reconnection.
  const streamSeqRef = useRef<Map<string, number>>(new Map());

  // Collapse `turnStatus` into a stable streaming/non-streaming boolean so the
  // effect only re-runs when we actually need to open or close the connection.
  //
  // Without this collapse, `running` -> `awaiting_approval` (and back after an
  // approval) would each re-trigger the effect, tearing down and recreating
  // the EventSource even though the backend's SSE hub is keyed by `turn_id`
  // and is fully capable of spanning the whole lifecycle over one connection.
  const isStreaming = isStreamingTurnStatus(turnStatus);

  useEffect(() => {
    // No turn selected or turn is in a terminal state — nothing to stream.
    if (!turnId) return;
    if (!isStreaming) return;

    // Open the SSE connection. EventSource handles GET requests and provides
    // automatic reconnection on transient network failures.
    const source = new EventSource(api.turns.streamUrl(sessionId, turnId));
    // Track whether the connection was closed by a terminal event handler
    // (turn_completed or turn_failed) so the cleanup function doesn't
    // double-close.
    let closedByTerminalEvent = false;

    /**
     * Guard that checks whether an event's sequence number should be applied.
     *
     * Returns `true` if `seq` is undefined (legacy events without seq) or if
     * it's strictly greater than the last applied seq for this turn. Updates
     * the cursor when the event is accepted.
     */
    const shouldApplySeq = (seq: number | undefined) => {
      if (typeof seq !== 'number') return true;
      const prev = streamSeqRef.current.get(turnId) ?? 0;
      // Reject events we've already applied (prevents duplicates on reconnect).
      if (seq <= prev) return false;
      streamSeqRef.current.set(turnId, seq);
      return true;
    };

    // --- Event handler: turn_snapshot ---
    // Full state snapshot. Sent on initial connect, reconnection, and
    // periodically by the backend. Replaces most turn fields atomically.
    source.addEventListener(TURN_STREAM_EVENT.TURN_SNAPSHOT, (evt) => {
      const data = JSON.parse(
        (evt as MessageEvent).data,
      ) as TurnSnapshotPayload;
      // Always accept snapshot seq to keep the cursor advancing, even if we
      // later decide not to apply individual fields.
      const prev = streamSeqRef.current.get(turnId) ?? 0;
      streamSeqRef.current.set(turnId, Math.max(prev, data.seq ?? 0));
      // Infer assistant text from the full transcript as a fallback for
      // snapshots where `assistant_text` is stale or absent.
      const inferredAssistantText = inferAssistantTextFromTranscript(
        Array.isArray(data.turn_messages) ? data.turn_messages : [],
      );
      // Patch the turn in the tree cache with the snapshot data.
      // Each field falls back to the existing value if the snapshot doesn't
      // include it (partial snapshots are possible).
      updateTurnInTree(queryClient, sessionId, turnId, (turn) => ({
        ...turn,
        status: data.status,
        turn_messages: data.turn_messages,
        runtime_state: data.runtime_state ?? turn.runtime_state,
        // Prefer the snapshot's explicit assistant_text, then the inferred
        // text from the transcript, then the existing value.
        assistant_text:
          typeof data.assistant_text === 'string'
            ? data.assistant_text
            : (inferredAssistantText ?? turn.assistant_text),
        input_tokens:
          typeof data.input_tokens === 'number'
            ? data.input_tokens
            : turn.input_tokens,
        output_tokens:
          typeof data.output_tokens === 'number'
            ? data.output_tokens
            : turn.output_tokens,
        cached_tokens:
          typeof data.cached_tokens === 'number'
            ? data.cached_tokens
            : turn.cached_tokens,
        error: data.error ?? turn.error,
      }));
    });

    // --- Event handler: assistant_entry_appended ---
    // A new assistant message entry has been appended to the transcript.
    // Patches the turn by appending the entry and updating assistant_text.
    source.addEventListener(
      TURN_STREAM_EVENT.ASSISTANT_ENTRY_APPENDED,
      (evt) => {
        const data = JSON.parse(
          (evt as MessageEvent).data,
        ) as AssistantEntryPayload;
        if (!shouldApplySeq(data.seq)) return;
        const entry = data.payload?.entry;
        if (!entry) return;
        // Try to extract assistant text from the new entry for a quick
        // update without scanning the full transcript.
        const assistantText = extractAssistantTextFromEntry(entry);
        updateTurnInTree(queryClient, sessionId, turnId, (turn) => ({
          ...turn,
          // Append the new entry to the existing transcript.
          turn_messages: [
            ...(Array.isArray(turn.turn_messages) ? turn.turn_messages : []),
            entry,
          ],
          // Prefer the explicit assistant_text from the payload, then the
          // extracted text, then keep the existing text.
          assistant_text:
            typeof data.payload?.assistant_text === 'string'
              ? data.payload.assistant_text
              : (assistantText ?? turn.assistant_text),
          input_tokens:
            typeof data.payload?.input_tokens === 'number'
              ? data.payload.input_tokens
              : turn.input_tokens,
          output_tokens:
            typeof data.payload?.output_tokens === 'number'
              ? data.payload.output_tokens
              : turn.output_tokens,
          cached_tokens:
            typeof data.payload?.cached_tokens === 'number'
              ? data.payload.cached_tokens
              : turn.cached_tokens,
        }));
      },
    );

    // --- Event handler: tool_result_appended ---
    // A tool result entry has been appended to the transcript. Less common
    // than assistant entries; only patches the transcript array.
    source.addEventListener(TURN_STREAM_EVENT.TOOL_RESULT_APPENDED, (evt) => {
      const data = JSON.parse((evt as MessageEvent).data) as ToolResultPayload;
      if (!shouldApplySeq(data.seq)) return;
      const entry = data.payload?.entry;
      if (!entry) return;
      updateTurnInTree(queryClient, sessionId, turnId, (turn) => ({
        ...turn,
        // Append the tool result entry to the transcript.
        turn_messages: [
          ...(Array.isArray(turn.turn_messages) ? turn.turn_messages : []),
          entry,
        ],
      }));
    });

    // --- Event handler: approval_needed ---
    // The turn loop paused because tool calls require human approval.
    // Patches the turn status and stores the pending tool calls in
    // runtime_state so the UI can render the approval buttons.
    source.addEventListener(TURN_STREAM_EVENT.APPROVAL_NEEDED, (evt) => {
      const data = JSON.parse(
        (evt as MessageEvent).data,
      ) as ApprovalNeededPayload;
      if (!shouldApplySeq(data.seq)) return;
      updateTurnInTree(queryClient, sessionId, turnId, (turn) => ({
        ...turn,
        // Transition to awaiting_approval state.
        status: TURN_STATUS.AWAITING_APPROVAL,
        // Store pending tool calls so the UI can render approval buttons.
        runtime_state: {
          ...turn.runtime_state,
          [TURN_RUNTIME_KEY.PENDING_TOOL_CALLS]: data.payload?.pending ?? [],
        },
      }));
    });

    // --- Event handler: turn_completed ---
    // Terminal event: the turn finished successfully. Patches final token
    // counts and assistant text, then closes the SSE connection.
    source.addEventListener(TURN_STREAM_EVENT.TURN_COMPLETED, (evt) => {
      const data = JSON.parse(
        (evt as MessageEvent).data,
      ) as TurnCompletedPayload;
      if (!shouldApplySeq(data.seq)) return;
      // Patch the turn with final state.
      updateTurnInTree(queryClient, sessionId, turnId, (turn) => ({
        ...turn,
        status: TURN_STATUS.COMPLETED,
        // The completed payload may carry a final assistant_text that
        // overrides the accumulated streaming text.
        assistant_text:
          typeof data.payload?.assistant_text === 'string'
            ? data.payload.assistant_text
            : turn.assistant_text,
        input_tokens:
          typeof data.payload?.input_tokens === 'number'
            ? data.payload.input_tokens
            : turn.input_tokens,
        output_tokens:
          typeof data.payload?.output_tokens === 'number'
            ? data.payload.output_tokens
            : turn.output_tokens,
        cached_tokens:
          typeof data.payload?.cached_tokens === 'number'
            ? data.payload.cached_tokens
            : turn.cached_tokens,
      }));
      // Invalidate the tree query to ensure the cache is eventually consistent
      // with the database (e.g. updated_at timestamps on the session).
      queryClient.invalidateQueries({ queryKey: ['tree', sessionId] });
      closedByTerminalEvent = true;
      source.close();
    });

    // --- Event handler: turn_failed ---
    // Terminal event: the turn failed. Patches the error and closes the
    // SSE connection.
    source.addEventListener(TURN_STREAM_EVENT.TURN_FAILED, (evt) => {
      const data = JSON.parse((evt as MessageEvent).data) as TurnFailedPayload;
      if (!shouldApplySeq(data.seq)) return;
      updateTurnInTree(queryClient, sessionId, turnId, (turn) => ({
        ...turn,
        status: TURN_STATUS.FAILED,
        // Store the error for display in the UI.
        error: data.payload?.error ?? turn.error,
      }));
      queryClient.invalidateQueries({ queryKey: ['tree', sessionId] });
      closedByTerminalEvent = true;
      source.close();
    });

    // --- Error handler ---
    // Don't close on errors — let the browser's native EventSource
    // reconnection behavior handle transient network failures.
    source.onerror = () => {
      // Keep native EventSource reconnect behavior.
    };

    // Cleanup: close the EventSource when the effect re-runs or the component
    // unmounts.  Skip if a terminal event handler already closed it.
    return () => {
      if (!closedByTerminalEvent) {
        source.close();
      }
    };
  }, [queryClient, sessionId, turnId, isStreaming]);
}
