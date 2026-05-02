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

    let created: Value = resp.json().await.unwrap();
    let turn_id = Uuid::parse_str(created["turn"]["id"].as_str().unwrap()).unwrap();
    let body = app
        .wait_turn_status(session_id, turn_id, &["completed"])
        .await;
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
    assert!(resp.status().is_success(), "status={}", resp.status());
    let created: Value = resp.json().await.unwrap();
    let turn_id = Uuid::parse_str(created["turn"]["id"].as_str().unwrap()).unwrap();
    let _ = app.wait_turn_status(session_id, turn_id, &["failed"]).await;

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
    assert!(
        fail_resp.status().is_success(),
        "status={}",
        fail_resp.status()
    );
    let failed_body: Value = fail_resp.json().await.unwrap();
    let failed_id = Uuid::parse_str(failed_body["turn"]["id"].as_str().unwrap()).unwrap();
    let _ = app
        .wait_turn_status(session_id, failed_id, &["failed"])
        .await;

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
    let new_id = Uuid::parse_str(body["turn"]["id"].as_str().unwrap()).unwrap();
    let completed = app
        .wait_turn_status(session_id, new_id, &["completed"])
        .await;
    assert_eq!(completed["turn"]["assistant_text"], "recovered");

    // Old failed turn is linked to the new one.
    let link: Option<Uuid> = sqlx::query_scalar("SELECT retry_turn_id FROM turns WHERE id = $1")
        .bind(failed_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(link, Some(new_id));

    app.cleanup().await;
}

#[tokio::test]
async fn anthropic_tool_use_requires_approval_then_completes_after_allow() {
    let app = spawn_app().await;
    let session_id = app.create_session_with(Protocol::Anthropic, None).await;
    app.mock_anthropic_tool_use(
        "msg_tool",
        "toolu_1",
        "bash",
        json!({"command":"echo hello"}),
    )
    .await;

    let create = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "write tmp.txt",
            "provider": "anthropic",
            "model": "claude-sonnet-4-6",
        }))
        .send()
        .await
        .unwrap();
    assert!(create.status().is_success(), "status={}", create.status());
    let created: Value = create.json().await.unwrap();
    let turn_id = Uuid::parse_str(created["turn"]["id"].as_str().unwrap()).unwrap();
    let pending_turn = app
        .wait_turn_status(session_id, turn_id, &["awaiting_approval"])
        .await;
    let pending_call_id =
        pending_turn["turn"]["runtime_state"]["pending_tool_calls"][0]["pending_call_id"]
            .as_str()
            .unwrap()
            .to_string();

    app.anthropic.reset().await;
    app.mock_anthropic_success("done", "msg_done").await;

    let approve = app
        .http
        .post(app.url(&format!(
            "/api/sessions/{session_id}/turns/{turn_id}/approve"
        )))
        .json(&json!({
            "decisions": [{ "pending_call_id": pending_call_id, "decision": "allow" }]
        }))
        .send()
        .await
        .unwrap();
    assert!(approve.status().is_success(), "status={}", approve.status());
    let approved: Value = approve.json().await.unwrap();
    assert_eq!(approved["turn"]["status"], "running");
    let approved_id = Uuid::parse_str(approved["turn"]["id"].as_str().unwrap()).unwrap();
    let completed = app
        .wait_turn_status(session_id, approved_id, &["completed"])
        .await;
    let msgs = completed["turn"]["turn_messages"].as_array().unwrap();
    assert!(msgs.iter().any(|m| {
        m["role"] == "user"
            && m["content"]
                .as_array()
                .is_some_and(|arr| arr.iter().any(|b| b["type"] == "tool_result"))
    }));

    app.cleanup().await;
}

#[tokio::test]
async fn anthropic_awaiting_approval_can_be_cancelled() {
    let app = spawn_app().await;
    let session_id = app.create_session_with(Protocol::Anthropic, None).await;
    app.mock_anthropic_tool_use(
        "msg_tool_cancel",
        "toolu_cancel",
        "bash",
        json!({"command":"echo hello"}),
    )
    .await;

    let create = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "write tmp.txt",
            "provider": "anthropic",
            "model": "claude-sonnet-4-6",
        }))
        .send()
        .await
        .unwrap();
    let created: Value = create.json().await.unwrap();
    let turn_id = Uuid::parse_str(created["turn"]["id"].as_str().unwrap()).unwrap();
    let _ = app
        .wait_turn_status(session_id, turn_id, &["awaiting_approval"])
        .await;

    let cancel = app
        .http
        .post(app.url(&format!(
            "/api/sessions/{session_id}/turns/{turn_id}/cancel"
        )))
        .send()
        .await
        .unwrap();
    assert!(cancel.status().is_success(), "status={}", cancel.status());
    let cancelled: Value = cancel.json().await.unwrap();
    assert_eq!(cancelled["turn"]["status"], "failed");
    assert_eq!(cancelled["turn"]["error"]["kind"], "cancelled");

    app.cleanup().await;
}

#[tokio::test]
async fn approve_is_idempotent_for_same_pending_call_and_decision() {
    let app = spawn_app().await;
    let session_id = app.create_session_with(Protocol::Anthropic, None).await;
    app.mock_anthropic_tool_use(
        "msg_tool_idempotent",
        "toolu_idempotent",
        "bash",
        json!({"command":"echo hello"}),
    )
    .await;

    let create = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "run bash",
            "provider": "anthropic",
            "model": "claude-sonnet-4-6",
        }))
        .send()
        .await
        .unwrap();
    let created: Value = create.json().await.unwrap();
    let turn_id = Uuid::parse_str(created["turn"]["id"].as_str().unwrap()).unwrap();
    let pending_turn = app
        .wait_turn_status(session_id, turn_id, &["awaiting_approval"])
        .await;
    let pending_call_id =
        pending_turn["turn"]["runtime_state"]["pending_tool_calls"][0]["pending_call_id"]
            .as_str()
            .unwrap()
            .to_string();

    app.anthropic.reset().await;
    app.mock_anthropic_success("done", "msg_done_idempotent")
        .await;

    let approve_body = json!({
        "decisions": [{ "pending_call_id": pending_call_id, "decision": "allow" }]
    });

    let first = app
        .http
        .post(app.url(&format!(
            "/api/sessions/{session_id}/turns/{turn_id}/approve"
        )))
        .json(&approve_body)
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success(), "status={}", first.status());

    let second = app
        .http
        .post(app.url(&format!(
            "/api/sessions/{session_id}/turns/{turn_id}/approve"
        )))
        .json(&approve_body)
        .send()
        .await
        .unwrap();
    assert!(second.status().is_success(), "status={}", second.status());

    let completed = app
        .wait_turn_status(session_id, turn_id, &["completed"])
        .await;
    let tool_result_blocks = completed["turn"]["turn_messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["role"] == "user")
        .flat_map(|entry| entry["content"].as_array().into_iter().flatten())
        .filter(|block| block["type"] == "tool_result")
        .count();
    assert_eq!(tool_result_blocks, 1);

    app.cleanup().await;
}
