mod common;

use common::spawn_app;

#[tokio::test]
async fn healthz_returns_ok_when_server_and_database_are_ready() {
    let app = spawn_app().await;

    let resp = app
        .http
        .get(app.url("/healthz"))
        .send()
        .await
        .expect("request failed");

    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");

    app.cleanup().await;
}
