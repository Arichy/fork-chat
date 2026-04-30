use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::error::{AppError, Result};

pub async fn create_pool(database_url: &str) -> Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
        .map_err(|e| AppError::DatabaseError(format!("Failed to create database pool: {}", e)))
}
