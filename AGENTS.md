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

# No backward compatibility

This project is currently in early development stage, so breaking changes are **encouraged** if they're better.

# Case Studies (Required)

This project documents technical highlights and challenges in `docs/case-studies/`. As an AI agent, you MUST follow these rules:

1. **New challenges/highlights**: When you encounter or implement a non-trivial technical solution (e.g. a tricky architecture decision, a clever workaround, a complex data model), proactively invoke the `case-study` skill to summarize it into a new file under `docs/case-studies/`. Do this after the feature or fix is complete, not before.

2. **Update existing case studies**: When you modify code that is already covered by a case study in `docs/case-studies/`, you MUST also update the corresponding case study document to keep it in sync. Check `docs/case-studies/` before committing to see if any existing docs reference the area you changed.

In short: new hard problem → write a case study; changed an already-documented area → update its case study.

# Code Style: Comments

- Do NOT only write documentation comments on function signatures.
- You MUST add inline comments inside function bodies.

## When to add inline comments

For any non-trivial logic, you MUST explain:

- why the logic exists (not just what it does)
- edge cases being handled
- invariants or assumptions
- tricky control flow or branching
- non-obvious performance considerations

## What counts as "non-trivial logic"

You MUST add inline comments for:

- complex conditionals (if/else with business logic)
- loops with non-obvious behavior
- state mutations
- error handling branches
- concurrency / async logic
- parsing / transformation logic

## Style of inline comments

- Prefer short comments above the code block they explain
- Focus on "why", not just "what"
- Avoid obvious comments like `// increment i`
