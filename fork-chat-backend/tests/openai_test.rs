use async_openai::types::responses::Response;
use fork_chat_backend::error::AppError;
use fork_chat_backend::llm::openai::OpenaiAdapter;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build a minimal `Response` fixture from a JSON value.
fn make_response(output: serde_json::Value, usage: Option<serde_json::Value>) -> Response {
    let mut value = json!({
        "id": "resp_test",
        "object": "response",
        "created_at": 0,
        "model": "test-model",
        "status": "completed",
        "output": output,
    });
    if let Some(u) = usage {
        value
            .as_object_mut()
            .unwrap()
            .insert("usage".to_string(), u);
    }
    serde_json::from_value(value).expect("failed to build Response fixture")
}

#[test]
fn extract_assistant_text_returns_none_for_empty_output() {
    let response = make_response(json!([]), None);
    assert!(OpenaiAdapter::extract_assistant_text(&response).is_none());
}

#[test]
fn extract_assistant_text_skips_non_message_items() {
    // A reasoning-only output carries no assistant message text.
    let response = make_response(
        json!([
            {
                "type": "reasoning",
                "id": "rs_1",
                "summary": []
            }
        ]),
        None,
    );
    assert!(OpenaiAdapter::extract_assistant_text(&response).is_none());
}

#[test]
fn extract_assistant_text_returns_text_from_message() {
    let response = make_response(
        json!([
            {
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "status": "completed",
                "content": [
                    {
                        "type": "output_text",
                        "text": "Hello, how can I help?",
                        "annotations": []
                    }
                ]
            }
        ]),
        None,
    );

    let text = OpenaiAdapter::extract_assistant_text(&response);
    assert_eq!(text.as_deref(), Some("Hello, how can I help?"));
}

#[test]
fn extract_assistant_text_picks_first_text_content() {
    let response = make_response(
        json!([
            {
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "status": "completed",
                "content": [
                    {
                        "type": "output_text",
                        "text": "first",
                        "annotations": []
                    },
                    {
                        "type": "output_text",
                        "text": "second",
                        "annotations": []
                    }
                ]
            }
        ]),
        None,
    );

    assert_eq!(
        OpenaiAdapter::extract_assistant_text(&response).as_deref(),
        Some("first")
    );
}

#[test]
fn extract_usage_returns_none_when_missing() {
    let response = make_response(json!([]), None);
    let (input, output) = OpenaiAdapter::extract_usage(&response);
    assert!(input.is_none());
    assert!(output.is_none());
}

#[test]
fn extract_usage_returns_token_counts() {
    let response = make_response(
        json!([]),
        Some(json!({
            "input_tokens": 12,
            "output_tokens": 34,
            "total_tokens": 46,
            "input_tokens_details": { "cached_tokens": 0 },
            "output_tokens_details": { "reasoning_tokens": 0 }
        })),
    );

    let (input, output) = OpenaiAdapter::extract_usage(&response);
    assert_eq!(input, Some(12));
    assert_eq!(output, Some(34));
}

#[test]
fn serialize_output_round_trips_via_json() {
    let response = make_response(
        json!([
            {
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "status": "completed",
                "content": [
                    {
                        "type": "output_text",
                        "text": "hi",
                        "annotations": []
                    }
                ]
            }
        ]),
        None,
    );

    let json_value = OpenaiAdapter::serialize_output(&response.output).expect("serialize");
    let arr = json_value.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["type"], "message");
    assert_eq!(arr[0]["content"][0]["text"], "hi");
}

#[tokio::test]
async fn send_raw_reports_status_request_id_and_empty_body_on_decode_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-request-id", "req_decode")
                .set_body_string(""),
        )
        .mount(&server)
        .await;

    let adapter = OpenaiAdapter::new(&server.uri(), "test-key", true);
    let err = adapter
        .send_raw(Vec::new(), "test-model", None)
        .await
        .expect_err("empty success body should fail to decode");

    let AppError::LlmApiError(_) = &err else {
        panic!("expected LLM API error, got {err:?}");
    };

    let message = err.to_string();
    assert!(message.contains("openai-compatible /responses decode failed"));
    assert!(message.contains("status 200"));
    assert!(message.contains("request_id=req_decode"));
    assert!(message.contains("body_len=0"));
    assert!(message.contains("body_preview=<empty>"));
    assert!(message.contains("origin:"));

    // The structured chain keeps the serde failure visible for logs and the
    // failed-turn JSON, instead of flattening everything into the top message.
    let chain = err.diagnostic_chain();
    assert!(
        chain
            .iter()
            .any(|entry| entry.contains("EOF while parsing a value")),
        "chain={chain:?}"
    );
}
