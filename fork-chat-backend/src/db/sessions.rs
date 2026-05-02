use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::Protocol;
use crate::error::{AppError, Result};
use crate::models::Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSort {
    UpdatedAt,
    CreatedAt,
}

pub async fn create_session(
    db: &PgPool,
    protocol: Protocol,
    system_prompt: Option<&str>,
) -> Result<Session> {
    sqlx::query_as::<_, Session>(
        r#"
        INSERT INTO sessions (protocol, system_prompt)
        VALUES ($1, $2)
        RETURNING *
        "#,
    )
    .bind(protocol.as_str())
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

pub async fn list_sessions(
    db: &PgPool,
    limit: i64,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    sort: SessionSort,
    title_filter: Option<&str>,
) -> Result<Vec<Session>> {
    let (before_at, before_id) = cursor
        .map(|(timestamp, id)| (Some(timestamp), Some(id)))
        .unwrap_or((None, None));
    match sort {
        SessionSort::UpdatedAt => sqlx::query_as::<_, Session>(
            r#"
                SELECT * FROM sessions
                WHERE (($1::timestamptz IS NULL AND $2::uuid IS NULL)
                  OR (updated_at, id) < ($1, $2))
                  AND ($3::text IS NULL OR coalesce(title, '') ILIKE '%' || $3 || '%')
                ORDER BY updated_at DESC, id DESC
                LIMIT $4
                "#,
        )
        .bind(before_at)
        .bind(before_id)
        .bind(title_filter)
        .bind(limit)
        .fetch_all(db)
        .await
        .map_err(|e| AppError::DatabaseError(format!("Failed to list sessions page: {}", e))),
        SessionSort::CreatedAt => sqlx::query_as::<_, Session>(
            r#"
                SELECT * FROM sessions
                WHERE (($1::timestamptz IS NULL AND $2::uuid IS NULL)
                  OR (created_at, id) < ($1, $2))
                  AND ($3::text IS NULL OR coalesce(title, '') ILIKE '%' || $3 || '%')
                ORDER BY created_at DESC, id DESC
                LIMIT $4
                "#,
        )
        .bind(before_at)
        .bind(before_id)
        .bind(title_filter)
        .bind(limit)
        .fetch_all(db)
        .await
        .map_err(|e| AppError::DatabaseError(format!("Failed to list sessions page: {}", e))),
    }
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

pub async fn update_session_preferences(
    db: &PgPool,
    id: Uuid,
    preferences: &JsonValue,
) -> Result<()> {
    sqlx::query("UPDATE sessions SET preferences = $1, updated_at = now() WHERE id = $2")
        .bind(preferences)
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| {
            AppError::DatabaseError(format!("Failed to update session preferences: {}", e))
        })?;
    Ok(())
}

pub async fn touch_session_updated_at(db: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE sessions SET updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| {
            AppError::DatabaseError(format!("Failed to touch session updated_at: {}", e))
        })?;
    Ok(())
}
