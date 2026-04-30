mod common;

use common::spawn_app;
use fork_chat_backend::models::Session;
use fork_chat_backend::openai::build_input_for_turn;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

async fn insert_session(db: &PgPool) -> Session {
    sqlx::query_as::<_, Session>("INSERT INTO sessions DEFAULT VALUES RETURNING *")
        .fetch_one(db)
        .await
        .unwrap()
}

async fn insert_turn(
    db: &PgPool,
    session_id: Uuid,
    parent: Option<Uuid>,
    user_text: Option<&str>,
    assistant_text: Option<&str>,
    raw_items: Value,
) -> Uuid {
    sqlx::query_scalar(
        r#"
        INSERT INTO turns
            (session_id, parent_turn_id, status, user_text, assistant_text, raw_items)
        VALUES ($1, $2, 'completed', $3, $4, $5)
        RETURNING id
        "#,
    )
    .bind(session_id)
    .bind(parent)
    .bind(user_text)
    .bind(assistant_text)
    .bind(raw_items)
    .fetch_one(db)
    .await
    .unwrap()
}

#[tokio::test]
async fn build_input_for_turn_no_parent_returns_single_user_message() {
    let app = spawn_app().await;
    let session = insert_session(&app.db).await;

    let items = build_input_for_turn(&app.db, &session, None, "hi")
        .await
        .expect("build_input_for_turn failed");

    // Serialize back to JSON to inspect shape without depending on private types.
    let serialized = serde_json::to_value(&items).unwrap();
    let arr = serialized.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    let last = &arr[0];
    assert_eq!(last["role"], "user");
    assert_eq!(last["content"], "hi");

    app.cleanup().await;
}

#[tokio::test]
async fn build_input_for_turn_uses_fallback_when_raw_items_empty() {
    let app = spawn_app().await;
    let session = insert_session(&app.db).await;

    let parent = insert_turn(
        &app.db,
        session.id,
        None,
        Some("hello"),
        Some("world"),
        json!([]),
    )
    .await;

    let items = build_input_for_turn(&app.db, &session, Some(parent), "next")
        .await
        .expect("build_input_for_turn failed");
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

    app.cleanup().await;
}

#[tokio::test]
async fn build_input_for_turn_passes_through_raw_items() {
    let app = spawn_app().await;
    let session = insert_session(&app.db).await;

    // Store a canned OpenAI Responses API message item in raw_items.
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

    let parent = insert_turn(
        &app.db,
        session.id,
        None,
        Some("prev question"),
        Some("prev answer"),
        raw,
    )
    .await;

    let items = build_input_for_turn(&app.db, &session, Some(parent), "follow-up")
        .await
        .expect("build_input_for_turn failed");
    let serialized = serde_json::to_value(&items).unwrap();
    let arr = serialized.as_array().unwrap();
    // Expect: the raw message item (not the fallback), then the new user message.
    assert_eq!(arr.len(), 2, "serialized = {serialized}");
    assert_eq!(arr[0]["type"], "message");
    assert_eq!(arr[1]["role"], "user");
    assert_eq!(arr[1]["content"], "follow-up");

    app.cleanup().await;
}

#[tokio::test]
async fn build_input_for_turn_walks_ancestor_chain_in_order() {
    let app = spawn_app().await;
    let session = insert_session(&app.db).await;

    let root = insert_turn(&app.db, session.id, None, Some("q1"), Some("a1"), json!([])).await;
    // Tiny sleep so created_at differs deterministically.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let mid = insert_turn(
        &app.db,
        session.id,
        Some(root),
        Some("q2"),
        Some("a2"),
        json!([]),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let leaf = insert_turn(
        &app.db,
        session.id,
        Some(mid),
        Some("q3"),
        Some("a3"),
        json!([]),
    )
    .await;

    let items = build_input_for_turn(&app.db, &session, Some(leaf), "q4")
        .await
        .expect("build_input_for_turn failed");
    let serialized = serde_json::to_value(&items).unwrap();
    let arr = serialized.as_array().unwrap();

    // Expected: (q1, a1, q2, a2, q3, a3, q4)
    let contents: Vec<&str> = arr
        .iter()
        .map(|v| v["content"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(contents, vec!["q1", "a1", "q2", "a2", "q3", "a3", "q4"]);

    app.cleanup().await;
}
