//! Integration tests for the Anthropic adapter path (session.protocol ==
//! "anthropic"). Exercises both successful dispatch and failure handling via
//! the shared wiremock instance provisioned by `spawn_app`.

mod common;

use common::spawn_app;
use fork_chat_backend::config::Protocol;
use serde_json::{Value, json};
use uuid::Uuid;

#[tokio::test]
async fn post_turn_with_anthropic_protocol_succeeds() {
    let app = spawn_app().await;
    let session_id = app.create_session_with(Protocol::Anthropic, None).await;
    app.mock_anthropic_success("Hello from Claude", "msg_ok")
        .await;

    let resp = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "Hi Claude",
            "provider": "anthropic",
            "model": "claude-sonnet-4-6",
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "status={}", resp.status());

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["turn"]["status"], "completed");
    assert_eq!(body["turn"]["assistant_text"], "Hello from Claude");
    assert_eq!(body["turn"]["response_id"], "msg_ok");
    assert_eq!(body["turn"]["input_tokens"], 11);
    assert_eq!(body["turn"]["output_tokens"], 22);
    assert_eq!(body["turn"]["model"], "claude-sonnet-4-6");
    assert_eq!(body["turn"]["provider"], "anthropic");

    // turn_messages stores the per-turn transcript (user + assistant).
    let msgs = body["turn"]["turn_messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[0]["content"][0]["type"], "text");
    assert_eq!(msgs[0]["content"][0]["text"], "Hi Claude");
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["content"][0]["type"], "text");
    assert_eq!(msgs[1]["content"][0]["text"], "Hello from Claude");
    assert_eq!(msgs[1]["response_id"], "msg_ok");
    assert_eq!(msgs[1]["stop_reason"], "end_turn");
    assert_eq!(msgs[1]["usage"]["input_tokens"], 11);
    assert_eq!(msgs[1]["usage"]["output_tokens"], 22);
    assert_eq!(msgs[1]["raw_response"]["id"], "msg_ok");

    app.cleanup().await;
}

#[tokio::test]
async fn post_turn_persists_failed_status_when_anthropic_errors() {
    let app = spawn_app().await;
    let session_id = app.create_session_with(Protocol::Anthropic, None).await;
    app.mock_anthropic_failure(500).await;

    let resp = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "boom",
            "provider": "anthropic",
            "model": "claude-sonnet-4-6",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_GATEWAY);

    let row: (String, Option<Value>) =
        sqlx::query_as("SELECT status, error FROM turns WHERE session_id = $1")
            .bind(session_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(row.0, "failed");
    assert!(row.1.is_some(), "error JSON should be populated");

    app.cleanup().await;
}

#[tokio::test]
async fn post_turn_rejects_openai_provider_on_anthropic_session() {
    let app = spawn_app().await;
    let session_id = app.create_session_with(Protocol::Anthropic, None).await;

    let resp = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "hi",
            "provider": "openai",
            "model": "gpt-5.4-mini",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let err: Value = resp.json().await.unwrap();
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .contains("not configured for protocol"),
        "got: {err}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn retry_on_anthropic_session_links_turns() {
    let app = spawn_app().await;
    let session_id = app.create_session_with(Protocol::Anthropic, None).await;

    // First request fails.
    app.mock_anthropic_failure(500).await;
    let fail_resp = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "retry me",
            "provider": "anthropic",
            "model": "claude-sonnet-4-6",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(fail_resp.status(), reqwest::StatusCode::BAD_GATEWAY);

    let failed_id: Uuid = sqlx::query_scalar("SELECT id FROM turns WHERE session_id = $1")
        .bind(session_id)
        .fetch_one(&app.db)
        .await
        .unwrap();

    // Swap the mock for a successful retry.
    app.anthropic.reset().await;
    app.mock_anthropic_success("recovered", "msg_retry").await;

    let retry = app
        .http
        .post(app.url(&format!(
            "/api/sessions/{session_id}/turns/{failed_id}/retry"
        )))
        .json(&json!({
            "provider": "anthropic",
            "model": "claude-sonnet-4-6",
        }))
        .send()
        .await
        .unwrap();
    assert!(retry.status().is_success(), "status={}", retry.status());

    let body: Value = retry.json().await.unwrap();
    let new_id = body["turn"]["id"].as_str().unwrap().to_string();
    assert_eq!(body["turn"]["assistant_text"], "recovered");

    // Old failed turn is linked to the new one.
    let link: Option<Uuid> = sqlx::query_scalar("SELECT retry_turn_id FROM turns WHERE id = $1")
        .bind(failed_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(link.map(|u| u.to_string()), Some(new_id));

    app.cleanup().await;
}
