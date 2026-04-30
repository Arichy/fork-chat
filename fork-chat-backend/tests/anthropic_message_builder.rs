//! Unit tests for Anthropic message replay. Ensures we can rebuild request
//! history from both legacy and new `turn_messages` shapes.

use chrono::Utc;
use fork_chat_backend::llm::anthropic::message_builder::build_messages;
use fork_chat_backend::models::Turn;
use serde_json::{Value, json};
use uuid::Uuid;

fn turn(user_text: Option<&str>, assistant_text: Option<&str>, turn_messages: Value) -> Turn {
    Turn {
        id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        parent_turn_id: None,
        retry_turn_id: None,
        status: "completed".to_string(),
        user_text: user_text.map(str::to_string),
        assistant_text: assistant_text.map(str::to_string),
        turn_messages,
        response_id: None,
        provider: Some("anthropic".to_string()),
        model: Some("claude-sonnet-4-6".to_string()),
        input_tokens: None,
        output_tokens: None,
        cached_tokens: None,
        error: None,
        metadata: json!({}),
        created_at: Utc::now(),
        completed_at: None,
    }
}

#[test]
fn build_messages_accepts_transcript_turn_messages() {
    let history = vec![turn(
        None,
        None,
        json!([
            {
                "role": "user",
                "content": [{ "type": "text", "text": "q1" }]
            },
            {
                "role": "assistant",
                "content": [
                    { "type": "text", "text": "a1" },
                    {
                        "type": "tool_use",
                        "id": "toolu_1",
                        "name": "calculator",
                        "input": { "x": 1, "y": 2 }
                    }
                ],
                "response_id": "msg_1"
            }
        ]),
    )];

    let messages = build_messages(&history, "q2");
    let serialized = serde_json::to_value(messages).unwrap();
    let arr = serialized.as_array().unwrap();

    // transcript user + transcript assistant + new user input
    assert_eq!(arr.len(), 3, "serialized = {serialized}");
    assert_eq!(arr[0]["role"], "user");
    assert_eq!(arr[0]["content"][0]["text"], "q1");
    assert_eq!(arr[1]["role"], "assistant");
    assert_eq!(arr[1]["content"][0]["text"], "a1");
    assert_eq!(arr[1]["content"][1]["type"], "tool_use");
    assert_eq!(arr[1]["content"][1]["name"], "calculator");
    assert_eq!(arr[2]["role"], "user");
    assert_eq!(arr[2]["content"][0]["text"], "q2");
}

#[test]
fn build_messages_supports_legacy_full_response_turn_messages() {
    let history = vec![turn(
        Some("q1"),
        Some("fallback"),
        json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-6",
            "content": [{ "type": "text", "text": "a1" }],
            "usage": { "input_tokens": 1, "output_tokens": 2 }
        }),
    )];

    let messages = build_messages(&history, "q2");
    let serialized = serde_json::to_value(messages).unwrap();
    let arr = serialized.as_array().unwrap();
    assert_eq!(arr[0]["role"], "user");
    assert_eq!(arr[1]["role"], "assistant");
    assert_eq!(arr[1]["content"][0]["text"], "a1");
}

#[test]
fn build_messages_supports_legacy_content_array_turn_messages() {
    let history = vec![turn(
        Some("q1"),
        Some("fallback"),
        json!([{ "type": "text", "text": "a1" }]),
    )];

    let messages = build_messages(&history, "q2");
    let serialized = serde_json::to_value(messages).unwrap();
    let arr = serialized.as_array().unwrap();
    assert_eq!(arr[1]["role"], "assistant");
    assert_eq!(arr[1]["content"][0]["text"], "a1");
}
