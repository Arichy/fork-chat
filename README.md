# ForkChat

A chat app where **every conversation is a tree**. Each turn is a node; you can fork from any node and explore a different branch. Each path from the root is an independent context sent to the LLM.

![Preview](./preview.png)

## Stack

| Layer    | Tech                                                                                       |
| -------- | ------------------------------------------------------------------------------------------ |
| Frontend | React 19 · Vite · TanStack Router · TanStack Query · shadcn · zustand · xyflow (tree view) |
| Backend  | Rust · Axum · sqlx · PostgreSQL · async-openai (Responses API) + native Anthropic client   |
| Tooling  | pnpm · biome · bacon · sqlx-cli · just                                                     |

Two LLM protocols are supported: **OpenAI Responses API** and **Anthropic Messages API**. A single session is pinned to one protocol at creation time.

## Repository layout

```
fork-chat/
├── fork-chat-backend/   # Axum + Postgres service
│   ├── migrations/      # sqlx migrations
│   └── src/             # handlers, db, llm adapters (openai + anthropic), models
├── fork-chat-frontend/  # Vite + React app
│   └── src/             # pages, routes, components, store, api
├── specs/               # design notes (init.md, multi-protocol.md)
├── AGENTS.md            # agent guidance
└── CLAUDE.md            # agent guidance
```

## Prerequisites

- Node.js ≥ 20 and `pnpm`
- Rust (stable) and `cargo`
- PostgreSQL 14+ for local development, or Docker for `just db-up`
- `sqlx-cli` (`cargo install sqlx-cli --no-default-features --features postgres`)
- Docker for backend integration tests (`testcontainers`)
- Optional: [`just`](https://github.com/casey/just), [`bacon`](https://github.com/Canop/bacon), [`cargo-nextest`](https://nexte.st/)

## Setup

### 1. Backend

```bash
cd fork-chat-backend
cp .env.example .env          # usually only DATABASE_URL is needed here
cp config.example.json config.json
# then edit config.json to fill in provider api keys, models, etc.
just db-up                    # optional: starts local Postgres via Docker
just reset-db                 # drops, recreates DB and runs migrations
cargo run                     # starts server on $server_addr (default 0.0.0.0:3000)
```

Configuration is driven by a JSON file (see [`config.example.json`](fork-chat-backend/config.example.json)). Providers are declared explicitly and each one advertises which protocols (`openai`, `anthropic`) it speaks and under which base URL/API key. The frontend reads the resulting provider/model/protocol matrix from `GET /api/config`.

Environment variables (see [.env.example](fork-chat-backend/.env.example)):

| Variable           | Purpose                                                                             |
| ------------------ | ----------------------------------------------------------------------------------- |
| `FORK_CHAT_CONFIG` | Path to the JSON config file. Defaults to `./config.json`.                          |
| `DATABASE_URL`     | Postgres connection string. Overrides `database_url` from the JSON file if set.     |
| `FORK_CHAT_<KEY>`  | Any JSON field can be overridden via env (use `__` as the nesting separator).       |

### 2. Frontend

```bash
cd fork-chat-frontend
pnpm install
pnpm dev                      # http://localhost:5173
```

Other scripts: `pnpm build`, `pnpm typecheck`, `pnpm lint`, `pnpm format`, `pnpm check` (biome lint + format), `pnpm check:fix`.

## Data model

Two tables (see [migrations/20260421150559_init.sql](fork-chat-backend/migrations/20260421150559_init.sql)):

- **`sessions`** — a conversation tree (id, title, system_prompt, `protocol` (`openai` | `anthropic`), metadata, timestamps). The protocol is chosen once at session creation and all turns in the session use that wire format.
- **`turns`** — a node in the tree. `parent_turn_id` defines the tree edge. Each turn stores:
  - `user_text` / `assistant_text` — the plain-text user input and the final assistant reply (null while `status = 'running'`).
  - `turn_messages` (JSONB) — the full per-turn message transcript (user/tool results + assistant replies), in the session's protocol format. This is what gets stitched together to reconstruct the context for the next call.
  - `response_id` — OpenAI Responses API `response.id` for conversation continuity (Anthropic sessions leave this null).
  - `provider`, `model`, `input_tokens`, `output_tokens`, `cached_tokens` — bookkeeping from the last call.
  - `status` (`running` / `completed` / `failed`), `error` (JSONB), `retry_turn_id` — for retry support.

To continue a branch, the backend walks from the target turn up to the root, concatenates each turn's `turn_messages`, appends the new user input, and sends the result to the model via the session's protocol adapter.

## API sketch

```
GET    /api/config                              provider/model/protocol catalog

POST   /api/sessions                            create session + first turn
GET    /api/sessions                            list sessions
GET    /api/sessions/:id                        session details
PATCH  /api/sessions/:id                        update session (e.g. title)
DELETE /api/sessions/:id                        delete session

POST   /api/sessions/:id/turns                  create turn (continue or fork)
GET    /api/sessions/:id/tree                   full tree
GET    /api/sessions/:id/turns/:turn_id         turn details
POST   /api/sessions/:id/turns/:turn_id/retry   retry a failed/completed turn
```

See [specs/init.md](specs/init.md) and [specs/multi-protocol.md](specs/multi-protocol.md) for the full design.

## Development notes

- **Backend tests:** `cargo test` runs the full backend suite. Integration tests use `testcontainers` to start isolated PostgreSQL containers, so Docker must be running. `just test` runs the same suite through `cargo nextest run`.
- **Frontend tests:** run `pnpm test:install` once to install Chromium for Vitest browser mode, then use `pnpm test:run`. Use `pnpm test:node` or `pnpm test:browser` to run one project.
- **Lint:** frontend uses Biome (`pnpm check:fix`); backend uses `cargo fmt` / `cargo clippy`.
- **Pre-commit gate:** see [AGENTS.md](AGENTS.md) for the required lint/typecheck/test sequence on both sides.

## License

Not yet specified.
