//! Reconstructs an OpenAI Responses `Vec<InputItem>` from the stored turn
//! history. Falls back to user/assistant text pairs when a turn has no
//! `turn_messages` (e.g. legacy rows created before raw-item persistence landed).

use async_openai::types::responses::{EasyInputContent, EasyInputMessage, InputItem, Item, Role};
use serde_json::Value as JsonValue;

use crate::models::Turn;

/// Build the full input for a Responses-API call, given the ordered turn path
/// (root → parent) and the new user message to append.
pub fn build_input_items(history: &[Turn], new_user_content: &str) -> Vec<InputItem> {
    let mut items: Vec<InputItem> = Vec::new();

    for turn in history {
        let turn_items = parse_turn_items(turn);
        if turn_items.is_empty() {
            items.extend(build_fallback_items(turn));
        } else {
            items.extend(turn_items);
        }
    }

    items.push(InputItem::EasyMessage(EasyInputMessage {
        role: Role::User,
        content: EasyInputContent::Text(new_user_content.to_string()),
        ..Default::default()
    }));

    items
}

fn parse_turn_items(turn: &Turn) -> Vec<InputItem> {
    let Some(arr) = turn.turn_messages.as_array() else {
        return Vec::new();
    };

    if arr.is_empty() {
        return Vec::new();
    }

    // New format: [{ role, content, ...meta }] where `content` is an array of
    // protocol-native OpenAI items/messages for replay.
    if let Some(items) = parse_transcript_messages(arr)
        && !items.is_empty()
    {
        return items;
    }

    parse_legacy_items(arr)
}

fn parse_transcript_messages(arr: &[JsonValue]) -> Option<Vec<InputItem>> {
    let mut items = Vec::new();
    let mut matched = false;

    for entry in arr {
        // Distinguish from legacy OpenAI output items (`type` is present there).
        if entry.get("type").is_some() {
            continue;
        }
        let Some(content_arr) = entry.get("content").and_then(|v| v.as_array()) else {
            continue;
        };
        matched = true;
        items.extend(parse_legacy_items(content_arr));
    }

    if matched { Some(items) } else { None }
}

fn parse_legacy_items(arr: &[JsonValue]) -> Vec<InputItem> {
    let mut items = Vec::new();
    for value in arr {
        if let Ok(item) = serde_json::from_value::<Item>(value.clone()) {
            items.push(InputItem::Item(item));
            continue;
        }

        if let Ok(easy_msg) = serde_json::from_value::<EasyInputMessage>(value.clone()) {
            items.push(InputItem::EasyMessage(easy_msg));
        }
    }
    items
}

fn build_fallback_items(turn: &Turn) -> Vec<InputItem> {
    let mut items = Vec::new();

    if let Some(user_text) = &turn.user_text {
        items.push(InputItem::EasyMessage(EasyInputMessage {
            role: Role::User,
            content: EasyInputContent::Text(user_text.clone()),
            ..Default::default()
        }));
    }

    if let Some(assistant_text) = &turn.assistant_text {
        items.push(InputItem::EasyMessage(EasyInputMessage {
            role: Role::Assistant,
            content: EasyInputContent::Text(assistant_text.clone()),
            ..Default::default()
        }));
    }

    items
}
