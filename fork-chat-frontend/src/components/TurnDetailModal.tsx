import { Brain, RefreshCw, Wrench } from 'lucide-react';
import { useMemo, useRef } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { TURN_RUNTIME_KEY, TURN_STATUS } from '../api/turnStream';
import type { ApproveDecisionKind, Protocol, Turn } from '../api/types';
import { MessageInput } from './MessageInput';
import { Button } from './ui/button';
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from './ui/dialog';

interface TurnDetailModalProps {
  turn: Turn | null;
  protocol: Protocol;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSend: (
    text: string,
    provider: string,
    model: string,
    parentId: string | null,
  ) => void;
  onRetry: (turnId: string, provider: string, model: string) => void;
  onApprove: (
    turnId: string,
    pendingCallId: string,
    decision: ApproveDecisionKind,
  ) => void;
  onCancel: (turnId: string) => void;
  isSending: boolean;
}

type PendingToolCall = {
  pending_call_id: string;
  call_id: string;
  name: string;
  input: unknown;
};

type TraceItem =
  | { kind: 'user_text'; text: string }
  | { kind: 'assistant_text'; text: string }
  | { kind: 'thinking'; text: string; raw?: unknown }
  | { kind: 'tool_call'; name: string; input: unknown; callId?: string }
  | {
      kind: 'tool_result';
      name?: string;
      output: string;
      callId?: string;
      isError: boolean;
      raw?: unknown;
    }
  | { kind: 'other'; role: string; raw: unknown };

const MAX_TRACE_ITEMS = 300;
const LARGE_TOOL_OUTPUT_THRESHOLD = 8_000;
const LARGE_TOOL_OUTPUT_PREVIEW_CHARS = 4_000;

function getPendingToolCalls(turn: Turn): PendingToolCall[] {
  const raw = turn.runtime_state?.[TURN_RUNTIME_KEY.PENDING_TOOL_CALLS];
  if (!Array.isArray(raw)) return [];
  return raw.filter((item): item is PendingToolCall => {
    if (!item || typeof item !== 'object') return false;
    const rec = item as Record<string, unknown>;
    return (
      typeof rec.pending_call_id === 'string' &&
      typeof rec.call_id === 'string' &&
      typeof rec.name === 'string'
    );
  });
}

function stringify(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function safeJson(value: string): unknown {
  try {
    return JSON.parse(value);
  } catch {
    return value;
  }
}

function extractMessageText(content: unknown): string | null {
  if (!Array.isArray(content)) return null;
  const parts = content
    .map((block) => {
      if (!block || typeof block !== 'object') return null;
      const rec = block as Record<string, unknown>;
      if (rec.type === 'output_text' && typeof rec.text === 'string') {
        return rec.text;
      }
      return null;
    })
    .filter((v): v is string => Boolean(v));
  if (parts.length === 0) return null;
  return parts.join('\n');
}

function extractReasoningText(block: Record<string, unknown>): string | null {
  const content = block.content;
  if (Array.isArray(content)) {
    const texts = content
      .map((entry) => {
        if (!entry || typeof entry !== 'object') return null;
        const rec = entry as Record<string, unknown>;
        return typeof rec.text === 'string' ? rec.text : null;
      })
      .filter((v): v is string => Boolean(v));
    if (texts.length > 0) return texts.join('\n');
  }
  const summary = block.summary;
  if (Array.isArray(summary)) {
    const texts = summary
      .map((entry) => {
        if (!entry || typeof entry !== 'object') return null;
        const rec = entry as Record<string, unknown>;
        return typeof rec.text === 'string' ? rec.text : null;
      })
      .filter((v): v is string => Boolean(v));
    if (texts.length > 0) return texts.join('\n');
  }
  return null;
}

function buildTraceItems(turn: Turn): TraceItem[] {
  const transcript = Array.isArray(turn.turn_messages)
    ? turn.turn_messages
    : [];
  const out: TraceItem[] = [];

  for (const entry of transcript) {
    if (out.length >= MAX_TRACE_ITEMS) {
      out.push({
        kind: 'other',
        role: 'system',
        raw: `Trace truncated at ${MAX_TRACE_ITEMS} items`,
      });
      break;
    }
    if (!entry || typeof entry !== 'object') {
      out.push({ kind: 'other', role: 'unknown', raw: entry });
      continue;
    }
    const row = entry as Record<string, unknown>;
    const role = typeof row.role === 'string' ? row.role : 'unknown';
    const content = Array.isArray(row.content) ? row.content : [];

    for (const block of content) {
      if (out.length >= MAX_TRACE_ITEMS) {
        out.push({
          kind: 'other',
          role: 'system',
          raw: `Trace truncated at ${MAX_TRACE_ITEMS} items`,
        });
        break;
      }
      if (!block || typeof block !== 'object') {
        out.push({ kind: 'other', role, raw: block });
        continue;
      }
      const b = block as Record<string, unknown>;
      const type = typeof b.type === 'string' ? b.type : 'unknown';

      if (role === 'user' && type === 'text' && typeof b.text === 'string') {
        out.push({ kind: 'user_text', text: b.text });
        continue;
      }

      if (
        role === 'user' &&
        b.role === 'user' &&
        typeof b.content === 'string'
      ) {
        out.push({ kind: 'user_text', text: b.content });
        continue;
      }

      if (
        role === 'user' &&
        type === 'message' &&
        b.role === 'user' &&
        typeof b.content === 'string'
      ) {
        out.push({ kind: 'user_text', text: b.content });
        continue;
      }

      if (
        role === 'assistant' &&
        type === 'message' &&
        b.role === 'assistant' &&
        Array.isArray(b.content)
      ) {
        const text = extractMessageText(b.content);
        if (text) {
          out.push({ kind: 'assistant_text', text });
          continue;
        }
      }

      if (
        role === 'assistant' &&
        type === 'text' &&
        typeof b.text === 'string'
      ) {
        out.push({ kind: 'assistant_text', text: b.text });
        continue;
      }

      if (role === 'assistant' && type === 'reasoning') {
        out.push({
          kind: 'thinking',
          text: extractReasoningText(b) ?? 'Thinking block',
          raw: block,
        });
        continue;
      }

      if (role === 'assistant' && type === 'function_call') {
        out.push({
          kind: 'tool_call',
          name: typeof b.name === 'string' ? b.name : 'function_call',
          input:
            typeof b.arguments === 'string'
              ? safeJson(b.arguments)
              : b.arguments,
          callId:
            typeof b.call_id === 'string'
              ? b.call_id
              : typeof b.id === 'string'
                ? b.id
                : undefined,
        });
        continue;
      }

      if (role === 'assistant' && type === 'tool_use') {
        out.push({
          kind: 'tool_call',
          name: typeof b.name === 'string' ? b.name : 'tool_use',
          input: b.input ?? {},
          callId: typeof b.id === 'string' ? b.id : undefined,
        });
        continue;
      }

      if (role === 'user' && type === 'function_call_output') {
        const output = b.output;
        out.push({
          kind: 'tool_result',
          output: typeof output === 'string' ? output : stringify(output ?? ''),
          callId: typeof b.call_id === 'string' ? b.call_id : undefined,
          isError: b.is_error === true,
          raw: block,
        });
        continue;
      }

      if (role === 'user' && type === 'tool_result') {
        const contentValue = b.content;
        out.push({
          kind: 'tool_result',
          output:
            typeof contentValue === 'string'
              ? contentValue
              : stringify(contentValue ?? ''),
          name: typeof b.name === 'string' ? b.name : undefined,
          callId: typeof b.tool_use_id === 'string' ? b.tool_use_id : undefined,
          isError: b.is_error === true,
          raw: block,
        });
        continue;
      }

      out.push({ kind: 'other', role, raw: block });
    }
  }

  return out;
}

export function TurnDetailModal({
  turn,
  protocol,
  open,
  onOpenChange,
  onSend,
  onRetry,
  onApprove,
  onCancel,
  isSending,
}: TurnDetailModalProps) {
  const lastTurnRef = useRef<Turn | null>(null);
  if (turn) lastTurnRef.current = turn;
  const displayTurn = turn ?? lastTurnRef.current;

  const pendingCalls = useMemo(
    () => (displayTurn ? getPendingToolCalls(displayTurn) : []),
    [displayTurn],
  );
  const pendingCallByCallId = useMemo(() => {
    const map = new Map<string, PendingToolCall>();
    for (const call of pendingCalls) {
      map.set(call.call_id, call);
    }
    return map;
  }, [pendingCalls]);
  const traceItems = useMemo(() => {
    if (!displayTurn) return [];
    const baseItems = buildTraceItems(displayTurn);
    if (pendingCalls.length === 0) return baseItems;

    const existingToolCallIds = new Set(
      baseItems.flatMap((item) =>
        item.kind === 'tool_call' && item.callId ? [item.callId] : [],
      ),
    );
    const synthesizedPendingCalls: TraceItem[] = pendingCalls
      .filter((call) => !existingToolCallIds.has(call.call_id))
      .map((call) => ({
        kind: 'tool_call',
        name: call.name,
        input: call.input,
        callId: call.call_id,
      }));

    if (synthesizedPendingCalls.length === 0) return baseItems;
    return [...baseItems, ...synthesizedPendingCalls];
  }, [displayTurn, pendingCalls]);

  if (!displayTurn) return null;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-4xl h-[85vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>
            {displayTurn.user_text?.slice(0, 80) || 'Assistant response'}
          </DialogTitle>
          <DialogClose />
        </DialogHeader>

        <div className="flex items-center gap-2 mb-4 text-xs text-gray-500">
          <span
            className={[
              'px-2 py-1 rounded',
              displayTurn.status === TURN_STATUS.COMPLETED
                ? 'bg-green-100 text-green-800'
                : '',
              displayTurn.status === TURN_STATUS.RUNNING
                ? 'bg-yellow-100 text-yellow-800'
                : '',
              displayTurn.status === TURN_STATUS.FAILED
                ? 'bg-red-100 text-red-800'
                : '',
            ].join(' ')}
          >
            {displayTurn.status}
          </span>
          <span>{displayTurn.model}</span>
          {displayTurn.input_tokens && (
            <span>
              {displayTurn.input_tokens} in / {displayTurn.output_tokens} out
            </span>
          )}
        </div>

        <div className="sidebar-scrollbar flex-1 min-h-0 space-y-4 overflow-y-auto">
          {traceItems.length === 0 && displayTurn.user_text && (
            <div>
              <div className="text-xs text-gray-400 mb-1 font-medium">User</div>
              <div className="text-gray-800 markdown-content">
                <ReactMarkdown remarkPlugins={[remarkGfm]}>
                  {displayTurn.user_text}
                </ReactMarkdown>
              </div>
            </div>
          )}
          {traceItems.length === 0 && displayTurn.assistant_text && (
            <div>
              <div className="text-xs text-gray-400 mb-1 font-medium">
                Assistant
              </div>
              <div className="text-gray-700 markdown-content">
                <ReactMarkdown remarkPlugins={[remarkGfm]}>
                  {displayTurn.assistant_text}
                </ReactMarkdown>
              </div>
            </div>
          )}

          {displayTurn.error && (
            <div className="p-3 bg-red-50 text-red-600 rounded text-sm">
              Error: {JSON.stringify(displayTurn.error)}
            </div>
          )}

          {traceItems.length > 0 && (
            <div className="space-y-2">
              <div className="text-xs text-gray-400 font-medium">Trace</div>
              {traceItems.map((item, i) => {
                if (item.kind === 'user_text') {
                  return (
                    <div
                      key={i}
                      className="rounded-lg border border-zinc-200 p-3 bg-white"
                    >
                      <div className="text-[11px] text-zinc-500 mb-1 font-medium">
                        User
                      </div>
                      <div className="text-sm text-zinc-800 markdown-content">
                        <ReactMarkdown remarkPlugins={[remarkGfm]}>
                          {item.text}
                        </ReactMarkdown>
                      </div>
                    </div>
                  );
                }

                if (item.kind === 'assistant_text') {
                  return (
                    <div
                      key={i}
                      className="rounded-lg border border-zinc-200 p-3 bg-white"
                    >
                      <div className="text-[11px] text-zinc-500 mb-1 font-medium">
                        Assistant
                      </div>
                      <div className="text-sm text-zinc-700 markdown-content">
                        <ReactMarkdown remarkPlugins={[remarkGfm]}>
                          {item.text}
                        </ReactMarkdown>
                      </div>
                    </div>
                  );
                }

                if (item.kind === 'thinking') {
                  return (
                    <details
                      key={i}
                      className="rounded-lg border border-zinc-200 bg-zinc-50"
                    >
                      <summary className="px-3 py-2 cursor-pointer text-xs text-zinc-500 flex items-center gap-1">
                        <Brain className="size-3.5" />
                        Thinking
                      </summary>
                      <div className="px-3 pb-3">
                        <div className="text-xs text-zinc-500 whitespace-pre-wrap">
                          {item.text}
                        </div>
                      </div>
                    </details>
                  );
                }

                if (item.kind === 'tool_call') {
                  const pendingCall = item.callId
                    ? pendingCallByCallId.get(item.callId)
                    : undefined;
                  const isPendingApproval =
                    displayTurn.status === TURN_STATUS.AWAITING_APPROVAL &&
                    Boolean(pendingCall);
                  return (
                    <div
                      key={i}
                      data-testid="tool-call-card"
                      className="rounded-lg border border-zinc-200 bg-white p-3"
                    >
                      <div className="text-sm font-medium text-zinc-800 flex items-center gap-2">
                        <Wrench className="size-4" />
                        Tool call: {item.name}
                      </div>
                      {item.callId && (
                        <div className="text-[11px] text-zinc-500 mt-1">
                          call_id: {item.callId}
                        </div>
                      )}
                      <details className="mt-2" open={isPendingApproval}>
                        <summary className="cursor-pointer text-xs text-zinc-500">
                          Input
                        </summary>
                        <pre className="mt-2 text-xs bg-zinc-50 border border-zinc-200 rounded p-2 whitespace-pre-wrap break-words text-zinc-700">
                          {stringify(pendingCall?.input ?? item.input)}
                        </pre>
                      </details>
                      {isPendingApproval && pendingCall && (
                        <div className="mt-3 flex items-center gap-2">
                          <Button
                            size="sm"
                            disabled={isSending}
                            onClick={() => {
                              onApprove(
                                displayTurn.id,
                                pendingCall.pending_call_id,
                                'allow',
                              );
                            }}
                          >
                            Allow
                          </Button>
                          <Button
                            variant="outline"
                            size="sm"
                            disabled={isSending}
                            onClick={() => {
                              onApprove(
                                displayTurn.id,
                                pendingCall.pending_call_id,
                                'allow_always',
                              );
                            }}
                          >
                            Always allow this tool
                          </Button>
                          <Button
                            variant="destructive"
                            size="sm"
                            disabled={isSending}
                            onClick={() => {
                              onApprove(
                                displayTurn.id,
                                pendingCall.pending_call_id,
                                'deny',
                              );
                            }}
                          >
                            Deny
                          </Button>
                        </div>
                      )}
                    </div>
                  );
                }

                if (item.kind === 'tool_result') {
                  const isLargeOutput =
                    item.output.length > LARGE_TOOL_OUTPUT_THRESHOLD;
                  const previewOutput = isLargeOutput
                    ? `${item.output.slice(0, LARGE_TOOL_OUTPUT_PREVIEW_CHARS)}\n\n[preview truncated for performance]`
                    : item.output;
                  return (
                    <div
                      key={i}
                      className={[
                        'rounded-lg border p-3 bg-white',
                        item.isError
                          ? 'border-red-200 bg-red-50'
                          : 'border-zinc-200',
                      ].join(' ')}
                    >
                      <div className="text-sm font-medium text-zinc-800">
                        Tool result
                      </div>
                      {item.callId && (
                        <div className="text-[11px] text-zinc-500 mt-1">
                          call_id: {item.callId}
                        </div>
                      )}
                      <details className="mt-2">
                        <summary className="cursor-pointer text-xs text-zinc-500">
                          Output
                        </summary>
                        {isLargeOutput ? (
                          <pre className="mt-2 text-xs bg-zinc-50 border border-zinc-200 rounded p-2 whitespace-pre-wrap break-words text-zinc-700">
                            {previewOutput}
                          </pre>
                        ) : (
                          <div className="mt-2 text-sm text-zinc-700 markdown-content">
                            <ReactMarkdown remarkPlugins={[remarkGfm]}>
                              {previewOutput}
                            </ReactMarkdown>
                          </div>
                        )}
                        {item.isError && item.raw && (
                          <pre className="mt-2 text-xs bg-red-50 border border-red-200 rounded p-2 whitespace-pre-wrap break-words text-red-700">
                            {stringify(item.raw)}
                          </pre>
                        )}
                      </details>
                    </div>
                  );
                }

                return (
                  <details
                    key={i}
                    className="rounded-lg border border-zinc-200 bg-zinc-50"
                  >
                    <summary className="px-3 py-2 cursor-pointer text-xs text-zinc-500">
                      {item.role} block
                    </summary>
                    <pre className="px-3 pb-3 text-xs whitespace-pre-wrap break-words text-zinc-600">
                      {stringify(item.raw)}
                    </pre>
                  </details>
                );
              })}
            </div>
          )}
        </div>

        <div className="border-t pt-4 mt-4">
          {isSending && (
            <div className="text-center text-sm text-muted-foreground mb-2">
              Waiting for AI response...
            </div>
          )}
          {displayTurn.status === TURN_STATUS.FAILED && (
            <div className="mb-3">
              <Button
                variant="outline"
                className="w-full"
                disabled={isSending}
                onClick={() => {
                  onRetry(
                    displayTurn.id,
                    displayTurn.provider ?? '',
                    displayTurn.model ?? '',
                  );
                }}
              >
                <RefreshCw className="size-4 mr-1" />
                Retry
              </Button>
            </div>
          )}
          {displayTurn.status === TURN_STATUS.AWAITING_APPROVAL && (
            <div className="mb-3">
              <Button
                variant="outline"
                className="w-full"
                disabled={isSending}
                onClick={() => onCancel(displayTurn.id)}
              >
                Cancel turn
              </Button>
            </div>
          )}
          {displayTurn.status === TURN_STATUS.RUNNING && (
            <div className="mb-3">
              <Button
                variant="outline"
                className="w-full"
                disabled={isSending}
                onClick={() => onCancel(displayTurn.id)}
              >
                Cancel turn
              </Button>
            </div>
          )}
          {displayTurn.status === TURN_STATUS.COMPLETED && (
            <MessageInput
              key={displayTurn.id}
              parentTurn={displayTurn}
              protocol={protocol}
              onSend={onSend}
              disabled={isSending}
            />
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
