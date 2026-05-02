# Database Design: Adjacency-List Tree Structure for Forkable Conversations

> A two-table schema using `parent_turn_id` adjacency lists and recursive CTEs to represent tree-structured LLM conversations where every fork is a first-class path.

## Problem

We needed a database schema that supports tree-structured conversations: every turn is a node in a tree, users can fork at any node to explore an alternative path, and each path from root to leaf is an independent LLM conversation context. The schema must efficiently support creating turns, forking at arbitrary points, retrieving the full path from root to any leaf, and rendering the entire tree for visualization.

## Why It's Hard

- **Tree vs. linear.** Most chat applications store a flat list of messages. Our tree structure means a single parent can have multiple children (forks), and we need to traverse ancestors efficiently to reconstruct conversation history for the LLM.
- **Fork semantics.** A fork is not an explicit entity -- it emerges when two or more turns share the same `parent_turn_id`. The schema must make this natural rather than bolted on.
- **Path reconstruction.** Given a leaf turn, we need to walk up to the root collecting all ancestor turns in chronological order. This is the "active branch" that becomes the LLM's conversation context.
- **Retry vs. fork.** Retrying a turn (re-running with the same prompt) is semantically different from forking (continuing from a point with a new prompt). Both need to be represented.
- **Runtime state.** Turns have complex lifecycle state (running, awaiting approval, completed, failed) plus per-turn JSON for the persisted runtime control state that must survive reconnects and approval round-trips.

## Alternatives Considered

### Option A: Separate `turn_messages` Table

Normalize messages into a separate table with `turn_id` foreign key.

- **Pros:** Each message is queryable. Can share messages across turns (deduplication).
- **Cons:** Complex joins for every conversation reconstruction. The "per-turn transcript" model (where each turn stores the full message history up to that point) makes a separate table redundant -- we'd just be duplicating data.
- **Cons:** Harder to store protocol-native message formats that differ between OpenAI and Anthropic.

### Option B: Materialized Path (ltree)

Store the full path from root in a column (e.g., `path = "root.turn1.turn3.turn7"`) using PostgreSQL's `ltree` extension.

- **Pros:** O(1) ancestor queries -- just prefix match. Easy to find all descendants.
- **Cons:** Requires a PostgreSQL extension. Path strings become long for deep trees. Updating a path (e.g., re-parenting a node) requires updating all descendants.
- **Cons:** Overkill for our access patterns -- we primarily need "path from root to leaf" and "all nodes in a tree", both of which recursive CTEs handle well.

### Option C: Adjacency List with JSONB Transcript (Chosen)

Use `parent_turn_id` as a self-referencing foreign key. Store each turn's message transcript as a JSONB array on the turn row. Use a recursive CTE for path queries.

- **Pros:** Simple schema (two tables). Forks emerge naturally from multiple children sharing a parent. JSONB gives us schemaless flexibility for protocol-native messages. PostgreSQL's recursive CTEs make path traversal efficient.
- **Cons:** Recursive CTEs are O(depth) -- fine for conversation depth (typically < 100). No built-in "all descendants of node X" query (but we don't need it -- we load the whole tree).

## Solution

### Schema

Two tables: `sessions` and `turns`.

**Sessions:**

| Column | Type | Notes |
|---|---|---|
| `id` | UUID PK | Auto-generated |
| `title` | TEXT nullable | User-defined session title |
| `system_prompt` | TEXT nullable | System instructions for the LLM |
| `protocol` | TEXT NOT NULL | `'openai'` or `'anthropic'`, locked at creation. Uses `CHECK` constraint (not ENUM) to avoid migration pain when adding new protocols |
| `preferences` | JSONB | Per-session settings including `tool_allow_rules` |
| `created_at` | TIMESTAMPTZ | |
| `updated_at` | TIMESTAMPTZ | Bumped on every mutation |

**Turns:**

| Column | Type | Notes |
|---|---|---|
| `id` | UUID PK | Auto-generated |
| `session_id` | UUID FK -> sessions | `ON DELETE CASCADE` |
| `parent_turn_id` | UUID FK -> turns | `ON DELETE SET NULL` -- orphaning preserves history |
| `retry_turn_id` | UUID FK -> turns | `ON DELETE SET NULL` -- stored on the old turn, points to the newer retry turn |
| `status` | TEXT | `'running'`, `'awaiting_approval'`, `'completed'`, `'failed'` |
| `user_text` | TEXT nullable | What the user typed |
| `assistant_text` | TEXT nullable | Final AI text (null while running) |
| `turn_messages` | JSONB | Protocol-native transcript array |
| `response_id` | TEXT nullable | OpenAI response.id for conversation continuity |
| `provider` | TEXT nullable | Which provider handled this turn |
| `model` | TEXT nullable | Which model was used |
| `input_tokens` | INT nullable | Token usage tracking |
| `output_tokens` | INT nullable | |
| `cached_tokens` | INT nullable | |
| `error` | JSONB nullable | Structured error info on failure |
| `runtime_state` | JSONB | Stream sequence counter, pending tool calls, approval decisions |
| `created_at` | TIMESTAMPTZ | |
| `completed_at` | TIMESTAMPTZ nullable | Set when status transitions to terminal state |

### Tree Structure

The tree uses an **adjacency list** via `parent_turn_id`:

- **Root turns:** `parent_turn_id IS NULL`. A session has at most one root turn (enforced in application logic).
- **Child turns:** `parent_turn_id = <parent_uuid>`. Multiple children with the same parent represent forks.
- **Retries:** the old failed turn stores `retry_turn_id = <new_retry_uuid>`. This lets the UI hide superseded failed turns while keeping the retry relationship explicit.

**ON DELETE behavior** is deliberate:
- Deleting a session cascades to all turns.
- Deleting a turn sets its children's `parent_turn_id` to NULL (orphans them), preserving history rather than cascading destruction.

### Path Reconstruction

The key query (`get_path_to_turn_in_session` in `fork-chat-backend/src/db/turns.rs`) uses a recursive CTE that walks **upward** from a leaf to the root:

```sql
WITH RECURSIVE path AS (
    -- Base case: start at the target turn
    SELECT * FROM turns WHERE id = $1 AND session_id = $2
    UNION ALL
    -- Recursive step: walk to the parent
    SELECT t.* FROM turns t
    JOIN path p ON t.id = p.parent_turn_id
    WHERE t.session_id = $2
)
SELECT * FROM path ORDER BY created_at ASC
```

This returns every turn from root to the target leaf, ordered chronologically. The LLM adapter then uses these turns (specifically their `turn_messages`) to build the conversation history.

### Indexes

| Index | Purpose |
|---|---|
| `turns(session_id)` | Load all turns for a session (tree rendering) |
| `turns(parent_turn_id)` | Find children of a given turn |
| `turns(session_id, created_at)` | Ordered retrieval within a session |
| `turns(response_id)` | OpenAI conversation continuity lookups |

### JSONB Columns

Two JSONB columns deserve explanation:

**`turn_messages`**: A protocol-native transcript array. Each entry is a `{ role, content }` object whose inner structure matches the session's protocol (OpenAI `InputItem`/`OutputItem` or Anthropic content blocks). This is built incrementally: user message on creation, assistant response after each LLM round, tool results after execution.

**`runtime_state`**: Turn-scoped runtime control state used by streaming and the
approval lifecycle. It is intentionally separate from user-facing turn content:

- `turn_messages` stores the protocol-native transcript
- `status` / `assistant_text` / `error` store the durable visible outcome
- `runtime_state` stores the backend bookkeeping needed to resume or reconnect

Current fields:

- `stream_seq`: monotonically increasing sequence counter used as the durable
  boundary between one snapshot and subsequent live SSE events
- `pending_tool_calls`: exact tool calls currently waiting for user approval
- `approval_decisions`: previously recorded approval decisions, keyed by
  `pending_call_id`, so approval requests are idempotent

On terminal transitions, `pending_tool_calls` is cleared, while `stream_seq`
and any recorded decisions remain persisted so reconnecting clients and
follow-up approval requests can be interpreted safely.

### Startup Cleanup

`fail_abandoned_turns` runs at server startup to mark any turns left in `running` or `awaiting_approval` as `failed` with an `abandoned` error. This handles crashes or restarts that interrupt active turns.

## Key Takeaways

- Adjacency list + recursive CTE is the right choice for tree-structured conversations. It's simple, standard SQL, and efficient for our access patterns (path depth is bounded by conversation length).
- Store protocol-native data in JSONB rather than trying to normalize it. The `turn_messages` column is opaque to the DB -- the application understands the structure, and JSONB gives us schemaless flexibility.
- Use `CHECK` constraints instead of PostgreSQL ENUMs for extensible enums like `protocol` and `status`. ENUMs require migrations to add values; CHECK constraints just need an app-level change.
- Orphan-on-delete (`ON DELETE SET NULL`) is safer than cascade for tree structures -- it preserves child turns when a parent is removed.
- A single migration during early development avoids schema migration complexity. Drop-and-recreate is viable when there's no production data.

## References

- `fork-chat-backend/migrations/20260421150559_init.sql` -- Full schema definition
- `fork-chat-backend/src/db/turns.rs` -- Turn queries including recursive CTE for path reconstruction
- `fork-chat-backend/src/db/sessions.rs` -- Session CRUD with keyset pagination
- `fork-chat-backend/src/models/turn.rs` -- Turn model mapping (SQLx `FromRow`)
- `fork-chat-backend/src/models/session.rs` -- Session model with `Protocol` enum
