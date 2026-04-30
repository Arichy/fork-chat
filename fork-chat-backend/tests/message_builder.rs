//! Unit tests for the OpenAI message builder. The builder is a pure function
//! over `&[Turn]` + the new user message, so we construct `Turn` structs
//! directly (no DB needed).

use chrono::Utc;
use fork_chat_backend::llm::openai::message_builder::build_input_items;
use fork_chat_backend::models::Turn;
use serde_json::{Value, json};
use uuid::Uuid;

/// Build a minimal `Turn` with the fields the builder actually inspects.
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
        provider: Some("openai".to_string()),
        model: Some("gpt-5.4-mini".to_string()),
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
fn empty_history_yields_single_user_message() {
    let items = build_input_items(&[], "hi");
    let serialized = serde_json::to_value(&items).unwrap();
    let arr = serialized.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["role"], "user");
    assert_eq!(arr[0]["content"], "hi");
}

#[test]
fn fallback_used_when_turn_messages_empty() {
    let history = vec![turn(Some("hello"), Some("world"), json!([]))];
    let items = build_input_items(&history, "next");
    let serialized = serde_json::to_value(&items).unwrap();
    let arr = serialized.as_array().unwrap();

    // Fallback inserts user+assistant as EasyInputMessages, then appends the new user message.
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0]["role"], "user");
    assert_eq!(arr[0]["content"], "hello");
    assert_eq!(arr[1]["role"], "assistant");
    assert_eq!(arr[1]["content"], "world");
    assert_eq!(arr[2]["role"], "user");
    assert_eq!(arr[2]["content"], "next");
}

#[test]
fn transcript_turn_messages_are_replayed() {
    let transcript = json!([
        {
            "role": "user",
            "content": [{ "role": "user", "content": "prev question" }]
        },
        {
            "role": "assistant",
            "content": [{
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "status": "completed",
                "content": [{
                    "type": "output_text",
                    "text": "prev answer",
                    "annotations": []
                }]
            }]
        }
    ]);

    let history = vec![turn(None, None, transcript)];
    let items = build_input_items(&history, "follow-up");
    let serialized = serde_json::to_value(&items).unwrap();
    let arr = serialized.as_array().unwrap();

    assert_eq!(arr.len(), 3, "serialized = {serialized}");
    assert_eq!(arr[0]["role"], "user");
    assert_eq!(arr[0]["content"], "prev question");
    assert_eq!(arr[1]["type"], "message");
    assert_eq!(arr[2]["role"], "user");
    assert_eq!(arr[2]["content"], "follow-up");
}

#[test]
fn legacy_turn_messages_are_passed_through() {
    let raw = json!([{
        "type": "message",
        "id": "msg_1",
        "role": "assistant",
        "status": "completed",
        "content": [{
            "type": "output_text",
            "text": "prev answer",
            "annotations": []
        }]
    }]);

    let history = vec![turn(Some("prev question"), Some("prev answer"), raw)];
    let items = build_input_items(&history, "follow-up");
    let serialized = serde_json::to_value(&items).unwrap();
    let arr = serialized.as_array().unwrap();

    // Expect: the raw message item (not the fallback), then the new user message.
    assert_eq!(arr.len(), 2, "serialized = {serialized}");
    assert_eq!(arr[0]["type"], "message");
    assert_eq!(arr[1]["role"], "user");
    assert_eq!(arr[1]["content"], "follow-up");
}

#[test]
fn walks_ancestor_chain_in_order() {
    let history = vec![
        turn(Some("q1"), Some("a1"), json!([])),
        turn(Some("q2"), Some("a2"), json!([])),
        turn(Some("q3"), Some("a3"), json!([])),
    ];
    let items = build_input_items(&history, "q4");
    let serialized = serde_json::to_value(&items).unwrap();
    let arr = serialized.as_array().unwrap();

    // Expected: (q1, a1, q2, a2, q3, a3, q4).
    let contents: Vec<&str> = arr
        .iter()
        .map(|v| v["content"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(contents, vec!["q1", "a1", "q2", "a2", "q3", "a3", "q4"]);
}
