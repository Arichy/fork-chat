//! HTTP request handlers, organized by resource.
//!
//! - [`config`] — read-only endpoint that exposes available protocols,
//!   providers, models, and tools for frontend UI initialization.
//! - [`health`] — liveness/readiness probe endpoint for orchestration.
//! - [`sessions`] — CRUD handlers for conversation sessions.
//! - [`turns`] — turn lifecycle handlers: create, read, retry, stream (SSE),
//!   approve, and cancel.
//!
//! All handlers accept [`AppState`](crate::config::AppState) via axum's
//! `State` extractor and return `Result<Json<T>, AppError>`.

pub mod config;
pub mod health;
pub mod sessions;
pub mod turns;

pub use config::get_config_handler;
pub use health::healthz_handler;
pub use sessions::{
    batch_delete_sessions_handler, create_session_handler, delete_session_handler,
    get_session_handler, list_sessions_handler, update_session_handler,
};
pub use turns::{
    approve_turn_handler, cancel_turn_handler, create_turn_handler, get_session_tree_handler,
    get_turn_handler, retry_turn_handler, stream_turn_handler,
};
