use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::Protocol;

/// A conversation tree container.
///
/// Each session owns a tree of turns (via `session_id` FK on the `turns`
/// table). The session's `protocol` determines how `turn_messages` are
/// serialized and which LLM API is called. A session has at most one root
/// turn (enforced in application logic), and users can fork at any node to
/// create alternative conversation paths.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct Session {
    /// Unique identifier for this session.
    pub id: Uuid,

    /// User-defined title for the session. `None` until the user (or an
    /// auto-title generation step) sets one.
    pub title: Option<String>,

    /// System-level instructions injected into every LLM call in this session.
    /// `None` means no system prompt is prepended.
    pub system_prompt: Option<String>,

    /// The LLM protocol used for all turns in this session.
    ///
    /// Locked at creation time -- once a session is created with `'openai'`
    /// or `'anthropic'`, it cannot be changed. This is critical because:
    /// 1. Each turn's `turn_messages` are serialized in the protocol's native
    ///    format; switching protocols mid-conversation would produce malformed
    ///    transcripts.
    /// 2. The `response_id` column is only meaningful for OpenAI sessions.
    /// 3. The LLM adapter selection is driven by this field.
    pub protocol: Protocol,

    /// Per-session settings stored as JSONB for schema flexibility.
    ///
    /// Contains tool-related configuration like `tool_allow_rules` (which
    /// tools the user has approved for automatic execution vs. which require
    /// explicit approval). Stored as JSONB because:
    /// - The preferences schema evolves independently of the DB layer.
    /// - Different protocols may have different preference fields.
    /// - Avoids a separate preferences table for what is essentially a bag of
    ///   key-value settings.
    pub preferences: serde_json::Value,

    /// When this session was created.
    pub created_at: DateTime<Utc>,

    /// When this session was last mutated (turn created, title updated, etc.).
    /// Bumped explicitly via `touch_session_updated_at` to control sort order
    /// in the session list. Used as the primary sort key for `SessionSort::UpdatedAt`.
    pub updated_at: DateTime<Utc>,
}
