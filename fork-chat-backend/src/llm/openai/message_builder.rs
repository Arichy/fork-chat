//! Reconstructs an OpenAI Responses `Vec<InputItem>` from the stored turn
//! history. Falls back to user/assistant text pairs when a turn has no
//! `turn_messages` (e.g. legacy rows created before raw-item persistence landed).

use async_openai::types::responses::{EasyInputContent, EasyInputMessage, InputItem, Item, Role};
use serde_json::Value as JsonValue;

use crate::models::Turn;

/// Build the full input for a Responses-API call, given the ordered turn path
/// (root -> parent) and the new user message to append.
///
/// # Reconstruction flow
///
/// 1. For each historical turn, try to parse its `turn_messages` into OpenAI
///    `InputItem`s using `parse_turn_items`.
/// 2. If `turn_messages` is empty or unparseable (legacy turns created before
///    raw-item persistence), fall back to `build_fallback_items` which
///    synthesises a simple user/assistant text pair from the turn's
///    `user_text` and `assistant_text` columns.
/// 3. Finally, append the new user message (if any) as a simple text item.
pub fn build_input_items(history: &[Turn], new_user_content: Option<&str>) -> Vec<InputItem> {
    let mut items: Vec<InputItem> = Vec::new();

    for turn in history {
        let turn_items = parse_turn_items(turn);
        if turn_items.is_empty() {
            // Legacy turn: no raw items stored, synthesize from text columns.
            items.extend(build_fallback_items(turn));
        } else {
            items.extend(turn_items);
        }
    }

    // Append the new user message.  This is the text the user just typed;
    // it hasn't been persisted as a turn yet.
    if let Some(new_user_content) = new_user_content {
        items.push(InputItem::EasyMessage(EasyInputMessage {
            role: Role::User,
            content: EasyInputContent::Text(new_user_content.to_string()),
            ..Default::default()
        }));
    }

    items
}

/// Try to parse a turn's `turn_messages` JSON into OpenAI `InputItem`s.
///
/// `turn_messages` can be in two formats:
///
/// 1. **New transcript format** — `[{ "role": "user", "content": [...items] }]`
///    where `content` is an array of protocol-native OpenAI items/messages for
///    replay.  This is the format produced by the current adapter.
///
/// 2. **Legacy raw-items format** — a flat array of OpenAI `Item` or
///    `EasyInputMessage` JSON objects, as stored by earlier versions of the
///    codebase.
///
/// We try the transcript format first because it's the canonical format going
/// forward.  If that yields nothing, we fall back to the legacy parser.
fn parse_turn_items(turn: &Turn) -> Vec<InputItem> {
    let Some(arr) = turn.turn_messages.as_array() else {
        return Vec::new();
    };

    if arr.is_empty() {
        return Vec::new();
    }

    // Try the new transcript envelope format first.
    if let Some(items) = parse_transcript_messages(arr)
        && !items.is_empty()
    {
        return items;
    }

    // Fallback: treat the array as a flat list of raw OpenAI items.
    parse_legacy_items(arr)
}

/// Parse the canonical transcript envelope format.
///
/// The shape is: `[ { "role": "user"|"assistant", "content": [ ...items ] } ]`.
/// Each entry's `content` array contains the actual OpenAI items to replay.
///
/// # How we distinguish from legacy format
///
/// Legacy raw items always have a `"type"` field (e.g. `"type": "message"`,
/// `"type": "function_call"`).  The transcript envelope entries do *not* have
/// a `"type"` field — they have `"role"` and `"content"` instead.  We use the
/// absence of `"type"` to identify transcript entries.
fn parse_transcript_messages(arr: &[JsonValue]) -> Option<Vec<InputItem>> {
    let mut items = Vec::new();
    let mut matched = false;

    for entry in arr {
        // If this entry has a "type" field, it's a raw OpenAI output item from
        // the legacy format, not a transcript envelope — skip it.
        if entry.get("type").is_some() {
            continue;
        }
        let Some(content_arr) = entry.get("content").and_then(|v| v.as_array()) else {
            continue;
        };
        // Found at least one valid transcript entry — mark as matched so we
        // return Some(...) even if the inner items parse to nothing.
        matched = true;
        // The content array itself is a list of raw OpenAI items, so we
        // delegate to the same legacy parser to extract them.
        items.extend(parse_legacy_items(content_arr));
    }

    if matched { Some(items) } else { None }
}

/// Parse a flat array of raw OpenAI items (legacy storage format).
///
/// Each element is deserialized as one of two types:
/// - `Item`: the full-fat message/function-call items from the Responses API.
/// - `EasyInputMessage`: the simpler "just a text message" variant.
///
/// We try `Item` first because it's the more specific type; if that fails we
/// fall back to `EasyInputMessage`.
///
/// # Reasoning block filtering
///
/// Some OpenAI-compatible providers (e.g. DeepSeek) emit `"type": "reasoning"`
/// blocks in their output.  When we replay the conversation history as input
/// for a subsequent turn, the Responses API does not accept reasoning blocks
/// as valid input items.  We filter them out here to avoid API errors.
fn parse_legacy_items(arr: &[JsonValue]) -> Vec<InputItem> {
    let mut items = Vec::new();
    for value in arr {
        // Filter out reasoning blocks — they are not valid input for replay.
        if value.get("type").and_then(|v| v.as_str()) == Some("reasoning") {
            continue;
        }

        // Try to deserialize as a full Item (covers messages, function calls,
        // function call outputs, etc.).
        if let Ok(item) = serde_json::from_value::<Item>(value.clone()) {
            items.push(InputItem::Item(item));
            continue;
        }

        // Fall back to EasyInputMessage — the simpler text-only message
        // variant that the Responses API also accepts as input.
        if let Ok(easy_msg) = serde_json::from_value::<EasyInputMessage>(value.clone()) {
            items.push(InputItem::EasyMessage(easy_msg));
        }
    }
    items
}

/// Build a minimal user/assistant text pair from a turn's `user_text` and
/// `assistant_text` columns.
///
/// This is the fallback path for **legacy turns** that were created before we
/// started persisting raw protocol items in `turn_messages`.  Such turns only
/// have the plain-text columns available, so we synthesize simple
/// `EasyInputMessage` items from them.  The quality is lower (we lose tool
/// calls, structured content, etc.) but it's enough to maintain basic
/// conversation continuity.
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
