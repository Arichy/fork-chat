use serde_json::Value as JsonValue;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{AppError, Result};
use crate::models::Turn;

pub async fn session_has_root_turn(db: &PgPool, session_id: Uuid) -> Result<bool> {
    let result = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM turns WHERE session_id = $1 AND parent_turn_id IS NULL)"
    )
    .bind(session_id)
    .fetch_one(db)
    .await
    .map_err(|e| AppError::DatabaseError(format!("Failed to check root turn: {}", e)))?;
    Ok(result)
}

pub async fn create_turn(
    db: &PgPool,
    session_id: Uuid,
    parent_turn_id: Option<Uuid>,
    status: &str,
    user_text: &str,
) -> Result<Turn> {
    sqlx::query_as::<_, Turn>(
        r#"
        INSERT INTO turns (session_id, parent_turn_id, status, user_text, raw_items)
        VALUES ($1, $2, $3, $4, '[]'::jsonb)
        RETURNING *
        "#,
    )
    .bind(session_id)
    .bind(parent_turn_id)
    .bind(status)
    .bind(user_text)
    .fetch_one(db)
    .await
    .map_err(|e| AppError::DatabaseError(format!("Failed to create turn: {}", e)))
}

pub async fn update_turn(
    db: &PgPool,
    id: Uuid,
    status: &str,
    assistant_text: Option<&str>,
    raw_items: &JsonValue,
    response_id: Option<&str>,
    provider: &str,
    model: &str,
    input_tokens: Option<i32>,
    output_tokens: Option<i32>,
    cached_tokens: Option<i32>,
    error: Option<&JsonValue>,
    retry_turn_id: Option<Uuid>,
) -> Result<Turn> {
    sqlx::query_as::<_, Turn>(
        r#"
        UPDATE turns SET
            status = $2,
            assistant_text = $3,
            raw_items = $4,
            response_id = $5,
            provider = $6,
            model = $7,
            input_tokens = $8,
            output_tokens = $9,
            cached_tokens = $10,
            error = $11,
            retry_turn_id = $12,
            completed_at = CASE WHEN $2 = 'completed' OR $2 = 'failed' THEN now() ELSE NULL END
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(id)
    .bind(status)
    .bind(assistant_text)
    .bind(raw_items)
    .bind(response_id)
    .bind(provider)
    .bind(model)
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(cached_tokens)
    .bind(error)
    .bind(retry_turn_id)
    .fetch_one(db)
    .await
    .map_err(|e| AppError::DatabaseError(format!("Failed to update turn: {}", e)))
}

pub async fn get_turn(db: &PgPool, id: Uuid) -> Result<Turn> {
    sqlx::query_as::<_, Turn>("SELECT * FROM turns WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Turn not found: {}", id)))
}

pub async fn get_path_to_turn(db: &PgPool, turn_id: Option<Uuid>) -> Result<Vec<Turn>> {
    if let Some(id) = turn_id {
        sqlx::query_as::<_, Turn>(
            r#"
            WITH RECURSIVE path AS (
                SELECT * FROM turns WHERE id = $1
                UNION ALL
                SELECT t.* FROM turns t
                JOIN path p ON t.id = p.parent_turn_id
            )
            SELECT * FROM path ORDER BY created_at ASC
            "#,
        )
        .bind(id)
        .fetch_all(db)
        .await
        .map_err(|e| AppError::DatabaseError(format!("Failed to get path to turn: {}", e)))
    } else {
        Ok(vec![])
    }
}

pub async fn get_session_tree(db: &PgPool, session_id: Uuid) -> Result<Vec<Turn>> {
    sqlx::query_as::<_, Turn>("SELECT * FROM turns WHERE session_id = $1 ORDER BY created_at")
        .bind(session_id)
        .fetch_all(db)
        .await
        .map_err(|e| AppError::DatabaseError(format!("Failed to get session tree: {}", e)))
}
