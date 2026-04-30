use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct Turn {
    pub id: Uuid,
    pub session_id: Uuid,
    pub parent_turn_id: Option<Uuid>,
    pub retry_turn_id: Option<Uuid>,
    pub status: String,
    pub user_text: Option<String>,
    pub assistant_text: Option<String>,
    pub raw_items: JsonValue,
    /// OpenAI Responses API response.id for conversation continuity
    pub response_id: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub input_tokens: Option<i32>,
    pub output_tokens: Option<i32>,
    pub cached_tokens: Option<i32>,
    pub error: Option<JsonValue>,
    pub metadata: JsonValue,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}
