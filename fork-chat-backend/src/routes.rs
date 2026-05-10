//! HTTP route definitions.
//!
//! The REST API surface is organized around two primary resources:
//!
//! - **Sessions** (`/api/sessions`) — CRUD for conversation containers. Each
//!   session has a locked `protocol` chosen at creation time.
//! - **Turns** (`/api/sessions/{id}/turns`) — CRUD + streaming for individual
//!   assistant/user exchanges within a session.  Turns form a tree (each turn
//!   has an optional `parent_turn_id`), enabling forking conversations at any
//!   point.
//!
//! Additional endpoints:
//!
//! - **Config** (`/api/config`) — returns available providers, protocols, and
//!   tools so the frontend can populate its UI without hardcoding anything.
//! - **Healthz** (`/healthz`) — lightweight readiness probe that also checks
//!   database reachability.
//! - **Static frontend** (`/*path`) — when a built frontend bundle is
//!   available, Axum serves it directly and falls back to `index.html` for SPA
//!   routes like `/sessions/:id`.
//!
//! # Why is streaming a GET endpoint?
//!
//! The `/stream` endpoint uses Server-Sent Events (SSE), which requires a
//! plain HTTP GET.  This is the standard pattern for SSE because the browser's
//! `EventSource` API only supports GET requests.  The turn is already created
//! (via POST to `/turns`) before streaming begins, so the GET is purely a
//! read/subscribe operation — no side effects beyond keeping the connection
//! open and pushing updates.

use axum::{
    Router,
    routing::{delete, get, patch, post},
};
use std::path::PathBuf;
use tower_http::services::{ServeDir, ServeFile};

use crate::config::AppState;
use crate::handlers::{
    approve_turn_handler, batch_delete_sessions_handler, cancel_turn_handler,
    create_session_handler, create_turn_handler, delete_session_handler, get_config_handler,
    get_session_handler, get_session_tree_handler, get_turn_handler, healthz_handler,
    list_sessions_handler, retry_turn_handler, stream_turn_handler, update_session_handler,
};

/// Resolve the frontend `dist` directory to serve, if any.
///
/// Resolution order:
/// 1. explicit `frontend_dist_dir` config / env override
/// 2. the repo's default sibling frontend build output
///
/// We require `index.html` to exist because a partial `dist` directory would
/// otherwise produce confusing 404s after startup.
fn resolve_frontend_dist_dir(configured: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = configured.map(str::trim).filter(|path| !path.is_empty()) {
        let candidate = PathBuf::from(path);
        if candidate.join("index.html").is_file() {
            return Some(candidate);
        }

        // An explicit path is operator intent, so log loudly when it doesn't
        // point at a real build output.
        tracing::warn!(
            frontend_dist_dir = %candidate.display(),
            "Configured frontend_dist_dir is missing index.html; static frontend serving disabled"
        );
        return None;
    }

    let repo_default = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fork-chat-frontend/dist");
    if repo_default.join("index.html").is_file() {
        return Some(repo_default);
    }

    None
}

/// Build the API-only router that lives under the `/api` prefix.
///
/// We attach the shared `AppState` at the top-level router so `/healthz` and
/// every nested `/api/*` route all receive the exact same state instance.
fn create_api_routes() -> Router<AppState> {
    Router::new()
        // --- Config ---
        // Read-only endpoint for the frontend to discover available protocols,
        // providers, models, and tools at startup.
        .route("/config", get(get_config_handler))
        // --- Sessions CRUD ---
        // Batch delete — must be mounted before /{id} to avoid path-param collision.
        .route(
            "/sessions/batch-delete",
            post(batch_delete_sessions_handler),
        )
        .route("/sessions", post(create_session_handler))
        .route("/sessions", get(list_sessions_handler))
        .route("/sessions/{id}", get(get_session_handler))
        .route("/sessions/{id}", delete(delete_session_handler))
        .route("/sessions/{id}", patch(update_session_handler))
        // --- Turns ---
        // Create a new turn (starts the LLM call asynchronously).
        .route("/sessions/{id}/turns", post(create_turn_handler))
        // Fetch the full turn tree for a session (all turns, flat list).
        .route("/sessions/{id}/tree", get(get_session_tree_handler))
        // Fetch a single turn by id.
        .route("/sessions/{id}/turns/{turn_id}", get(get_turn_handler))
        // Retry a failed turn (creates a new turn that re-executes from the
        // same parent context).
        .route(
            "/sessions/{id}/turns/{turn_id}/retry",
            post(retry_turn_handler),
        )
        // SSE stream for live turn updates. Uses GET because `EventSource`
        // only supports GET requests — the turn is already created by the
        // time the client subscribes.
        .route(
            "/sessions/{id}/turns/{turn_id}/stream",
            get(stream_turn_handler),
        )
        // Submit an approval decision for a turn awaiting human approval.
        .route(
            "/sessions/{id}/turns/{turn_id}/approve",
            post(approve_turn_handler),
        )
        // Cancel a running or awaiting-approval turn.
        .route(
            "/sessions/{id}/turns/{turn_id}/cancel",
            post(cancel_turn_handler),
        )
}

/// Build the complete axum `Router`.
///
/// `/api/*` always serves the JSON + SSE backend. When a built frontend bundle
/// exists, every other path is handled by `ServeDir` with an `index.html`
/// fallback so client-side routes survive page refreshes.
pub fn create_routes(state: AppState) -> Router {
    let frontend_dist_dir = resolve_frontend_dist_dir(state.config.frontend_dist_dir.as_deref());
    let mut app = Router::new()
        // Keep health checks at the root so deploy platforms don't need to
        // know about the app's `/api` namespace.
        .route("/healthz", get(healthz_handler))
        .nest("/api", create_api_routes());

    if let Some(frontend_dist_dir) = frontend_dist_dir {
        let index_path = frontend_dist_dir.join("index.html");
        tracing::info!(
            frontend_dist_dir = %frontend_dist_dir.display(),
            "Serving built frontend assets from Axum"
        );
        app = app.fallback_service(
            ServeDir::new(frontend_dist_dir).not_found_service(ServeFile::new(index_path)),
        );
    } else {
        tracing::info!("Frontend dist not found; serving API routes only");
    }

    app.with_state(state)
}
