-- Add migration script here
CREATE TABLE
  sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid (),
    title TEXT,
    system_prompt TEXT,
    protocol TEXT NOT NULL CHECK (protocol IN ('openai', 'anthropic')),
    preferences JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now (),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now ()
  );

-- A turn is a node in the tree, a round of user input and AI final response (may experience multi function call steps)
CREATE TABLE
  turns (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid (),
    session_id UUID NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    parent_turn_id UUID REFERENCES turns (id) ON DELETE SET NULL,
    retry_turn_id UUID REFERENCES turns (id) ON DELETE SET NULL,
    status TEXT NOT NULL CHECK (status IN ('running', 'awaiting_approval', 'completed', 'failed')),
    user_text TEXT, -- user input
    assistant_text TEXT, -- final text from AI, would be null when running
    turn_messages JSONB NOT NULL DEFAULT '[]', -- Per-turn message transcript (user/tool results + assistant replies)
    response_id TEXT, -- OpenAI Responses API response.id for conversation continuity
    provider TEXT,
    model TEXT,
    input_tokens INT,
    output_tokens INT,
    cached_tokens INT,
    error JSONB,
    runtime_state JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now (),
    completed_at TIMESTAMPTZ
  );

-- indexes
CREATE INDEX idx_turns_session_id ON turns (session_id);

CREATE INDEX idx_turns_parent_turn_id ON turns (parent_turn_id);

CREATE INDEX idx_turns_session_created ON turns (session_id, created_at);

CREATE INDEX idx_turns_response_id ON turns (response_id);
