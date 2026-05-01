mod common;

use common::spawn_app;
use serde_json::{Value, json};
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

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["turn"]["status"], "completed");
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
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_GATEWAY);

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
    assert_eq!(fail_resp.status(), reqwest::StatusCode::BAD_GATEWAY);

    // Fetch the failed turn id from the DB.
    let failed_id: Uuid = sqlx::query_scalar("SELECT id FROM turns WHERE session_id = $1")
        .bind(session_id)
        .fetch_one(&app.db)
        .await
        .unwrap();

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
    let new_id = body["turn"]["id"].as_str().unwrap().to_string();
    assert_eq!(body["turn"]["status"], "completed");
    assert_eq!(body["turn"]["assistant_text"], "retry worked");

    // Old failed turn should now have retry_turn_id == new_id.
    let link: Option<Uuid> = sqlx::query_scalar("SELECT retry_turn_id FROM turns WHERE id = $1")
        .bind(failed_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(link.map(|u| u.to_string()), Some(new_id));

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
    assert_eq!(fail_resp.status(), reqwest::StatusCode::BAD_GATEWAY);

    let old_id: Uuid = sqlx::query_scalar("SELECT id FROM turns WHERE session_id = $1")
        .bind(session_id)
        .fetch_one(&app.db)
        .await
        .unwrap();

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
    assert_eq!(retry.status(), reqwest::StatusCode::BAD_GATEWAY);

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
