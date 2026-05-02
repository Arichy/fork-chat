//! Reconstruct an Anthropic `Vec<AnthropicMessage>` from the ordered turn
//! history. We only support the canonical transcript shape in
//! `turn_messages`: `[{ role, content }, ...]`.

use serde_json::Value as JsonValue;
use serde_json::json;

use crate::models::Turn;

use super::types::AnthropicMessage;

/// Build the full `messages` payload for a new Messages-API call.
pub fn build_messages(history: &[Turn], new_user_content: Option<&str>) -> Vec<AnthropicMessage> {
    let mut msgs: Vec<AnthropicMessage> = Vec::new();

    for turn in history {
        // Canonical format: explicit per-turn transcript entries.
        if let Some(turn_msgs) = parse_transcript_messages(&turn.turn_messages) {
            msgs.extend(turn_msgs);
        }
    }

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

/// Parses the canonical transcript shape into `AnthropicMessage` values.
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
        let Some(role) = entry.get("role").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(content) = entry.get("content").and_then(|v| v.as_array()) else {
            continue;
        };
        matched = true;
        out.push(AnthropicMessage {
            role: role.to_string(),
            content: content.clone(),
        });
    }

    if matched { Some(out) } else { None }
}
