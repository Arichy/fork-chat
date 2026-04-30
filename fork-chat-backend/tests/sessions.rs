mod common;

use common::spawn_app;
use serde_json::{Value, json};
use uuid::Uuid;

#[tokio::test]
async fn post_sessions_creates_a_session() {
    let app = spawn_app().await;

    let resp = app
        .http
        .post(app.url("/api/sessions"))
        .json(&json!({ "protocol": "openai" }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "status={}", resp.status());

    let body: Value = resp.json().await.unwrap();
    assert!(Uuid::parse_str(body["session"]["id"].as_str().unwrap()).is_ok());
    assert!(body["session"]["title"].is_null());
    assert!(body["session"]["system_prompt"].is_null());
    assert_eq!(body["session"]["protocol"], "openai");

    app.cleanup().await;
}

#[tokio::test]
async fn post_sessions_rejects_missing_protocol() {
    let app = spawn_app().await;

    let resp = app
        .http
        .post(app.url("/api/sessions"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    // Axum returns 422 for JSON deserialization failures by default.
    assert!(
        !resp.status().is_success(),
        "expected failure, got {}",
        resp.status()
    );

    app.cleanup().await;
}

#[tokio::test]
async fn post_sessions_accepts_anthropic_protocol() {
    let app = spawn_app().await;

    let resp = app
        .http
        .post(app.url("/api/sessions"))
        .json(&json!({ "protocol": "anthropic" }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "status={}", resp.status());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["session"]["protocol"], "anthropic");

    app.cleanup().await;
}

#[tokio::test]
async fn post_sessions_persists_system_prompt() {
    let app = spawn_app().await;

    let resp = app
        .http
        .post(app.url("/api/sessions"))
        .json(&json!({ "protocol": "openai", "system_prompt": "be concise" }))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["session"]["system_prompt"], "be concise");

    app.cleanup().await;
}

#[tokio::test]
async fn get_session_returns_404_for_missing_id() {
    let app = spawn_app().await;
    let missing = Uuid::new_v4();
    let resp = app
        .http
        .get(app.url(&format!("/api/sessions/{missing}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("not found"));

    app.cleanup().await;
}

#[tokio::test]
async fn get_session_returns_200_for_existing_id() {
    let app = spawn_app().await;
    let id = app.create_session(Some("sp")).await;

    let resp = app
        .http
        .get(app.url(&format!("/api/sessions/{id}")))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["session"]["id"], id.to_string());
    assert_eq!(body["session"]["system_prompt"], "sp");

    app.cleanup().await;
}

#[tokio::test]
async fn list_sessions_orders_by_created_at_desc() {
    let app = spawn_app().await;
    let first = app.create_session(None).await;
    // Ensure distinguishable created_at ordering (Postgres now() is microsec).
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let second = app.create_session(None).await;

    let resp = app.http.get(app.url("/api/sessions")).send().await.unwrap();
    let arr: Value = resp.json().await.unwrap();
    let ids: Vec<String> = arr
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect();
    // Newest first.
    assert_eq!(ids[0], second.to_string());
    assert_eq!(ids[1], first.to_string());

    app.cleanup().await;
}

#[tokio::test]
async fn patch_session_updates_title_and_bumps_updated_at() {
    let app = spawn_app().await;
    let id = app.create_session(None).await;

    // Grab current updated_at so we can compare.
    let before: Value = app
        .http
        .get(app.url(&format!("/api/sessions/{id}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let before_updated = before["session"]["updated_at"]
        .as_str()
        .unwrap()
        .to_string();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let resp = app
        .http
        .patch(app.url(&format!("/api/sessions/{id}")))
        .json(&json!({ "title": "renamed" }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["session"]["title"], "renamed");
    let after_updated = body["session"]["updated_at"].as_str().unwrap().to_string();
    assert_ne!(before_updated, after_updated);

    app.cleanup().await;
}

#[tokio::test]
async fn delete_session_returns_deleted_true_and_then_404() {
    let app = spawn_app().await;
    let id = app.create_session(None).await;

    let del = app
        .http
        .delete(app.url(&format!("/api/sessions/{id}")))
        .send()
        .await
        .unwrap();
    assert!(del.status().is_success());
    let body: Value = del.json().await.unwrap();
    assert_eq!(body["deleted"], true);

    let missing = app
        .http
        .get(app.url(&format!("/api/sessions/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    let double_delete = app
        .http
        .delete(app.url(&format!("/api/sessions/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(double_delete.status(), reqwest::StatusCode::NOT_FOUND);

    app.cleanup().await;
}

#[tokio::test]
async fn delete_session_cascades_to_turns() {
    let app = spawn_app().await;
    let id = app.create_session(None).await;
    app.mock_openai_success("hi", "resp_cascade").await;

    // Create a root turn so the session has a child row.
    let turn_resp = app
        .http
        .post(app.url(&format!("/api/sessions/{id}/turns")))
        .json(&json!({
            "user_text": "hello",
            "provider": "openai",
            "model": "gpt-5.4-mini",
        }))
        .send()
        .await
        .unwrap();
    assert!(turn_resp.status().is_success());

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM turns WHERE session_id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(count.0, 1);

    let del = app
        .http
        .delete(app.url(&format!("/api/sessions/{id}")))
        .send()
        .await
        .unwrap();
    assert!(del.status().is_success());

    let count_after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM turns WHERE session_id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(count_after.0, 0, "ON DELETE CASCADE should wipe turns");

    app.cleanup().await;
}
