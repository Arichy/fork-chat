mod common;

use common::spawn_app;

#[tokio::test]
async fn get_config_returns_default_models() {
    let app = spawn_app().await;

    let resp = app
        .http
        .get(app.url("/api/config"))
        .send()
        .await
        .expect("request failed");

    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();

    let models = body["models"].as_array().expect("models array");
    assert_eq!(models.len(), 2);
    assert_eq!(models[0]["id"], "gpt-4o-mini");
    assert_eq!(models[0]["provider"], "openai");
    assert_eq!(models[1]["id"], "gpt-4o");

    app.cleanup().await;
}
