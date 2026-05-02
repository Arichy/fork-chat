# Chat Tree Backend Design

> This is an early bootstrap design doc.
> Current normative behavior is defined in:
> - `specs/multi-protocol.md`
> - `specs/tool-use.md`
> If this file conflicts with those docs, follow the newer docs.

## Context

Backend service for fork-chat project with tree-structured conversations.
Current implementation supports both OpenAI and Anthropic protocols.

**Stack**: Rust + Axum + tracing + eyre + PostgreSQL + async-openai

**Challenges**:
- Tree structure storage and queries
- Session state management (running/awaiting_approval/completed/failed)
- OpenAI API integration with tool calls support

---

## 1. Data Models

### 1.1 Database Schema (PostgreSQL)

Based on actual migration `20260421150559_init.sql`:

```sql
-- Sessions (conversation trees)
CREATE TABLE sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title TEXT,
    system_prompt TEXT,
    protocol TEXT NOT NULL CHECK (protocol IN ('openai', 'anthropic')),
    preferences JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Turns (nodes in the tree)
CREATE TABLE turns (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    parent_turn_id UUID REFERENCES turns(id) ON DELETE SET NULL,
    retry_turn_id UUID REFERENCES turns(id) ON DELETE SET NULL,
    status TEXT NOT NULL CHECK (status IN ('running', 'awaiting_approval', 'completed', 'failed')),
    user_text TEXT,              -- User input (display/search)
    assistant_text TEXT,         -- Final AI response (display/search)
    turn_messages JSONB NOT NULL DEFAULT '[]',  -- Protocol-native transcript for this turn
    response_id TEXT,
    provider TEXT,
    model TEXT,
    input_tokens INT,
    output_tokens INT,
    cached_tokens INT,
    error JSONB,
    runtime_state JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ
);

-- Indexes
CREATE INDEX idx_turns_session_id ON turns(session_id);
CREATE INDEX idx_turns_parent_turn_id ON turns(parent_turn_id);
CREATE INDEX idx_turns_session_created ON turns(session_id, created_at);
```

### 1.2 Design Notes

- **`turn_messages`**: JSONB array containing protocol-native transcript entries
  - Includes user/assistant/tool blocks appended per internal round
  - Source of truth for replay
- **`user_text`/`assistant_text`**: Simplified text for display and search
- **`status`**: Supports active loop (`running`), approval pause
  (`awaiting_approval`), success (`completed`), and failures (`failed`)
- **Token tracking**: `input_tokens`, `output_tokens`, `cached_tokens` for cost monitoring
- **`system_prompt`**: Stored in session, prepended to messages when calling API

---

## 2. API Design (Axum)

### 2.1 RESTful Endpoints

```
POST   /api/sessions                  Create new session
GET    /api/sessions                  List all sessions
GET    /api/sessions/:id              Get session details
DELETE /api/sessions/:id              Delete session
GET    /api/config                    Public protocol/provider/tool config

POST   /api/sessions/:id/turns        Create new turn (continue conversation)
GET    /api/sessions/:id/tree         Get full tree structure
GET    /api/sessions/:id/turns/:id    Get specific turn details
POST   /api/sessions/:id/turns/:id/retry
GET    /api/sessions/:id/turns/:id/stream
POST   /api/sessions/:id/turns/:id/approve
POST   /api/sessions/:id/turns/:id/cancel
```

### 2.2 Request/Response Structures

```rust
// Create session request
#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub protocol: String,              // "openai" | "anthropic"
    pub system_prompt: Option<String>,  // Optional system prompt
}

// Create session response
#[derive(Debug, Serialize)]
pub struct CreateSessionResponse {
    pub session: Session,
}

// Create turn request (continue conversation)
#[derive(Debug, Deserialize)]
pub struct CreateTurnRequest {
    pub parent_turn_id: Option<Uuid>,  // null = root turn (first in session)
    pub user_text: String,             // user input
    pub provider: String,              // "openai" or "anthropic"
    pub model: String,                 // model to use
}

// Create turn response
#[derive(Debug, Serialize)]
pub struct CreateTurnResponse {
    pub turn: Turn,
}

// Turn structure (matches database schema)
#[derive(Debug, Serialize)]
pub struct Turn {
    pub id: Uuid,
    pub session_id: Uuid,
    pub parent_turn_id: Option<Uuid>,
    pub retry_turn_id: Option<Uuid>,
    pub status: String,  // running, awaiting_approval, completed, failed
    pub user_text: Option<String>,
    pub assistant_text: Option<String>,
    pub turn_messages: serde_json::Value,  // protocol-native transcript
    pub response_id: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub input_tokens: Option<i32>,
    pub output_tokens: Option<i32>,
    pub cached_tokens: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

// Session structure
#[derive(Debug, Serialize)]
pub struct Session {
    pub id: Uuid,
    pub title: Option<String>,  // generated after first turn completes
    pub system_prompt: Option<String>,
    pub protocol: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

### 2.3 Title Generation Flow

Current lightweight behavior:
1. If session title is empty, derive it from the first user text (truncated).
2. Persist to `session.title`.
3. Display in session list.

---

## 3. Adapter Layer

OpenAI and Anthropic adapters share the same high-level contract and are chosen
by `(session.protocol, provider)`.

```rust
use async_openai::{Client, types::CreateChatCompletionRequestArgs};
use async_openai::config::OpenAIConfig;

pub struct OpenaiAdapter {
    client: Client<OpenAIConfig>,
}

impl OpenaiAdapter {
    pub fn new(api_key: &str) -> Self {
        let config = OpenAIConfig::new().with_api_key(api_key);
        Self { client: Client::with_config(config) }
    }

    pub async fn send(
        &self,
        messages: Vec<ChatCompletionRequestMessage>,
        model: &str,
    ) -> Result<Vec<ChatCompletionRequestMessage>> {
        let request = CreateChatCompletionRequestArgs::default()
            .model(model)
            .messages(messages)
            .build()?;

        let response = self.client.chat().create(request).await?;

        // Build turn_messages: protocol-native transcript entries
        // Note: response may include tool calls, handle multiple messages
        let turn_messages = ...;  // TODO: handle tool calls

        Ok(turn_messages)
    }
}
```

### 3.1 Message Assembly Logic

To continue a conversation, merge messages from all parent turns:

```rust
pub async fn build_messages_for_turn(
    db: &PgPool,
    session: &Session,
    parent_turn_id: Option<Uuid>,
    new_user_content: &str,
) -> Result<Vec<ChatCompletionRequestMessage>> {
    // Get path from root to parent turn
    let turns = get_path_to_turn(db, parent_turn_id).await?;

    // Merge all turn_messages entries from parent turns
    let history: Vec<ChatCompletionRequestMessage> = turns
        .iter()
        .flat_map(|t| {
            // Deserialize turn_messages from JSONB
            serde_json::from_value::<Vec<ChatCompletionRequestMessage>>(t.turn_messages.clone())
                .unwrap_or_default()
        })
        .collect();

    // Prepend system prompt (if exists)
    let system = if let Some(prompt) = &session.system_prompt {
        vec![ChatCompletionRequestMessage::System(
            ChatCompletionRequestSystemMessageArgs::default()
                .content(prompt)
                .build()?
        )]
    } else {
        vec![]
    };

    // Append new user message
    let new_user = ChatCompletionRequestMessage::User(
        ChatCompletionRequestUserMessageArgs::default()
            .content(new_user_content)
            .build()?
    );

    Ok(system.into_iter().chain(history).chain(vec![new_user]).collect())
}
```

---

## 4. Tree Operations

### 4.1 Get Path to Turn (Root to Turn)

```rust
pub async fn get_path_to_turn(
    db: &PgPool,
    turn_id: Option<Uuid>,  // None returns empty (root level)
) -> Result<Vec<Turn>> {
    if let Some(id) = turn_id {
        sqlx::query_as!(
            Turn,
            r#"
            WITH RECURSIVE path AS (
                SELECT * FROM turns WHERE id = $1
                UNION ALL
                SELECT t.* FROM turns t
                JOIN path p ON t.id = p.parent_turn_id
            )
            SELECT * FROM path ORDER BY created_at ASC
            "#,
            id
        )
        .fetch_all(db)
        .await
        .map_err(|e| eyre!("Failed to get path: {}", e))
    } else {
        Ok(vec![])  // No parent = starting from root
    }
}
```

### 4.2 Get Full Tree

```rust
pub async fn get_session_tree(
    db: &PgPool,
    session_id: Uuid,
) -> Result<Vec<Turn>> {
    sqlx::query_as!(
        Turn,
        "SELECT * FROM turns WHERE session_id = $1 ORDER BY created_at",
        session_id
    )
    .fetch_all(db)
    .await
    .map_err(|e| eyre!("Failed to get tree: {}", e))
}
```

### 4.3 Create Turn Node

```rust
pub async fn create_turn(
    db: &PgPool,
    session_id: Uuid,
    parent_turn_id: Option<Uuid>,
    user_text: &str,
    status: &str,
) -> Result<Turn> {
    sqlx::query_as!(
        Turn,
        r#"
        INSERT INTO turns (session_id, parent_turn_id, status, user_text)
        VALUES ($1, $2, $3, $4)
        RETURNING *
        "#,
        session_id,
        parent_turn_id,
        status,
        user_text
    )
    .fetch_one(db)
    .await
    .map_err(|e| eyre!("Failed to create turn: {}", e))
}
```

---

## 5. Project Structure

```
fork-chat-backend/
├── Cargo.toml
├── .env                          # OPENAI_API_KEY, DATABASE_URL
├── migrations/
│   └── 20260421150559_init.sql   # PostgreSQL schema (already exists)
├── src/
│   ├── main.rs                   # Axum entry + tracing
│   ├── config.rs                 # Config management
│   ├── error.rs                  # eyre error handling
│   ├── db/
│   │   ├── mod.rs
│   │   ├── pool.rs               # PostgreSQL pool
│   │   ├── turns.rs              # Turn CRUD + tree operations
│   │   └── sessions.rs           # Session CRUD
│   ├── models/
│   │   ├── mod.rs
│   │   ├── session.rs            # Session struct
│   │   └── turn.rs               # Turn struct
│   ├── openai/
│   │   ├── mod.rs
│   │   ├── adapter.rs            # async-openai wrapper
│   │   ├── message_builder.rs    # Message assembly logic
│   ├── handlers/
│   │   ├── mod.rs
│   │   ├── sessions.rs           # Session routes (create with first turn)
│   │   ├── turns.rs              # Turn routes (create, get)
│   ├── routes.rs                 # Route assembly
```

---

## 6. Dependencies (Cargo.toml) - Latest Versions

```toml
[package]
name = "fork-chat-backend"
version = "0.1.0"
edition = "2024"

[dependencies]
# Web framework
axum = "0.8.9"
tokio = { version = "1.52.1", features = ["full"] }
tower-http = { version = "0.6.8", features = ["cors", "trace"] }

# OpenAI API
async-openai = "0.35.0"

# Database
sqlx = { version = "0.8", features = ["runtime-tokio", "postgres", "uuid", "chrono", "json"] }

# Serialization
serde = { version = "1.0.228", features = ["derive"] }
serde_json = "1.0.149"

# ID and Time
uuid = { version = "1.23.1", features = ["v4", "serde"] }
chrono = { version = "0.4.44", features = ["serde"] }

# Error handling
eyre = "0.6.12"
color-eyre = "0.6.12"

# Logging
tracing = "0.1.44"
tracing-subscriber = { version = "0.3.23", features = ["env-filter"] }

# HTTP client
reqwest = "0.13.2"

# Config
dotenvy = "0.15.7"
```

---

## 7. Files to Create

| File | Purpose |
|------|---------|
| `fork-chat-backend/Cargo.toml` | Dependencies |
| `fork-chat-backend/.env` | Env vars (OPENAI_API_KEY, DATABASE_URL) |
| `fork-chat-backend/src/main.rs` | Axum entry + tracing |
| `fork-chat-backend/src/config.rs` | Config from env |
| `fork-chat-backend/src/error.rs` | eyre to Axum response |
| `fork-chat-backend/src/models/session.rs` | Session struct |
| `fork-chat-backend/src/models/turn.rs` | Turn struct |
| `fork-chat-backend/src/db/*.rs` | Database operations |
| `fork-chat-backend/src/openai/*.rs` | async-openai wrapper + message builder |
| `fork-chat-backend/src/handlers/*.rs` | API handlers |
| `fork-chat-backend/src/routes.rs` | Route assembly |

**Note**: Migration file already exists at `migrations/20260421150559_init.sql`

---

## 8. Verification

1. **DB Migration**: Use sqlx-cli for migrations
2. **Unit Tests**: Adapter tests with mock HTTP
3. **Integration Tests**: testcontainers for PostgreSQL
4. **API Tests**: curl/HTTPie for all endpoints
5. **Logging**: tracing to stdout, verify request flow
