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

use crate::config::AppState;
use crate::handlers::{
    approve_turn_handler, cancel_turn_handler, create_session_handler, create_turn_handler,
    delete_session_handler, get_config_handler, get_session_handler, get_session_tree_handler,
    get_turn_handler, list_sessions_handler, retry_turn_handler, stream_turn_handler,
    update_session_handler,
};

/// Build the complete axum `Router` with all API routes mounted.
///
/// All routes share the same `AppState` via `.with_state(state)`, which makes
/// the database pool, config, stream hub, and task manager available to every
/// handler through axum's `State` extractor.
pub fn create_routes(state: AppState) -> Router {
    Router::new()
        // --- Config ---
        // Read-only endpoint for the frontend to discover available protocols,
        // providers, models, and tools at startup.
        .route("/api/config", get(get_config_handler))
        // --- Sessions CRUD ---
        .route("/api/sessions", post(create_session_handler))
        .route("/api/sessions", get(list_sessions_handler))
        .route("/api/sessions/{id}", get(get_session_handler))
        .route("/api/sessions/{id}", delete(delete_session_handler))
        .route("/api/sessions/{id}", patch(update_session_handler))
        // --- Turns ---
        // Create a new turn (starts the LLM call asynchronously).
        .route("/api/sessions/{id}/turns", post(create_turn_handler))
        // Fetch the full turn tree for a session (all turns, flat list).
        .route("/api/sessions/{id}/tree", get(get_session_tree_handler))
        // Fetch a single turn by id.
        .route("/api/sessions/{id}/turns/{turn_id}", get(get_turn_handler))
        // Retry a failed turn (creates a new turn that re-executes from the
        // same parent context).
        .route(
            "/api/sessions/{id}/turns/{turn_id}/retry",
            post(retry_turn_handler),
        )
        // SSE stream for live turn updates. Uses GET because `EventSource`
        // only supports GET requests — the turn is already created by the
        // time the client subscribes.
        .route(
            "/api/sessions/{id}/turns/{turn_id}/stream",
            get(stream_turn_handler),
        )
        // Submit an approval decision for a turn awaiting human approval.
        .route(
            "/api/sessions/{id}/turns/{turn_id}/approve",
            post(approve_turn_handler),
        )
        // Cancel a running or awaiting-approval turn.
        .route(
            "/api/sessions/{id}/turns/{turn_id}/cancel",
            post(cancel_turn_handler),
        )
        .with_state(state)
}
