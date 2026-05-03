//! Minimal hand-written serde types for the Anthropic Messages API. We keep
//! message/content blocks as raw JSON values so unknown block schemas round-trip
//! losslessly into `turn_messages`.
//!
//! # Why hand-rolled types instead of a vendor SDK?
//!
//! The Anthropic API shape is simple enough that a full SDK is unnecessary.
//! Using raw `serde_json::Value` for content blocks means we never need to
//! update these types when Anthropic adds new block variants (thinking, image,
//! etc.) — they pass through unchanged for protocol-native storage.

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// The top-level request body for `POST /v1/messages`.
///
/// Serialized directly to JSON and sent to the Anthropic API.  Fields that are
/// optional use `#[serde(skip_serializing_if)]` to avoid sending nulls.
#[derive(Debug, Clone, Serialize)]
pub struct MessagesRequest {
    /// The model identifier (e.g. "claude-sonnet-4-20250514").
    pub model: String,
    /// Maximum number of tokens the model should generate.  **Anthropic
    /// requires this field** — unlike OpenAI, there is no default.  We use a
    /// reasonable fallback (`DEFAULT_MAX_TOKENS`) when no per-call override is
    /// provided.
    pub max_tokens: u32,
    /// System prompt.  Sent as a top-level field (not embedded in messages),
    /// per the Anthropic Messages API spec.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// The conversation history as a sequence of user/assistant messages.
    pub messages: Vec<AnthropicMessage>,
    /// Tool definitions available to the model in this turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,
}

/// A single tool definition in the Anthropic format.
///
/// Anthropic's tool schema is simpler than OpenAI's — it's just a name,
/// description, and a JSON Schema for the input.  There's no `strict` field
/// equivalent; Anthropic always validates tool inputs against the schema.
#[derive(Debug, Clone, Serialize)]
pub struct AnthropicTool {
    /// The tool name, used by the model in `tool_use` content blocks.
    pub name: String,
    /// Human-readable description that helps the model decide when to use
    /// this tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema describing the tool's input parameters.
    pub input_schema: JsonValue,
}

/// A single message in the Anthropic conversation format.
///
/// # Why is `content` a `Vec<JsonValue>`?
///
/// Anthropic's content is an array of typed blocks (text, tool_use, tool_result,
/// thinking, etc.).  We store these as raw `JsonValue` rather than a typed enum
/// for two reasons:
///
/// 1. **Protocol-native storage**: raw JSON round-trips losslessly into
///    `turn_messages`, so we never lose information when persisting or
///    replaying turns.
/// 2. **Forward compatibility**: when Anthropic adds new block types we don't
///    need to update this struct — the unknown blocks pass through as-is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessage {
    /// The speaker role: `"user"` or `"assistant"`.
    pub role: String, // "user" | "assistant"
    /// Content blocks as raw JSON.  Each block is an object with a `"type"`
    /// field (e.g. `{ "type": "text", "text": "..." }` or
    /// `{ "type": "tool_use", "id": "...", "name": "...", "input": {...} }`).
    pub content: Vec<JsonValue>,
}

/// The top-level response from `POST /v1/messages`.
///
/// Contains the model's output content blocks, token usage, and metadata.
/// Deserialized from the raw JSON returned by the Anthropic API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagesResponse {
    /// Unique response identifier assigned by Anthropic (e.g. "msg_01XYZ...").
    pub id: String,
    /// The model that actually generated the response (may differ from the
    /// requested model if Anthropic rerouted the request).
    #[allow(dead_code)]
    pub model: String,
    /// The assistant's output as an array of typed content blocks (text,
    /// tool_use, etc.).  Kept as raw JSON for protocol-native storage.
    pub content: Vec<JsonValue>,
    /// Why the model stopped generating: `"end_turn"`, `"tool_use"`,
    /// `"max_tokens"`, etc.  Useful for the caller to decide whether to
    /// continue the conversation loop.
    #[allow(dead_code)]
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// Token usage breakdown for this request.
    pub usage: Usage,
}

/// Token usage statistics returned by the Anthropic Messages API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    /// Number of tokens in the input (prompt).
    pub input_tokens: u32,
    /// Number of tokens in the output (completion).
    pub output_tokens: u32,
    /// Number of input tokens served from Anthropic's prompt cache.  When
    /// prompt caching is enabled (via cache control hints in the messages),
    /// Anthropic caches long-lived prefix tokens and reports how many were
    /// read from cache.  This is useful for cost tracking.
    #[serde(default)]
    pub cache_read_input_tokens: Option<u32>,
}
