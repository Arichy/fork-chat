//! Unit tests for Anthropic message replay. Ensures we rebuild request history
//! from the canonical transcript `turn_messages` shape only.

use chrono::Utc;
use fork_chat_backend::llm::anthropic::message_builder::build_messages;
use fork_chat_backend::models::Turn;
use fork_chat_backend::turn_runtime::TurnRuntimeState;
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
        runtime_state: TurnRuntimeState::default(),
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

    let messages = build_messages(&history, Some("q2"));
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
fn build_messages_ignores_non_transcript_turn_messages() {
    let history = vec![turn(
        Some("q1"),
        Some("fallback"),
        json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "text", "text": "a1" }]
        }),
    )];

    let messages = build_messages(&history, Some("q2"));
    let serialized = serde_json::to_value(messages).unwrap();
    let arr = serialized.as_array().unwrap();

    // Non-transcript rows are intentionally ignored in early-stage strict mode.
    assert_eq!(arr.len(), 1, "serialized = {serialized}");
    assert_eq!(arr[0]["role"], "user");
    assert_eq!(arr[0]["content"][0]["text"], "q2");
}
