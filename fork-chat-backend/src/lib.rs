//! `fork-chat-backend` — an axum-based backend for tree-structured LLM chat sessions.
//!
//! # Crate structure
//!
//! | Module              | Responsibility                                                 |
//! |---------------------|----------------------------------------------------------------|
//! | `config`            | Application configuration, protocol enum, shared `AppState`    |
//! | `db`                | Database access layer (sqlx queries for sessions and turns)    |
//! | `error`             | Unified `AppError` enum with HTTP status mapping               |
//! | `handlers`          | Axum request handlers, organized by resource (config, sessions, turns) |
//! | `llm`               | Protocol adapter trait and per-vendor implementations          |
//! | `models`            | Domain models (`Session`, `Turn`) shared with the database layer |
//! | `routes`            | Route definitions and router construction                      |
//! | `tooling`           | Built-in tool definitions and tool execution logic             |
//! | `turn_lifecycle`    | High-level turn execution loop (create → stream → complete)    |
//! | `turn_runtime`      | Per-turn mutable runtime state (pending tool calls, approvals) |
//! | `turn_stream`       | SSE pub/sub hub for real-time turn updates to clients          |
//! | `turn_task_manager` | In-flight task tracking and cancellation for active turns      |
//!
//! The entry point is [`main`] (in `main.rs`), which loads config, connects to
//! the database, builds shared state, and starts the axum HTTP server.

pub mod config;
pub mod db;
pub mod error;
pub mod handlers;
pub mod llm;
pub mod models;
pub mod routes;
pub mod tooling;
pub mod turn_lifecycle;
pub mod turn_runtime;
pub mod turn_stream;
pub mod turn_task_manager;
