//! Reconstruct an Anthropic `Vec<AnthropicMessage>` from the ordered turn
//! history. We only support the canonical transcript shape in
//! `turn_messages`: `[{ role, content }, ...]`.

use serde_json::Value as JsonValue;
use serde_json::json;

use crate::models::Turn;

use super::types::AnthropicMessage;

/// Build the full `messages` payload for a new Messages-API call.
///
/// Walks the ordered turn history and reconstructs the Anthropic message array
/// by parsing each turn's `turn_messages` JSON.  Unlike the OpenAI message
/// builder, we only support the canonical transcript format — there is no
/// legacy fallback because the Anthropic adapter was added after raw-item
/// persistence was already in place.
///
/// After replaying history, appends the new user message (if present) as a
/// simple text content block.
pub fn build_messages(history: &[Turn], new_user_content: Option<&str>) -> Vec<AnthropicMessage> {
    let mut msgs: Vec<AnthropicMessage> = Vec::new();

    for turn in history {
        // Each turn stores its messages in the canonical transcript shape.
        // If parsing returns None (empty or unrecognized format), the turn is
        // simply skipped — there is no text-column fallback for Anthropic.
        if let Some(turn_msgs) = parse_transcript_messages(&turn.turn_messages) {
            msgs.extend(turn_msgs);
        }
    }

    // Append the new user message as a single text content block.  Anthropic
    // requires messages to have content as an array of typed blocks.
    if let Some(new_user_content) = new_user_content {
        msgs.push(AnthropicMessage {
            role: "user".to_string(),
            content: vec![json!({
                "type": "text",
                "text": new_user_content
            })],
        });
    }

    msgs
}

/// Parse the canonical transcript shape into `AnthropicMessage` values.
///
/// The expected format is: `[ { "role": "user"|"assistant", "content": [ ...blocks ] } ]`.
/// This mirrors the Anthropic Messages API's native message structure, which
/// is why we can clone the content array directly — it's already in the right
/// shape for round-tripping.
///
/// # Protocol-native design
///
/// Because `content` is stored as raw `Vec<JsonValue>`, any block type that
/// Anthropic defines (text, tool_use, tool_result, thinking, etc.) round-trips
/// losslessly.  We don't need to know about every possible block type here.
///
/// Returns `None` when the JSON value is not an array of `{ role, content }`
/// objects. Invalid entries are skipped; if no valid entries remain, returns
/// `None`.
fn parse_transcript_messages(raw: &JsonValue) -> Option<Vec<AnthropicMessage>> {
    let arr = raw.as_array()?;
    if arr.is_empty() {
        return None;
    }

    let mut out = Vec::new();
    let mut matched = false;
    for entry in arr {
        // Each entry must have a "role" string — "user" or "assistant".
        let Some(role) = entry.get("role").and_then(|v| v.as_str()) else {
            continue;
        };
        // Each entry must have a "content" array of typed blocks.
        let Some(content) = entry.get("content").and_then(|v| v.as_array()) else {
            continue;
        };
        matched = true;
        // Clone the content blocks directly — they're already in the
        // protocol-native Anthropic format, so no transformation needed.
        out.push(AnthropicMessage {
            role: role.to_string(),
            content: content.clone(),
        });
    }

    if matched { Some(out) } else { None }
}
