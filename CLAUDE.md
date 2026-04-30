This is an AI agent project. The main feature is tree sessions: every turn is a tree node, users can fork at any node, and every path in the tree is a separate context for the LLM API.

# Frontend

react + vite + shadcn + tanstack-query + tanstack-router.
Please use shadcn components.

# Backend

axum server + postgresql + sqlx.
In initial development, do not create more than one migration. If you need to update the schema, you should:

1. Tell me you'll update the schema, so I can disconnect other connections to db.
2. `sqlx database drop -y` to delete existing db.
3. `sqlx database create` to create a new db.
4. `sqlx migrate run` to run the new migration.

# Pre-commit Quality Gate (Required)

Before every commit, run both frontend and backend checks. Do not commit if any step fails.

Frontend (`fork-chat-frontend`):

1. `pnpm format`
2. `pnpm lint`
3. `pnpm typecheck`
4. `pnpm test`

Backend (`fork-chat-backend`):

1. `cargo fmt --all --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo nextest run` (or `cargo test` if nextest is unavailable)

# Tests

Write detailed test cases after feature implementation or bug fix if possible.
