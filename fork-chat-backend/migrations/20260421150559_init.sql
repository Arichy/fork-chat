-- =============================================================================
-- Fork-Chat Schema: Adjacency-List Tree for Forkable Conversations
-- =============================================================================
--
-- Design overview:
--   Two tables (sessions + turns) represent tree-structured conversations.
--   Every turn is a node in the tree. Users can fork at any node, producing a
--   sibling with the same parent. Each root-to-leaf path is an independent LLM
--   conversation context.
--
-- Why adjacency list over materialized path (ltree)?
--   - No PostgreSQL extension required.
--   - Our access patterns are (a) "path from root to leaf" and (b) "all nodes
--     in a tree", both handled efficiently by recursive CTEs.
--   - Conversation depth is bounded (typically < 100), so O(depth) traversal
--     is negligible.
--   - Materialized path adds complexity for reparenting and is overkill when
--     we never need "all descendants of node X" queries.
-- =============================================================================

-- ---------------------------------------------------------------------------
-- sessions: one row per conversation tree
-- ---------------------------------------------------------------------------
CREATE TABLE
  sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid (),
    title TEXT,
    system_prompt TEXT,
    -- CHECK constraint (not ENUM) so adding new protocols later only requires
    -- an application-level change, not a database migration.
    protocol TEXT NOT NULL CHECK (protocol IN ('openai', 'anthropic')),
    -- JSONB for flexibility: stores tool_allow_rules and other per-session
    -- settings whose schema may evolve independently of the DB layer.
    preferences JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now (),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now ()
  );

-- ---------------------------------------------------------------------------
-- turns: tree nodes -- each row is one user/AI round
-- ---------------------------------------------------------------------------
-- A turn is a node in the tree, representing a round of user input and the
-- AI's final response (which may involve multiple function-call steps).
--
-- Tree structure:
--   parent_turn_id IS NULL  =>  root turn (session has at most one root,
--                                enforced in application logic).
--   parent_turn_id = <uuid>  =>  child turn; multiple children sharing the
--                                same parent represent forks.
--
-- Retry semantics:
--   When a user retries a failed turn, a NEW turn is created as a sibling
--   (same parent). The OLD turn's retry_turn_id is set to point to the new
--   turn. This lets the UI hide superseded failed turns while keeping the
--   retry relationship explicit. Retries are siblings, not parent/child,
--   because they replace (not extend) the original turn.
-- ---------------------------------------------------------------------------
CREATE TABLE
  turns (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid (),
    -- ON DELETE CASCADE: deleting a session removes all its turns. There is
    -- no reason to keep orphaned turns when their session is gone.
    session_id UUID NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    -- ON DELETE SET NULL: when a parent turn is deleted, its children become
    -- orphans (parent_turn_id = NULL) rather than being cascade-deleted.
    -- This preserves the user's conversation history; the application can
    -- handle orphaned nodes gracefully instead of silently losing data.
    parent_turn_id UUID REFERENCES turns (id) ON DELETE SET NULL,
    -- Points FROM the original (old) turn TO the new retry turn. Stored on
    -- the original so the UI can navigate from failed to replacement.
    retry_turn_id UUID REFERENCES turns (id) ON DELETE SET NULL,
    -- CHECK constraint (not ENUM) so adding new statuses (e.g. 'cancelled')
    -- only requires an application-level change, not a migration.
    status TEXT NOT NULL CHECK (status IN ('running', 'awaiting_approval', 'completed', 'failed')),
    user_text TEXT, -- user input
    assistant_text TEXT, -- final text from AI, would be null when running
    -- JSONB for protocol-native storage: each entry is a { role, content }
    -- object whose inner structure matches the session's protocol (OpenAI
    -- InputItem/OutputItem or Anthropic content blocks). Stored as JSONB
    -- rather than a separate messages table to avoid complex joins and to
    -- enable lossless round-tripping of protocol-specific fields that differ
    -- between providers. The DB is opaque to the structure; the application
    -- owns serialization/deserialization.
    turn_messages JSONB NOT NULL DEFAULT '[]',
    -- Stores the OpenAI Responses API response.id so that follow-up requests
    -- can reference the previous response for conversation continuity. Only
    -- populated for OpenAI protocol sessions.
    response_id TEXT,
    provider TEXT,
    model TEXT,
    input_tokens INT,
    output_tokens INT,
    cached_tokens INT,
    error JSONB,
    -- JSONB for flexible backend control state used by streaming and the
    -- approval lifecycle. Intentionally separate from user-facing content
    -- (turn_messages). Contains stream_seq (snapshot boundary counter),
    -- pending_tool_calls (tool calls awaiting approval), and
    -- approval_decisions (recorded approval answers keyed by call_id).
    -- Storing this as JSONB allows the runtime state schema to evolve
    -- without DB migrations.
    runtime_state JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now (),
    -- Set automatically via application logic when status transitions to a
    -- terminal state ('completed' or 'failed'). NULL while the turn is still
    -- in an active lifecycle state.
    completed_at TIMESTAMPTZ
  );

-- ---------------------------------------------------------------------------
-- Indexes
-- ---------------------------------------------------------------------------
-- idx_turns_session_id: supports loading all turns for a session, used by
--   get_session_tree (renders the full tree) and session-scoped queries.
CREATE INDEX idx_turns_session_id ON turns (session_id);

-- idx_turns_parent_turn_id: supports finding children of a given turn, used
--   when forking (finding existing siblings) and tree traversal.
CREATE INDEX idx_turns_parent_turn_id ON turns (parent_turn_id);

-- idx_turns_session_created: composite index for chronologically-ordered
--   retrieval within a session. Used by get_session_tree and any query that
--   needs turns in creation order (e.g. path reconstruction ordering).
CREATE INDEX idx_turns_session_created ON turns (session_id, created_at);

-- idx_turns_response_id: supports looking up turns by OpenAI response.id for
--   conversation continuity. Without this, follow-up requests that reference
--   a previous response_id would require a full table scan.
CREATE INDEX idx_turns_response_id ON turns (response_id);
