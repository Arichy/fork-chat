use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::error::{AppError, Result};

/// Create a PostgreSQL connection pool.
///
/// Uses a fixed max of 5 connections, which is sufficient for early
/// development. The pool manages connection lifecycle (creation, health
/// checks, recycling) so callers never deal with raw connections.
/// All DB functions in this crate accept `&PgPool` rather than owning
/// a connection, allowing any pool-managed connection to serve any request.
pub async fn create_pool(database_url: &str) -> Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
        .map_err(|e| AppError::DatabaseError(format!("Failed to create database pool: {}", e)))
}
