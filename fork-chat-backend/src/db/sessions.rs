use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::Protocol;
use crate::error::{AppError, Result};
use crate::models::Session;

/// Sort mode for session listing.
///
/// Determines which timestamp column drives the sort order and cursor-based
/// pagination. `UpdatedAt` is the default for the session list UI (recently
/// active sessions first), while `CreatedAt` is useful for chronological
/// browsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSort {
    UpdatedAt,
    CreatedAt,
}

/// Create a new session with a given protocol and optional system prompt.
///
/// The `protocol` is locked at creation and cannot be changed later. This is
/// critical because each turn's `turn_messages` are serialized in the
/// protocol's native format; switching protocols mid-conversation would
/// produce malformed transcripts. The `preferences` column defaults to `'{}'`
/// (empty JSONB object) in the migration.
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

/// Fetch a single session by ID.
pub async fn get_session(db: &PgPool, id: Uuid) -> Result<Session> {
    sqlx::query_as::<_, Session>("SELECT * FROM sessions WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Session not found: {}", id)))
}

/// List sessions with **cursor-based (keyset) pagination**.
///
/// Unlike OFFSET-based pagination, cursor pagination uses a `(timestamp, id)`
/// tuple comparison for stable results regardless of insertions/deletions
/// between pages. The cursor represents the last row from the previous page:
///
/// ```sql
/// WHERE (updated_at, id) < (cursor_timestamp, cursor_id)
/// ORDER BY updated_at DESC, id DESC
/// ```
///
/// The `id` tiebreaker is essential because multiple sessions can share the
/// same timestamp (especially `updated_at`, which is bumped via `now()`).
/// Without it, sessions with identical timestamps would be randomly ordered
/// and could be skipped or duplicated across pages.
///
/// The `($1 IS NULL AND $2 IS NULL)` clause handles the first page (no cursor)
/// by returning all rows up to the limit. On subsequent pages, the cursor
/// values are provided and the tuple comparison takes effect.
///
/// The `title_filter` parameter performs case-insensitive ILIKE matching
/// against the session title (treating NULL titles as empty strings via
/// `coalesce`). A `NULL` filter parameter disables filtering entirely.
pub async fn list_sessions(
    db: &PgPool,
    limit: i64,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    sort: SessionSort,
    title_filter: Option<&str>,
) -> Result<Vec<Session>> {
    // Destructure the cursor into separate timestamp and ID for binding.
    // When cursor is None (first page), both become NULL and the WHERE
    // clause's NULL check returns all rows up to the limit.
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

/// Delete a session and all its turns.
///
/// The `ON DELETE CASCADE` FK on `turns.session_id` ensures all turns are
/// removed automatically when their parent session is deleted. Returns
/// `AppError::NotFound` if no session with the given ID exists.
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

/// Update the session's user-visible title.
///
/// Also bumps `updated_at` so the session moves to the top of the
/// "recently updated" list. This is the primary way users organize their
/// sessions in the sidebar.
pub async fn update_session_title(db: &PgPool, id: Uuid, title: &str) -> Result<()> {
    sqlx::query("UPDATE sessions SET title = $1, updated_at = now() WHERE id = $2")
        .bind(title)
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| AppError::DatabaseError(format!("Failed to update session title: {}", e)))?;
    Ok(())
}

/// Replace the session's preferences JSONB blob.
///
/// Used primarily for tool-related settings like `tool_allow_rules`, which
/// control which tools the user has approved for automatic execution vs.
/// which require explicit approval on each use. The entire `preferences`
/// object is replaced on each call (not merged), so the caller must provide
/// the complete preferences state.
///
/// Bumps `updated_at` to reflect the configuration change.
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

/// Bump the session's `updated_at` timestamp to `now()` without changing any
/// other field.
///
/// This is called when a new turn is created in the session so the session
/// floats to the top of the "recently updated" list. Without this, sessions
/// with new turns would remain sorted by their original `updated_at`, making
/// active sessions hard to find in a long list.
///
/// Note: turn creation does NOT cascade an `updated_at` bump to the parent
/// session via a trigger -- it's done explicitly in application code to keep
/// the control flow transparent and avoid hidden side effects.
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
