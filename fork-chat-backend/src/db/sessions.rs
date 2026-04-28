use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{AppError, Result};
use crate::models::Session;

pub async fn create_session(db: &PgPool, system_prompt: Option<&str>) -> Result<Session> {
    sqlx::query_as::<_, Session>(
        r#"
        INSERT INTO sessions (system_prompt)
        VALUES ($1)
        RETURNING *
        "#,
    )
    .bind(system_prompt)
    .fetch_one(db)
    .await
    .map_err(|e| AppError::DatabaseError(format!("Failed to create session: {}", e)))
}

pub async fn get_session(db: &PgPool, id: Uuid) -> Result<Session> {
    sqlx::query_as::<_, Session>("SELECT * FROM sessions WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Session not found: {}", id)))
}

pub async fn list_sessions(db: &PgPool) -> Result<Vec<Session>> {
    sqlx::query_as::<_, Session>("SELECT * FROM sessions ORDER BY created_at DESC")
        .fetch_all(db)
        .await
        .map_err(|e| AppError::DatabaseError(format!("Failed to list sessions: {}", e)))
}

pub async fn delete_session(db: &PgPool, id: Uuid) -> Result<()> {
    let result = sqlx::query("DELETE FROM sessions WHERE id = $1")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| AppError::DatabaseError(format!("Failed to delete session: {}", e)))?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Session not found: {}", id)));
    }

    Ok(())
}

pub async fn update_session_title(db: &PgPool, id: Uuid, title: &str) -> Result<()> {
    sqlx::query("UPDATE sessions SET title = $1, updated_at = now() WHERE id = $2")
        .bind(title)
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| AppError::DatabaseError(format!("Failed to update session title: {}", e)))?;
    Ok(())
}
