mod common;

use common::spawn_app;
use fork_chat_backend::turn_runtime::stream_event;
use fork_chat_backend::turn_stream::TurnStreamEvent;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::sleep;
use uuid::Uuid;

#[tokio::test]
async fn post_turn_success_completes_and_auto_titles_session() {
    let app = spawn_app().await;
    let session_id = app.create_session(None).await;
    app.mock_openai_success("Hello from assistant", "resp_ok")
        .await;

    let resp = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "Hi there",
            "provider": "openai",
            "model": "gpt-5.4-mini",
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
    assert_eq!(body["turn"]["assistant_text"], "Hello from assistant");
    assert_eq!(body["turn"]["response_id"], "resp_ok");
    assert_eq!(body["turn"]["input_tokens"], 10);
    assert_eq!(body["turn"]["output_tokens"], 20);
    assert_eq!(body["turn"]["model"], "gpt-5.4-mini");
    assert_eq!(body["turn"]["provider"], "openai");

    // Session should now be auto-titled from the first 50 chars of user_text.
    let session: Value = app
        .http
        .get(app.url(&format!("/api/sessions/{session_id}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(session["session"]["title"], "Hi there");

    app.cleanup().await;
}

#[tokio::test]
async fn post_turn_rejects_second_root() {
    let app = spawn_app().await;
    let session_id = app.create_session(None).await;
    app.mock_openai_success("root answer", "resp_root").await;

    let first = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "first root",
            "provider": "openai",
            "model": "gpt-5.4-mini",
        }))
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());
    let first_body: Value = first.json().await.unwrap();
    let first_turn_id = Uuid::parse_str(first_body["turn"]["id"].as_str().unwrap()).unwrap();
    let _ = app
        .wait_turn_status(session_id, first_turn_id, &["completed"])
        .await;

    let second = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "second root",
            "provider": "openai",
            "model": "gpt-5.4-mini",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::BAD_REQUEST);
    let err: Value = second.json().await.unwrap();
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .contains("already has a root turn"),
        "got error: {err}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn post_turn_accepts_fork_from_existing_parent() {
    let app = spawn_app().await;
    let session_id = app.create_session(None).await;
    app.mock_openai_success("ok", "resp_fork").await;

    let first = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "root",
            "provider": "openai",
            "model": "gpt-5.4-mini",
        }))
        .send()
        .await
        .unwrap();
    let first_body: Value = first.json().await.unwrap();
    let parent_id = first_body["turn"]["id"].as_str().unwrap().to_string();

    let fork = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "child",
            "parent_turn_id": parent_id,
            "provider": "openai",
            "model": "gpt-5.4-mini",
        }))
        .send()
        .await
        .unwrap();
    assert!(fork.status().is_success());
    let fork_body: Value = fork.json().await.unwrap();
    assert_eq!(fork_body["turn"]["parent_turn_id"], parent_id);
    let first_turn_id = Uuid::parse_str(first_body["turn"]["id"].as_str().unwrap()).unwrap();
    let fork_turn_id = Uuid::parse_str(fork_body["turn"]["id"].as_str().unwrap()).unwrap();
    let _ = app
        .wait_turn_status(session_id, first_turn_id, &["completed"])
        .await;
    let _ = app
        .wait_turn_status(session_id, fork_turn_id, &["completed"])
        .await;

    app.cleanup().await;
}

#[tokio::test]
async fn post_turn_rejects_parent_from_another_session() {
    let app = spawn_app().await;
    let first_session = app.create_session(None).await;
    let second_session = app.create_session(None).await;
    app.mock_openai_success("root answer", "resp_cross_parent")
        .await;

    let root: Value = app
        .http
        .post(app.url(&format!("/api/sessions/{first_session}/turns")))
        .json(&json!({
            "user_text": "first session root",
            "provider": "openai",
            "model": "gpt-5.4-mini",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let foreign_parent_id = root["turn"]["id"].as_str().unwrap();
    let root_turn_id = Uuid::parse_str(foreign_parent_id).unwrap();
    let _ = app
        .wait_turn_status(first_session, root_turn_id, &["completed"])
        .await;

    let resp = app
        .http
        .post(app.url(&format!("/api/sessions/{second_session}/turns")))
        .json(&json!({
            "user_text": "should not cross sessions",
            "parent_turn_id": foreign_parent_id,
            "provider": "openai",
            "model": "gpt-5.4-mini",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    let second_session_turns: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM turns WHERE session_id = $1")
            .bind(second_session)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(second_session_turns.0, 0);

    app.cleanup().await;
}

#[tokio::test]
async fn post_turn_rejects_provider_not_on_session_protocol() {
    let app = spawn_app().await;
    // Session is openai-protocol; 'anthropic' provider only supports anthropic
    // protocol, so dispatch must refuse even though the provider exists.
    let session_id = app.create_session(None).await;

    let resp = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "hi",
            "provider": "anthropic",
            "model": "claude-sonnet-4-6",
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
        "got error: {err}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn post_turn_rejects_unknown_provider() {
    let app = spawn_app().await;
    let session_id = app.create_session(None).await;

    let resp = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "hi",
            "provider": "does-not-exist",
            "model": "gpt-5.4-mini",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let err: Value = resp.json().await.unwrap();
    assert!(
        err["error"].as_str().unwrap().contains("unknown provider"),
        "got error: {err}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn post_turn_rejects_model_not_exposed_by_provider() {
    let app = spawn_app().await;
    let session_id = app.create_session(None).await;

    let resp = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "hi",
            "provider": "openai",
            "model": "not-a-model",
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
            .contains("not exposed by provider"),
        "got error: {err}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn post_turn_rejects_reply_to_failed_parent() {
    let app = spawn_app().await;
    let session_id = app.create_session(None).await;

    // Insert a failed root turn directly via sqlx.
    let failed_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO turns (session_id, parent_turn_id, status, user_text, turn_messages)
        VALUES ($1, NULL, 'failed', 'bad', '[]'::jsonb)
        RETURNING id
        "#,
    )
    .bind(session_id)
    .fetch_one(&app.db)
    .await
    .unwrap();

    let resp = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "reply",
            "parent_turn_id": failed_id,
            "provider": "openai",
            "model": "gpt-5.4-mini",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let err: Value = resp.json().await.unwrap();
    assert!(err["error"].as_str().unwrap().contains("failed turn"));

    app.cleanup().await;
}

#[tokio::test]
async fn post_turn_persists_failed_status_when_openai_errors() {
    let app = spawn_app().await;
    let session_id = app.create_session(None).await;
    app.mock_openai_failure(500).await;

    let resp = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "hello",
            "provider": "openai",
            "model": "gpt-5.4-mini",
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "status={}", resp.status());
    let created: Value = resp.json().await.unwrap();
    let turn_id = Uuid::parse_str(created["turn"]["id"].as_str().unwrap()).unwrap();
    let turn = app.wait_turn_status(session_id, turn_id, &["failed"]).await;
    assert_eq!(turn["turn"]["status"], "failed");

    // The handler should have persisted a failed turn record.
    let row: (String, Option<Value>) =
        sqlx::query_as("SELECT status, error FROM turns WHERE session_id = $1")
            .bind(session_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(row.0, "failed");
    assert!(row.1.is_some(), "error JSON should be populated");

    // Even when the first turn fails, session should still be auto-titled from
    // the first user message.
    let title: Option<String> = sqlx::query_scalar("SELECT title FROM sessions WHERE id = $1")
        .bind(session_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(title.as_deref(), Some("hello"));

    app.cleanup().await;
}

#[tokio::test]
async fn retry_succeeds_and_links_old_turn() {
    let app = spawn_app().await;
    let session_id = app.create_session(None).await;

    // First call fails, retry call succeeds. wiremock matches first matching mock
    // so we register the success mock AFTER the failure is consumed.
    app.mock_openai_failure(500).await;

    let fail_resp = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "retry me",
            "provider": "openai",
            "model": "gpt-5.4-mini",
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

    // Reset and install a success mock for the retry call.
    app.openai.reset().await;
    app.mock_openai_success("retry worked", "resp_retry").await;

    let retry = app
        .http
        .post(app.url(&format!(
            "/api/sessions/{session_id}/turns/{failed_id}/retry"
        )))
        .json(&json!({
            "provider": "openai",
            "model": "gpt-5.4-mini",
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
    assert_eq!(completed["turn"]["assistant_text"], "retry worked");

    // Old failed turn should now have retry_turn_id == new_id.
    let link: Option<Uuid> = sqlx::query_scalar("SELECT retry_turn_id FROM turns WHERE id = $1")
        .bind(failed_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(link, Some(new_id));

    app.cleanup().await;
}

#[tokio::test]
async fn retry_rejects_turn_from_another_session() {
    let app = spawn_app().await;
    let first_session = app.create_session(None).await;
    let second_session = app.create_session(None).await;

    let foreign_failed_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO turns (session_id, parent_turn_id, status, user_text, turn_messages)
        VALUES ($1, NULL, 'failed', 'foreign failure', '[]'::jsonb)
        RETURNING id
        "#,
    )
    .bind(first_session)
    .fetch_one(&app.db)
    .await
    .unwrap();

    let resp = app
        .http
        .post(app.url(&format!(
            "/api/sessions/{second_session}/turns/{foreign_failed_id}/retry"
        )))
        .json(&json!({
            "provider": "openai",
            "model": "gpt-5.4-mini",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    let second_session_turns: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM turns WHERE session_id = $1")
            .bind(second_session)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(second_session_turns.0, 0);

    app.cleanup().await;
}

#[tokio::test]
async fn retry_double_failure_still_links_turns() {
    let app = spawn_app().await;
    let session_id = app.create_session(None).await;
    app.mock_openai_failure(500).await;

    let fail_resp = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "boom",
            "provider": "openai",
            "model": "gpt-5.4-mini",
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
    let old_id = Uuid::parse_str(failed_body["turn"]["id"].as_str().unwrap()).unwrap();
    let _ = app.wait_turn_status(session_id, old_id, &["failed"]).await;

    let retry = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns/{old_id}/retry")))
        .json(&json!({
            "provider": "openai",
            "model": "gpt-5.4-mini",
        }))
        .send()
        .await
        .unwrap();
    assert!(retry.status().is_success(), "status={}", retry.status());
    let retry_body: Value = retry.json().await.unwrap();
    let retry_id = Uuid::parse_str(retry_body["turn"]["id"].as_str().unwrap()).unwrap();
    let _ = app
        .wait_turn_status(session_id, retry_id, &["failed"])
        .await;

    // Two turns exist, old one linked to the new (also failed) one.
    let turns: Vec<(Uuid, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT id, status, retry_turn_id FROM turns WHERE session_id = $1 ORDER BY created_at",
    )
    .bind(session_id)
    .fetch_all(&app.db)
    .await
    .unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].1, "failed");
    assert_eq!(turns[1].1, "failed");
    assert_eq!(turns[0].2, Some(turns[1].0));

    app.cleanup().await;
}

#[tokio::test]
async fn retry_rejects_provider_not_on_session_protocol() {
    let app = spawn_app().await;
    let session_id = app.create_session(None).await;

    // Insert a failed turn directly so we have something to retry.
    let failed_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO turns (session_id, parent_turn_id, status, user_text, turn_messages)
        VALUES ($1, NULL, 'failed', 'x', '[]'::jsonb)
        RETURNING id
        "#,
    )
    .bind(session_id)
    .fetch_one(&app.db)
    .await
    .unwrap();

    // Session is openai-protocol; trying to retry with the anthropic provider
    // must fail at the dispatch layer.
    let resp = app
        .http
        .post(app.url(&format!(
            "/api/sessions/{session_id}/turns/{failed_id}/retry"
        )))
        .json(&json!({ "provider": "anthropic", "model": "claude-sonnet-4-6" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

#[tokio::test]
async fn openai_tool_result_is_sent_back_and_turn_reaches_final_answer() {
    let app = spawn_app().await;
    let session_id = app.create_session(None).await;

    app.mock_openai_tool_call(
        "resp_tool",
        "fc_read_1",
        "read",
        "{\"path\":\"./Cargo.toml\"}",
    )
    .await;
    app.mock_openai_success_after_function_output("Done after tool call.", "resp_done")
        .await;

    let create = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "看看当前目录的项目配置",
            "provider": "openai",
            "model": "gpt-5.4-mini",
        }))
        .send()
        .await
        .unwrap();
    assert!(create.status().is_success(), "status={}", create.status());
    let created: Value = create.json().await.unwrap();
    let turn_id = Uuid::parse_str(created["turn"]["id"].as_str().unwrap()).unwrap();

    let completed = app
        .wait_turn_status(session_id, turn_id, &["completed"])
        .await;
    assert_eq!(completed["turn"]["assistant_text"], "Done after tool call.");
    let msgs = completed["turn"]["turn_messages"].as_array().unwrap();
    assert!(msgs.iter().any(|m| {
        m["role"] == "user"
            && m["content"]
                .as_array()
                .is_some_and(|arr| arr.iter().any(|b| b["type"] == "function_call_output"))
    }));
    assert!(msgs.iter().any(|m| {
        m["role"] == "assistant"
            && m["content"].as_array().is_some_and(|arr| {
                arr.iter()
                    .any(|b| b["type"] == "message" || b["type"] == "function_call")
            })
    }));

    app.cleanup().await;
}

#[tokio::test]
async fn cancel_during_inflight_openai_call_stays_failed() {
    let app = spawn_app().await;
    let session_id = app.create_session(None).await;

    app.mock_openai_delayed_success(
        "this answer should be dropped by cancellation",
        "resp_delayed",
        Duration::from_millis(800),
    )
    .await;

    let create = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "please run slowly",
            "provider": "openai",
            "model": "gpt-5.4-mini",
        }))
        .send()
        .await
        .unwrap();
    assert!(create.status().is_success(), "status={}", create.status());
    let created: Value = create.json().await.unwrap();
    let turn_id = Uuid::parse_str(created["turn"]["id"].as_str().unwrap()).unwrap();

    // Give the loop a short head start so the delayed upstream request is
    // definitely in-flight when cancel arrives.
    sleep(Duration::from_millis(120)).await;

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

    // Wait long enough for the delayed upstream response to arrive. If
    // cancellation is not sticky, stale loop writes may incorrectly overwrite
    // this row back to running/completed.
    sleep(Duration::from_millis(1200)).await;
    let latest = app.get_turn(session_id, turn_id).await;
    assert_eq!(latest["turn"]["status"], "failed");
    assert_eq!(latest["turn"]["error"]["kind"], "cancelled");

    app.cleanup().await;
}

#[tokio::test]
async fn terminal_turn_stream_returns_snapshot_only() {
    let app = spawn_app().await;
    let session_id = app.create_session(None).await;

    let turn_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO turns (session_id, status, user_text, turn_messages, runtime_state, completed_at)
        VALUES ($1, 'completed', 'done', '[]'::jsonb, $2, now())
        RETURNING id
        "#,
    )
    .bind(session_id)
    .bind(json!({ "stream_seq": 7 }))
    .fetch_one(&app.db)
    .await
    .unwrap();

    let response = app
        .http
        .get(app.url(&format!(
            "/api/sessions/{session_id}/turns/{turn_id}/stream"
        )))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "status={}",
        response.status()
    );
    let body = response.text().await.unwrap();

    assert_eq!(body.matches("event: ").count(), 1, "body={body}");
    assert!(body.contains("event: turn_snapshot"), "body={body}");
    assert!(body.contains(r#""seq":7"#), "body={body}");
    assert!(!body.contains("event: turn_completed"), "body={body}");

    app.cleanup().await;
}

#[tokio::test]
async fn stream_only_forwards_live_events_newer_than_snapshot_seq() {
    let app = spawn_app().await;
    let session_id = app.create_session(None).await;

    let turn_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO turns (session_id, status, user_text, turn_messages, runtime_state)
        VALUES ($1, 'running', 'stream me', '[]'::jsonb, $2)
        RETURNING id
        "#,
    )
    .bind(session_id)
    .bind(json!({ "stream_seq": 3 }))
    .fetch_one(&app.db)
    .await
    .unwrap();

    let response = app
        .http
        .get(app.url(&format!(
            "/api/sessions/{session_id}/turns/{turn_id}/stream"
        )))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "status={}",
        response.status()
    );

    let hub = app.turn_stream_hub.clone();
    tokio::spawn(async move {
        sleep(Duration::from_millis(50)).await;
        hub.publish(
            turn_id,
            TurnStreamEvent {
                seq: 3,
                event: stream_event::ROUND_STARTED.to_string(),
                payload: json!({ "round": 0 }),
            },
        )
        .await;
        hub.publish(
            turn_id,
            TurnStreamEvent {
                seq: 4,
                event: stream_event::ROUND_STARTED.to_string(),
                payload: json!({ "round": 1 }),
            },
        )
        .await;
        hub.publish(
            turn_id,
            TurnStreamEvent {
                seq: 5,
                event: stream_event::TURN_FAILED.to_string(),
                payload: json!({ "error": { "kind": "test_terminal" } }),
            },
        )
        .await;
    });

    let body = response.text().await.unwrap();
    assert!(body.contains("event: turn_snapshot"), "body={body}");
    assert_eq!(
        body.matches("event: round_started").count(),
        1,
        "body={body}"
    );
    assert!(body.contains(r#""seq":4"#), "body={body}");
    assert!(body.contains(r#""seq":5"#), "body={body}");
    assert!(
        !body.contains(r#""seq":3,"payload":{"round":0}"#),
        "body={body}"
    );
    assert!(body.contains("event: turn_failed"), "body={body}");

    app.cleanup().await;
}
