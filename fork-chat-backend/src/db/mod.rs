//! Database access layer for fork-chat.
//!
//! This module provides the data access functions for the two-table schema
//! (`sessions` + `turns`) that implements tree-structured conversations.
//!
//! ## Architecture
//!
//! The schema uses an **adjacency list** pattern: each turn references its
//! parent via `parent_turn_id`, forming a tree. Forks emerge naturally when
//! multiple turns share the same parent. Path reconstruction (root-to-leaf)
//! uses a recursive CTE that walks upward from any node to the root.
//!
//! ## Module layout
//!
//! - [`pool`] -- PostgreSQL connection pool creation.
//! - [`sessions`] -- Session CRUD with cursor-based (keyset) pagination.
//! - [`turns`] -- Turn lifecycle queries including the recursive CTE path
//!   reconstruction and a CAS-guarded update for concurrent safety.
//!
//! ## Key design decisions
//!
//! - **JSONB over normalized tables**: `turn_messages` and `runtime_state`
//!   are JSONB columns rather than separate tables, avoiding complex joins
//!   and allowing schemaless evolution of protocol-native data.
//! - **CHECK constraints over ENUMs**: Status and protocol fields use CHECK
//!   constraints so adding new values only requires application-level changes.
//! - **CAS-style guarded updates**: `update_turn_if_active` prevents stale
//!   background workers from overwriting terminal turns after cancellation.
//! - **Startup cleanup**: `fail_abandoned_turns` marks interrupted turns as
//!   failed, preventing permanently stuck UI states after backend restarts.

pub mod pool;
pub mod sessions;
pub mod turns;

pub use pool::create_pool;
pub use sessions::{
    SessionSort, create_session, delete_session, get_session, list_sessions,
    touch_session_updated_at,
};
pub use turns::{
    UpdateTurnParams, create_turn, get_path_to_turn_in_session, get_session_tree,
    get_turn_in_session, session_has_root_turn, update_turn, update_turn_if_active,
};
