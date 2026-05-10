# Local Single-Origin Deployment

> We kept local deployment simple by letting Axum serve the built frontend bundle directly, while still auto-running migrations on startup.

## Problem

We had a repo that was comfortable to develop in, but awkward to run like a product:

- the frontend bundle hardcoded `http://localhost:3000/api`
- local usage still assumed separate frontend/backend processes
- a fresh database needed a manual migration step before the app was usable

That was fine during early feature work, but once the app felt close to complete
we needed a smoother "just run it" path for local demos and smoke testing.

## Why It's Hard

- We wanted a **single public origin** so browser requests, SSE streams, and UI routes all worked without extra setup.
- The frontend is a client-side SPA, so deep links like `/sessions/:id` must still resolve after refresh.
- The backend depends on PostgreSQL schema state, so "process started" is not enough unless migrations also run automatically.
- We wanted to simplify local deployment without changing the separate-process development workflow.

## Alternatives Considered

### Option A: Keep separate ports and document two processes

- How it works: run the backend on `:3000` and the frontend dev/build server on another port.
- Pros: minimal code changes, matches the earliest development flow.
- Cons: still feels like a developer setup, not a local deployment. It also keeps the API origin split visible in the browser.

### Option B: Teach the Rust backend to serve the frontend bundle directly (Chosen)

- How it works: build the Vite app and let Axum serve static files plus `/api/*`.
- Pros: one process, one public port, and no extra reverse-proxy config for local usage.
- Cons: the backend now owns SPA fallback behavior in addition to the API.

### Option C: Put Nginx in front of frontend and backend

- How it works: serve the compiled SPA from Nginx and proxy `/api/*` to Axum.
- Pros: clean separation between static hosting and API hosting.
- Cons: more files, more moving parts, and more local setup than this project needs right now.

## Solution

We changed the frontend so it no longer assumes one fixed backend host. In
[`fork-chat-frontend/src/api/client.ts`](../../fork-chat-frontend/src/api/client.ts),
`resolveApiBase()` now prefers an explicit `VITE_API_BASE_URL`, otherwise it
derives `/api` from the current browser origin and only falls back to the old
dev default in non-browser contexts.

```ts
export function resolveApiBase(options?: {
  envBase?: string;
  browserOrigin?: string | null;
}): string {
  const envBase = options?.envBase?.trim();
  if (envBase && envBase.length > 0) {
    return trimTrailingSlash(envBase);
  }
  if (browserOrigin && /^https?:\/\//.test(browserOrigin)) {
    return `${trimTrailingSlash(browserOrigin)}/api`;
  }
  return DEFAULT_DEV_API_BASE;
}
```

For local frontend development, [`fork-chat-frontend/vite.config.ts`](../../fork-chat-frontend/vite.config.ts)
still proxies `/api` to the Rust backend, but the target is no longer pinned
to one port. Vite reads `fork-chat-backend/config.json`, the same file the
backend uses. That preserves the same-origin browser shape in development
without forcing developers to edit the frontend config whenever the backend
port changes.

The actual single-origin serving now lives in
[`fork-chat-backend/src/routes.rs`](../../fork-chat-backend/src/routes.rs).
We split the API into a nested `/api` router and only use the static fallback
for non-API requests:

```rust
let mut app = Router::new().nest("/api", create_api_routes(state));

app = app.fallback_service(
    ServeDir::new(frontend_dist_dir)
        .not_found_service(ServeFile::new(index_path)),
);
```

That nesting detail matters. If we had attached static fallback directly to a
flat `/api/...` route table, a typo like `GET /api/whatever` could accidentally
return the SPA shell instead of an API 404. Nesting `/api` keeps API misses
inside the API router, while everything else behaves like a normal SPA host.

We also removed the last manual bootstrap step by running migrations during
backend startup in [`fork-chat-backend/src/main.rs`](../../fork-chat-backend/src/main.rs).

```rust
let db = db::create_pool(&config.database_url).await?;

// Local deploys should be able to come up from an empty Postgres volume
// without any manual `sqlx migrate run` step.
sqlx::migrate!("./migrations").run(&db).await?;
```

Finally, the repo-level [`justfile`](../../justfile)
became the local operator entrypoint. The key improvement was splitting
deployment-style commands from split development commands:

- `just build` compiles the frontend bundle and backend release binary once
- `just run` starts Postgres and launches the already-built backend
- `just up` is just the convenience wrapper for `build + run`
- `just dev` starts the backend and Vite together for active frontend/backend development

That separation matters because local restart time was dominated by repeating
`pnpm build` and `cargo run --release` on every launch, even when the user only
wanted to bounce the service. At the same time, the project still needed a
fast split-dev path, so `dev` stays on the incremental toolchain
(`cargo run` + `vite dev`) instead of forcing iteration through release builds.
Reading the backend port from config was part of making that workflow real:
otherwise the frontend still had one hidden "3000" assumption even after the
repo-level command surface had been unified.

One follow-up detail mattered for day-to-day usability: `just dev` also needed
to own process cleanup, not just process startup. The wrapper shell now keeps
Vite in the foreground instead of `exec`-ing into it, so its `EXIT` trap can
still reap the background Rust backend when the paired dev session stops.

We still reused the backend-local Docker orchestration in
[`fork-chat-backend/justfile`](../../fork-chat-backend/justfile),
including the healthcheck-based readiness wait. That check moved away from
`docker compose exec ... pg_isready` because some macOS Docker setups could
deliver `SIGHUP` to the non-interactive `compose exec` path even while the
container itself was perfectly healthy.

## Key Takeaways

- Single-origin deployment simplified both browser fetches and SSE streaming.
- For this project's local workflow, Axum static serving was simpler than adding a separate reverse proxy.
- Separating `build` from `run` made local restarts much faster without giving up a one-command first launch.
- Keeping `dev` separate from `run` preserved fast incremental feedback for split frontend/backend work.
- Reading backend port settings from the backend config kept the frontend proxy honest during split development.
- Automatic migrations are part of deployment ergonomics, not just backend internals.
- Nesting `/api` before adding SPA fallback prevented frontend routing from swallowing API 404s.
- Reusing Docker healthchecks for readiness was more robust than probing via `compose exec`.

## References

- [`../../justfile`](../../justfile) — root-level `build` / `run` / `up` / `dev` workflow
- [`../../fork-chat-backend/justfile`](../../fork-chat-backend/justfile) — backend-local Postgres orchestration and DB utilities
- [`../../fork-chat-frontend/src/api/client.ts`](../../fork-chat-frontend/src/api/client.ts) — runtime API base resolution
- [`../../fork-chat-frontend/vite.config.ts`](../../fork-chat-frontend/vite.config.ts) — same-origin local dev proxy
- [`../../fork-chat-backend/src/routes.rs`](../../fork-chat-backend/src/routes.rs) — nested `/api` router plus SPA fallback
- [`../../fork-chat-backend/src/main.rs`](../../fork-chat-backend/src/main.rs) — startup migration hook
