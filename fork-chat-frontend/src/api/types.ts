/**
 * TypeScript type definitions for the fork-chat API.
 *
 * These types mirror the Rust structs returned by the backend API endpoints.
 * They are used throughout the frontend for type-safe API calls, React Query
 * cache types, and component props.
 */

import type { TurnStatus } from './turnStream';

/** Wire protocol identifier. Must match the backend `Protocol` enum. */
export type Protocol = 'openai' | 'anthropic';

/** A single model exposed by a provider. `name` is an optional display label. */
export interface Model {
  /** Wire model id sent to the upstream API (e.g. "gpt-4o", "deepseek-chat"). */
  id: string;
  /** Human-friendly display name for the UI. Null means fall back to `id`. */
  name: string | null;
}

/** A provider as returned by the config endpoint. */
export interface PublicProvider {
  /** Unique provider name (e.g. "deepseek", "openai"). */
  name: string;
  /** Protocols this provider supports (subset of all known protocols). */
  supported_protocols: Protocol[];
  /** Models available from this provider. */
  models: Model[];
}

/** Response from `GET /api/config`. */
export interface ConfigResponse {
  /** All protocols the server knows about. */
  protocols: Protocol[];
  /** All configured providers with their models and supported protocols. */
  providers: PublicProvider[];
  /** All built-in tools with their default approval policies. */
  tools: PublicTool[];
}

/** A built-in tool definition as returned by the config endpoint. */
export interface PublicTool {
  /** Tool name (unique identifier, e.g. "web_search"). */
  name: string;
  /** Human-readable description of what the tool does. */
  description: string;
  /** Default approval policy: "auto" (execute immediately) or
   *  "require_approval" (pause for human approval before execution). */
  default_policy: 'auto' | 'require_approval';
}

/**
 * A conversation session.
 *
 * Sessions are the top-level container. Each session owns a tree of turns and
 * has a protocol locked at creation time.
 */
export interface Session {
  id: string;
  /** Session title. Null until auto-titled or manually updated. */
  title: string | null;
  /** System prompt prepended to every turn context. Null if not set. */
  system_prompt: string | null;
  /** Wire protocol chosen at session creation. Locked for the session's lifetime. */
  protocol: Protocol;
  /** Arbitrary preferences blob (reserved for future use). */
  preferences: Record<string, unknown>;
  created_at: string;
  updated_at: string;
}

/**
 * A single turn in a conversation tree.
 *
 * Turns form a tree via `parent_turn_id`. Each turn represents one
 * user-assistant exchange and contains the full protocol-native transcript
 * of messages sent to and received from the LLM.
 */
export interface Turn {
  id: string;
  session_id: string;
  /** Parent turn in the conversation tree. Null for the root turn. */
  parent_turn_id: string | null;
  /** If this turn is a retry of a previous turn, points to the original. */
  retry_turn_id: string | null;
  /** Current lifecycle status (running, awaiting_approval, completed, failed). */
  status: TurnStatus;
  /** The user's input text for this turn. */
  user_text: string | null;
  /** Extracted assistant response text. May lag behind turn_messages during streaming. */
  assistant_text: string | null;
  /**
   * Protocol-native transcript entries.
   *
   * This is an array of raw message objects as produced by the LLM API. The
   * exact structure depends on the session's protocol (OpenAI vs Anthropic),
   * so it's typed as `unknown[]` here. The `buildTraceItems` function in
   * `TurnDetailModal.tsx` handles protocol-agnostic rendering.
   */
  turn_messages: unknown[];
  /** Provider name used for this turn (e.g. "deepseek"). */
  provider: string | null;
  /** Model id used for this turn (e.g. "deepseek-chat"). */
  model: string | null;
  /** Token usage: input tokens consumed. Null if not yet available. */
  input_tokens: number | null;
  /** Token usage: output tokens generated. Null if not yet available. */
  output_tokens: number | null;
  /** Token usage: tokens served from cache. Null if not supported/not available. */
  cached_tokens: number | null;
  /** Error details if the turn failed. Null on success or while running. */
  error: Record<string, unknown> | null;
  /**
   * Mutable runtime state for the turn lifecycle.
   *
   * Contains transient data that changes during turn execution:
   * - `pending_tool_calls`: array of tool calls awaiting human approval
   * - `stream_seq`: monotonically increasing sequence number for event ordering
   * - `approval_decisions`: recorded approval/denial decisions
   *
   * Typed as `Record<string, unknown>` because the shape varies by turn state.
   * Access specific keys using `TURN_RUNTIME_KEY` constants.
   */
  runtime_state: Record<string, unknown>;
  created_at: string;
  /** When the turn reached a terminal state (completed or failed). Null while active. */
  completed_at: string | null;
}

/** Request body for `POST /api/sessions`. */
export interface CreateSessionRequest {
  /** Wire protocol for the session. Immutable after creation. */
  protocol: Protocol;
  /** Optional system prompt prepended to every turn. */
  system_prompt?: string;
}

/** Response from `POST /api/sessions`. */
export interface CreateSessionResponse {
  session: Session;
}

/** Cursor for paginated session listing. */
export interface SessionsPageCursor {
  /** Timestamp of the last session in the current page. */
  before_at: string;
  /** ID of the last session in the current page (disambiguates same-timestamp rows). */
  before_id: string;
}

/** Sort field for session listing. */
export type SessionsSort = 'updated_at' | 'created_at';

/** Response from `GET /api/sessions`. */
export interface SessionsPageResponse {
  sessions: Session[];
  /** Cursor to fetch the next page. Null if there are no more results. */
  next_cursor: SessionsPageCursor | null;
}

/** Request body for `POST /api/sessions/batch-delete`. */
export interface BatchDeleteRequest {
  /** Session ids to delete. Non-empty, max 100. */
  ids: string[];
}

/** Response from `POST /api/sessions/batch-delete`. */
export interface BatchDeleteResponse {
  /** Number of sessions actually deleted. */
  deleted: number;
}

/** Request body for `POST /api/sessions/{id}/turns`. */
export interface CreateTurnRequest {
  /** Parent turn to fork from. If omitted, appends to the latest turn. */
  parent_turn_id?: string;
  /** The user's message text. */
  user_text: string;
  /** Provider to use (must match a configured provider name). */
  provider: string;
  /** Model id to use (must be listed under the provider's models). */
  model: string;
}

/** Response from `POST /api/sessions/{id}/turns`. */
export interface CreateTurnResponse {
  turn: Turn;
}

/** Response from `GET /api/sessions/{id}/tree`. */
export interface TreeResponse {
  /** All turns in the session, flat list. The frontend reconstructs the tree
   *  client-side using `parent_turn_id` references. */
  turns: Turn[];
}

/** Possible approval decisions for a pending tool call. */
export type ApproveDecisionKind = 'allow' | 'allow_always' | 'deny';

/** Request body for `POST /api/sessions/{id}/turns/{turn_id}/approve`. */
export interface ApproveTurnRequest {
  /** One decision per pending tool call. */
  decisions: Array<{
    /** The pending_call_id from runtime_state.pending_tool_calls. */
    pending_call_id: string;
    /** The approval decision: allow (once), allow_always (auto-approve future
     *  calls to this tool), or deny (reject the tool call). */
    decision: ApproveDecisionKind;
  }>;
}
