mod common;

use common::spawn_app;
use serde_json::{Value, json};
use uuid::Uuid;

#[tokio::test]
async fn batch_delete_removes_multiple_sessions() {
    let app = spawn_app().await;
    let s1 = app.create_session(None).await;
    let s2 = app.create_session(None).await;
    let s3 = app.create_session(None).await;

    let resp = app
        .http
        .post(app.url("/api/sessions/batch-delete"))
        .json(&json!({ "ids": [s1, s2] }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "status={}", resp.status());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["deleted"], 2);

    // Verify the two deleted sessions are gone.
    for id in [s1, s2] {
        let get = app
            .http
            .get(app.url(&format!("/api/sessions/{id}")))
            .send()
            .await
            .unwrap();
        assert_eq!(get.status(), reqwest::StatusCode::NOT_FOUND);
    }
    // The third session should still exist.
    let get3 = app
        .http
        .get(app.url(&format!("/api/sessions/{s3}")))
        .send()
        .await
        .unwrap();
    assert!(get3.status().is_success());

    app.cleanup().await;
}

#[tokio::test]
async fn batch_delete_cascades_to_turns() {
    let app = spawn_app().await;
    let id = app.create_session(None).await;
    app.mock_openai_success("hi", "resp_batch_cascade").await;

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
    let turn_body: Value = turn_resp.json().await.unwrap();
    let turn_id = Uuid::parse_str(turn_body["turn"]["id"].as_str().unwrap()).unwrap();
    let _ = app.wait_turn_status(id, turn_id, &["completed"]).await;

    // Verify the turn exists.
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM turns WHERE session_id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(count.0, 1);

    // Batch delete the session.
    let resp = app
        .http
        .post(app.url("/api/sessions/batch-delete"))
        .json(&json!({ "ids": [id] }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // Turns should be cascade-deleted.
    let count_after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM turns WHERE session_id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(count_after.0, 0, "ON DELETE CASCADE should wipe turns");

    app.cleanup().await;
}

#[tokio::test]
async fn batch_delete_with_nonexistent_ids_returns_deleted_count() {
    let app = spawn_app().await;
    let fake1 = Uuid::new_v4();
    let fake2 = Uuid::new_v4();

    let resp = app
        .http
        .post(app.url("/api/sessions/batch-delete"))
        .json(&json!({ "ids": [fake1, fake2] }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["deleted"], 0);

    app.cleanup().await;
}

#[tokio::test]
async fn batch_delete_rejects_empty_array() {
    let app = spawn_app().await;

    let resp = app
        .http
        .post(app.url("/api/sessions/batch-delete"))
        .json(&json!({ "ids": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

#[tokio::test]
async fn batch_delete_deduplicates_ids() {
    let app = spawn_app().await;
    let id = app.create_session(None).await;

    let resp = app
        .http
        .post(app.url("/api/sessions/batch-delete"))
        .json(&json!({ "ids": [id, id, id] }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: Value = resp.json().await.unwrap();
    // Postgres deduplicates in the ANY match — only 1 row deleted.
    assert_eq!(body["deleted"], 1);

    app.cleanup().await;
}

#[tokio::test]
async fn batch_delete_rejects_over_100_ids() {
    let app = spawn_app().await;
    let ids: Vec<Uuid> = (0..101).map(|_| Uuid::new_v4()).collect();

    let resp = app
        .http
        .post(app.url("/api/sessions/batch-delete"))
        .json(&json!({ "ids": ids }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    app.cleanup().await;
}
