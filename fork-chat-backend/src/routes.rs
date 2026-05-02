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

pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/api/config", get(get_config_handler))
        .route("/api/sessions", post(create_session_handler))
        .route("/api/sessions", get(list_sessions_handler))
        .route("/api/sessions/{id}", get(get_session_handler))
        .route("/api/sessions/{id}", delete(delete_session_handler))
        .route("/api/sessions/{id}", patch(update_session_handler))
        .route("/api/sessions/{id}/turns", post(create_turn_handler))
        .route("/api/sessions/{id}/tree", get(get_session_tree_handler))
        .route("/api/sessions/{id}/turns/{turn_id}", get(get_turn_handler))
        .route(
            "/api/sessions/{id}/turns/{turn_id}/retry",
            post(retry_turn_handler),
        )
        .route(
            "/api/sessions/{id}/turns/{turn_id}/stream",
            get(stream_turn_handler),
        )
        .route(
            "/api/sessions/{id}/turns/{turn_id}/approve",
            post(approve_turn_handler),
        )
        .route(
            "/api/sessions/{id}/turns/{turn_id}/cancel",
            post(cancel_turn_handler),
        )
        .with_state(state)
}
