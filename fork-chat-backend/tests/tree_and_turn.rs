mod common;

use common::spawn_app;
use serde_json::{Value, json};
use uuid::Uuid;

#[tokio::test]
async fn get_tree_returns_all_turns_ordered_by_created_at() {
    let app = spawn_app().await;
    let session_id = app.create_session(None).await;
    app.mock_openai_success("ok", "resp_tree").await;

    // Root -> left child, root -> right child (three levels if we keep forking)
    let root_body: Value = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "root",
            "provider": "openai",
            "model": "gpt-4o-mini",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let root_id = root_body["turn"]["id"].as_str().unwrap().to_string();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let left_body: Value = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "left",
            "parent_turn_id": root_id,
            "provider": "openai",
            "model": "gpt-4o-mini",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let left_id = left_body["turn"]["id"].as_str().unwrap().to_string();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let right_body: Value = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "right",
            "parent_turn_id": root_id,
            "provider": "openai",
            "model": "gpt-4o-mini",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let right_id = right_body["turn"]["id"].as_str().unwrap().to_string();

    let resp = app
        .http
        .get(app.url(&format!("/api/sessions/{session_id}/tree")))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: Value = resp.json().await.unwrap();
    let turns = body["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 3);
    let ids: Vec<String> = turns
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec![root_id, left_id, right_id]);

    app.cleanup().await;
}

#[tokio::test]
async fn get_tree_returns_404_for_unknown_session() {
    let app = spawn_app().await;
    let missing = Uuid::new_v4();
    let resp = app
        .http
        .get(app.url(&format!("/api/sessions/{missing}/tree")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    app.cleanup().await;
}

#[tokio::test]
async fn get_single_turn_returns_turn_or_404() {
    let app = spawn_app().await;
    let session_id = app.create_session(None).await;
    app.mock_openai_success("yo", "resp_single").await;

    let created: Value = app
        .http
        .post(app.url(&format!("/api/sessions/{session_id}/turns")))
        .json(&json!({
            "user_text": "first",
            "provider": "openai",
            "model": "gpt-4o-mini",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let turn_id = created["turn"]["id"].as_str().unwrap();

    let got = app
        .http
        .get(app.url(&format!("/api/sessions/{session_id}/turns/{turn_id}")))
        .send()
        .await
        .unwrap();
    assert!(got.status().is_success());
    let body: Value = got.json().await.unwrap();
    assert_eq!(body["turn"]["id"], turn_id);

    let unknown = Uuid::new_v4();
    let missing = app
        .http
        .get(app.url(&format!("/api/sessions/{session_id}/turns/{unknown}")))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    app.cleanup().await;
}

#[tokio::test]
async fn get_single_turn_returns_404_for_turn_from_another_session() {
    let app = spawn_app().await;
    let first_session = app.create_session(None).await;
    let second_session = app.create_session(None).await;
    app.mock_openai_success("yo", "resp_cross_get").await;

    let created: Value = app
        .http
        .post(app.url(&format!("/api/sessions/{first_session}/turns")))
        .json(&json!({
            "user_text": "private to first session",
            "provider": "openai",
            "model": "gpt-4o-mini",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let foreign_turn_id = created["turn"]["id"].as_str().unwrap();

    let resp = app
        .http
        .get(app.url(&format!(
            "/api/sessions/{second_session}/turns/{foreign_turn_id}"
        )))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    app.cleanup().await;
}
