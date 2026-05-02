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

type TurnSnapshotPayload = {
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

type AssistantEntryPayload = {
  seq?: number;
  payload?: {
    entry?: unknown;
    assistant_text?: string | null;
    input_tokens?: number | null;
    output_tokens?: number | null;
    cached_tokens?: number | null;
  };
};

type ToolResultPayload = {
  seq?: number;
  payload?: { entry?: unknown };
};

type ApprovalNeededPayload = {
  seq?: number;
  payload?: { pending?: unknown[] };
};

type TurnCompletedPayload = {
  seq?: number;
  payload?: {
    assistant_text?: string | null;
    input_tokens?: number | null;
    output_tokens?: number | null;
    cached_tokens?: number | null;
  };
};

type TurnFailedPayload = {
  seq?: number;
  payload?: {
    error?: Record<string, unknown> | null;
  };
};

interface UseTurnStreamParams {
  sessionId: string;
  turnId: string | null;
  turnStatus: Turn['status'] | null;
  queryClient: QueryClient;
}

/**
 * Extracts assistant plain text from one transcript entry.
 *
 * OpenAI-style assistant messages can appear either as direct `text` blocks or
 * nested `message -> output_text` blocks. We normalize both to a display text.
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
    if (b.type === 'text' && typeof b.text === 'string') {
      parts.push(b.text);
      continue;
    }
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
 * Used for snapshot recovery and refresh scenarios where `assistant_text`
 * may lag behind `turn_messages`.
 */
function inferAssistantTextFromTranscript(
  turnMessages: unknown[],
): string | null {
  let latest: string | null = null;
  for (const entry of turnMessages) {
    const text = extractAssistantTextFromEntry(entry);
    if (text) latest = text;
  }
  return latest;
}

/**
 * Applies one immutable update to a target turn in the tree cache.
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
 * The hook keeps a per-turn sequence cursor to avoid duplicate application on
 * reconnects and browser/network retries.
 */
export function useTurnStream({
  sessionId,
  turnId,
  turnStatus,
  queryClient,
}: UseTurnStreamParams) {
  const streamSeqRef = useRef<Map<string, number>>(new Map());

  // Collapse `turnStatus` into a stable streaming/non-streaming boolean so the
  // effect only re-runs when we actually need to open or close the connection.
  //
  // Without this collapse, `running` → `awaiting_approval` (and back after an
  // approval) would each re-trigger the effect, tearing down and recreating
  // the EventSource even though the backend's SSE hub is keyed by `turn_id`
  // and is fully capable of spanning the whole lifecycle over one connection.
  const isStreaming = isStreamingTurnStatus(turnStatus);

  useEffect(() => {
    if (!turnId) return;
    if (!isStreaming) return;

    const source = new EventSource(api.turns.streamUrl(sessionId, turnId));
    let closedByTerminalEvent = false;

    const shouldApplySeq = (seq: number | undefined) => {
      if (typeof seq !== 'number') return true;
      const prev = streamSeqRef.current.get(turnId) ?? 0;
      if (seq <= prev) return false;
      streamSeqRef.current.set(turnId, seq);
      return true;
    };

    source.addEventListener(TURN_STREAM_EVENT.TURN_SNAPSHOT, (evt) => {
      const data = JSON.parse(
        (evt as MessageEvent).data,
      ) as TurnSnapshotPayload;
      const prev = streamSeqRef.current.get(turnId) ?? 0;
      streamSeqRef.current.set(turnId, Math.max(prev, data.seq ?? 0));
      const inferredAssistantText = inferAssistantTextFromTranscript(
        Array.isArray(data.turn_messages) ? data.turn_messages : [],
      );
      updateTurnInTree(queryClient, sessionId, turnId, (turn) => ({
        ...turn,
        status: data.status,
        turn_messages: data.turn_messages,
        runtime_state: data.runtime_state ?? turn.runtime_state,
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

    source.addEventListener(
      TURN_STREAM_EVENT.ASSISTANT_ENTRY_APPENDED,
      (evt) => {
        const data = JSON.parse(
          (evt as MessageEvent).data,
        ) as AssistantEntryPayload;
        if (!shouldApplySeq(data.seq)) return;
        const entry = data.payload?.entry;
        if (!entry) return;
        const assistantText = extractAssistantTextFromEntry(entry);
        updateTurnInTree(queryClient, sessionId, turnId, (turn) => ({
          ...turn,
          turn_messages: [
            ...(Array.isArray(turn.turn_messages) ? turn.turn_messages : []),
            entry,
          ],
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

    source.addEventListener(TURN_STREAM_EVENT.TOOL_RESULT_APPENDED, (evt) => {
      const data = JSON.parse((evt as MessageEvent).data) as ToolResultPayload;
      if (!shouldApplySeq(data.seq)) return;
      const entry = data.payload?.entry;
      if (!entry) return;
      updateTurnInTree(queryClient, sessionId, turnId, (turn) => ({
        ...turn,
        turn_messages: [
          ...(Array.isArray(turn.turn_messages) ? turn.turn_messages : []),
          entry,
        ],
      }));
    });

    source.addEventListener(TURN_STREAM_EVENT.APPROVAL_NEEDED, (evt) => {
      const data = JSON.parse(
        (evt as MessageEvent).data,
      ) as ApprovalNeededPayload;
      if (!shouldApplySeq(data.seq)) return;
      updateTurnInTree(queryClient, sessionId, turnId, (turn) => ({
        ...turn,
        status: TURN_STATUS.AWAITING_APPROVAL,
        runtime_state: {
          ...turn.runtime_state,
          [TURN_RUNTIME_KEY.PENDING_TOOL_CALLS]: data.payload?.pending ?? [],
        },
      }));
    });

    source.addEventListener(TURN_STREAM_EVENT.TURN_COMPLETED, (evt) => {
      const data = JSON.parse(
        (evt as MessageEvent).data,
      ) as TurnCompletedPayload;
      if (!shouldApplySeq(data.seq)) return;
      updateTurnInTree(queryClient, sessionId, turnId, (turn) => ({
        ...turn,
        status: TURN_STATUS.COMPLETED,
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
      queryClient.invalidateQueries({ queryKey: ['tree', sessionId] });
      closedByTerminalEvent = true;
      source.close();
    });

    source.addEventListener(TURN_STREAM_EVENT.TURN_FAILED, (evt) => {
      const data = JSON.parse((evt as MessageEvent).data) as TurnFailedPayload;
      if (!shouldApplySeq(data.seq)) return;
      updateTurnInTree(queryClient, sessionId, turnId, (turn) => ({
        ...turn,
        status: TURN_STATUS.FAILED,
        error: data.payload?.error ?? turn.error,
      }));
      queryClient.invalidateQueries({ queryKey: ['tree', sessionId] });
      closedByTerminalEvent = true;
      source.close();
    });

    source.onerror = () => {
      // Keep native EventSource reconnect behavior.
    };

    return () => {
      if (!closedByTerminalEvent) {
        source.close();
      }
    };
  }, [queryClient, sessionId, turnId, isStreaming]);
}
