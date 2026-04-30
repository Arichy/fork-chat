//! Reconstruct an Anthropic `Vec<AnthropicMessage>` from the ordered turn
//! history. New rows store the full Messages API response JSON in `turn_messages`;
//! older rows may store just the assistant-side `content` array. We support
//! both formats for replay.

use serde_json::Value as JsonValue;
use serde_json::json;

use crate::models::Turn;

use super::types::AnthropicMessage;

/// Build the full `messages` payload for a new Messages-API call.
pub fn build_messages(history: &[Turn], new_user_content: &str) -> Vec<AnthropicMessage> {
    let mut msgs: Vec<AnthropicMessage> = Vec::new();

    for turn in history {
        // New format: explicit per-turn message transcript.
        if let Some(turn_msgs) = parse_transcript_messages(&turn.turn_messages) {
            msgs.extend(turn_msgs);
            continue;
        }

        // User half of the turn (if any).
        if let Some(user_text) = &turn.user_text {
            msgs.push(AnthropicMessage {
                role: "user".to_string(),
                content: vec![json!({
                    "type": "text",
                    "text": user_text
                })],
            });
        }

        // Assistant half: prefer native stored `turn_messages`.
        // Supported formats:
        // 1) Legacy full response object, with `content: [...]`
        // 2) Legacy direct `content` array
        // Fallback to flat assistant_text for legacy/failed rows.
        let assistant_content = parse_assistant_content(&turn.turn_messages).unwrap_or_else(|| {
            match &turn.assistant_text {
                Some(text) if !text.is_empty() => {
                    vec![json!({ "type": "text", "text": text.clone() })]
                }
                _ => vec![],
            }
        });

        if !assistant_content.is_empty() {
            msgs.push(AnthropicMessage {
                role: "assistant".to_string(),
                content: assistant_content,
            });
        }
    }

    msgs.push(AnthropicMessage {
        role: "user".to_string(),
        content: vec![json!({
            "type": "text",
            "text": new_user_content
        })],
    });

    msgs
}

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

fn parse_assistant_content(raw: &JsonValue) -> Option<Vec<JsonValue>> {
    // Legacy format: whole response object stored in `turn_messages`.
    if let Some(arr) = raw.get("content").and_then(|v| v.as_array()) {
        if arr.is_empty() {
            return None;
        }
        return Some(arr.clone());
    }

    // Legacy format: assistant `content` array directly stored in `turn_messages`.
    let arr = raw.as_array()?;
    if arr.is_empty() {
        return None;
    }
    Some(arr.clone())
}
