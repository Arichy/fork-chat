# ForkChat

A chat app where **every conversation is a tree**. Each turn is a node; you can fork from any node and explore a different branch. Each path from the root is an independent context sent to the LLM. Turns can also invoke backend-hosted tools (`read`, `write`, `bash`) and loop for multiple rounds inside a single node, with a per-call approval UI for anything potentially destructive.

![Preview](./preview.png)

## Stack

| Layer    | Tech                                                                                       |
| -------- | ------------------------------------------------------------------------------------------ |
| Frontend | React 19 · Vite · TanStack Router · TanStack Query · shadcn/ui · zustand · xyflow (tree view) |
| Backend  | Rust · Axum · sqlx · PostgreSQL · async-openai (OpenAI Responses API) + custom Anthropic client |
| Tooling  | pnpm · biome · bacon · sqlx-cli · just · cargo-nextest                                       |

Two LLM protocols are supported: **OpenAI Responses API** and **Anthropic Messages API**. A single session is pinned to one protocol at creation time. Tool-calling is wired through both protocols (OpenAI function calling / Anthropic `tool_use`). For design details, see [specs/](specs/); for architectural deep-dives, see [docs/case-studies/](docs/case-studies/).

## Repository layout

```
fork-chat/
├── fork-chat-backend/   # Axum + Postgres service
│   ├── migrations/      # sqlx migrations (single init migration in early dev)
│   └── src/             # handlers, db, llm adapters (openai + anthropic), tooling, turn lifecycle
├── fork-chat-frontend/  # Vite + React app
│   └── src/             # pages, routes, components, store, api, hooks (SSE turn stream)
├── specs/               # design notes (init.md, multi-protocol.md, tool-use.md)
├── docs/case-studies/   # architectural deep-dives
├── AGENTS.md            # agent guidance
└── CLAUDE.md            # agent guidance
```

## Prerequisites

- Node.js ≥ 20 and `pnpm`
- Rust (stable) and `cargo`
- [`just`](https://github.com/casey/just) for the repo-level local workflow
- PostgreSQL 14+ for local development, or Docker for `just db-up`
- `sqlx-cli` (`cargo install sqlx-cli --no-default-features --features postgres`)
- Docker for backend integration tests (`testcontainers`)
- Optional: [`bacon`](https://github.com/Canop/bacon), [`cargo-nextest`](https://nexte.st/)

## Local deployment

If you want the app to run like a finished product locally, the repo now uses a
root [justfile](justfile) to separate
"build once" from "restart quickly".

```bash
pnpm --dir fork-chat-frontend install
cp fork-chat-backend/config.example.json fork-chat-backend/config.json
cp fork-chat-backend/.env.example fork-chat-backend/.env
# fill in the provider API keys/models you want to use

just build
just run
# app: http://127.0.0.1:<backend port from config.json, default 3000>
```

Notes:

- `just build` compiles the frontend bundle and the backend release binary.
- `just run` starts local Postgres and launches the already-built backend
  binary, so restart time stays low.
- `just up` is the convenience "build then run" path for the first launch.
- Stop the backend with `Ctrl-C`, then run `just db-down` if you also want to
  stop the local Postgres container.
- The backend now runs SQL migrations automatically on startup, so a fresh
  local deploy does not need a separate `sqlx migrate run`.
- When `fork-chat-frontend/dist/index.html` exists, Axum serves the built SPA
  directly and uses `index.html` as the fallback for client-side routes.
- If startup fails with `migration ... was previously applied but has been modified`,
  run `just db-nuke` and then `just run`.

## Development setup

For split frontend/backend development from the repo root:

```bash
just dev
```

That starts:

- backend API on `http://127.0.0.1:<backend port from config.json, default 3000>`
- frontend Vite dev server on `http://127.0.0.1:5173`

If you want them in separate terminals, use `just dev-backend` and
`just dev-frontend`.

If the backend port changes, you do not need to edit the Vite config anymore.
Vite reads `fork-chat-backend/config.json` by default and keeps the `/api`
proxy in sync with `server_addr`.

```bash
# fork-chat-backend/config.json
{
  "server_addr": "0.0.0.0:4000"
}
```

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

If you already built the frontend, the backend will automatically try to serve
`../fork-chat-frontend/dist` relative to the backend crate. You can override
that path explicitly with `FORK_CHAT_FRONTEND_DIST_DIR`.

Environment variables (see [.env.example](fork-chat-backend/.env.example)):

| Variable           | Purpose                                                                             |
| ------------------ | ----------------------------------------------------------------------------------- |
| `FORK_CHAT_CONFIG` | Path to the JSON config file. Defaults to `./config.json`.                          |
| `FORK_CHAT_FRONTEND_DIST_DIR` | Optional path to the built frontend `dist` directory for Axum static serving. |
| `DATABASE_URL`     | Postgres connection string. Overrides `database_url` from the JSON file if set.     |
| `FORK_CHAT_<KEY>`  | Any JSON field can be overridden via env (use `__` as the nesting separator).       |

String values in `config.json` can also reference environment variables with
`${NAME}` placeholders. This is the recommended way to keep provider API keys
out of the JSON file:

```json
{
  "api_key": "${DEEPSEEK_API_KEY}"
}
```

### 2. Frontend

```bash
cd fork-chat-frontend
pnpm install
pnpm dev                      # http://localhost:5173
```

Vite now proxies `/api/*` to the backend port from
`fork-chat-backend/config.json`, so local frontend development still works
without hardcoding a separate API origin in the browser bundle.

Other scripts: `pnpm build`, `pnpm typecheck`, `pnpm lint`, `pnpm format`, `pnpm check` (biome lint + format), `pnpm check:fix`.

## Tool use

Turns are not a single request/response: the backend runs a multi-round loop where the model can request tool calls, the backend executes them, feeds the results back, and lets the model continue reasoning — all inside one tree node. Three tools ship in v1 (see [`fork-chat-backend/src/tooling.rs`](fork-chat-backend/src/tooling.rs)):

| Tool    | Inputs                              | Default policy     |
| ------- | ----------------------------------- | ------------------ |
| `read`  | `path`                              | `auto`             |
| `write` | `path`, `content`                   | `require_approval` |
| `bash`  | `command`, `cwd?`, `timeout_sec?`   | `require_approval` |

Permission resolution is three-layered per call:

1. Unknown tool → synthetic `is_error: true` result (`error.kind = "unknown_tool"`), loop continues.
2. Session `tool_allow_rules` — bare tool name (`write`) or `bash(pattern)` with `*` wildcards (e.g. `bash(cargo check *)`).
3. Default tool policy — `auto` runs immediately, `require_approval` suspends the turn.

When approval is needed the turn transitions to `awaiting_approval`, pending calls are persisted in `runtime_state`, and an `approval_needed` SSE event is emitted. The frontend renders one prompt per pending call with **Allow / Always allow this tool / Deny**; "always" derives a rule and appends it to `sessions.preferences.tool_allow_rules`. Denied calls produce a synthetic error result so the model can recover within the same turn. `POST /cancel` signals the background task via `CancellationToken` and drops any in-flight `bash` child (`kill_on_drop`). Tool output is truncated to 20,000 characters.

The SSE stream emits monotonically sequenced events: `turn_started`, `round_started`, `turn_snapshot`, `assistant_entry_appended`, `tool_calls`, `approval_needed`, `tool_result_appended`, `turn_completed`, `turn_failed`. A fresh `turn_snapshot` is sent on every subscribe so reconnects catch up without replay.

## Development notes

- **Backend tests:** `cargo test` runs the full backend suite. Integration tests use `testcontainers` to start isolated PostgreSQL containers, so Docker must be running. `just test` runs the same suite through `cargo nextest run`.
- **Frontend tests:** run `pnpm test:install` once to install Chromium for Vitest browser mode, then use `pnpm test:run` (alias `pnpm test`). Use `pnpm test:node` or `pnpm test:browser` to run one project, `pnpm test:watch` during development, or `pnpm test:ui` for the Vitest UI.
- **Lint:** frontend uses Biome (`pnpm check:fix`); backend uses `cargo fmt` / `cargo clippy`.
- **Pre-commit gate:** see [AGENTS.md](AGENTS.md) for the required lint/typecheck/test sequence on both sides.

## License

Not yet specified.
