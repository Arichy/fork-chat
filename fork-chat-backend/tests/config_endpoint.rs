mod common;

use common::spawn_app;

#[tokio::test]
async fn get_config_returns_protocols_and_providers() {
    let app = spawn_app().await;

    let resp = app
        .http
        .get(app.url("/api/config"))
        .send()
        .await
        .expect("request failed");

    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();

    // Top-level protocols list (order is stable: sorted by name).
    let protocols = body["protocols"].as_array().expect("protocols array");
    assert_eq!(protocols.len(), 2);
    assert!(protocols.iter().any(|p| p == "openai"));
    assert!(protocols.iter().any(|p| p == "anthropic"));

    let providers = body["providers"].as_array().expect("providers array");
    assert_eq!(providers.len(), 2);
    let tools = body["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 3);
    assert_eq!(tools[0]["name"], "read");
    assert_eq!(tools[1]["name"], "write");
    assert_eq!(tools[2]["name"], "bash");

    // Find the openai + anthropic providers by name (no ordering assumed).
    let find = |name: &str| {
        providers
            .iter()
            .find(|p| p["name"] == name)
            .unwrap_or_else(|| panic!("missing provider {name}"))
    };

    let openai = find("openai");
    let openai_protocols = openai["supported_protocols"].as_array().unwrap();
    assert_eq!(openai_protocols, &[serde_json::json!("openai")]);
    let openai_models = openai["models"].as_array().unwrap();
    assert_eq!(openai_models.len(), 2);
    assert_eq!(openai_models[0]["id"], "gpt-5.4-mini");
    assert_eq!(openai_models[1]["id"], "gpt-5.5");

    let anthropic = find("anthropic");
    let anth_protocols = anthropic["supported_protocols"].as_array().unwrap();
    assert_eq!(anth_protocols, &[serde_json::json!("anthropic")]);
    let anth_models = anthropic["models"].as_array().unwrap();
    assert_eq!(anth_models.len(), 2);
    assert_eq!(anth_models[0]["id"], "claude-sonnet-4-6");

    // Secrets must NOT leak.
    let body_str = body.to_string();
    assert!(!body_str.contains("api_key"), "api_key leaked: {body_str}");
    assert!(
        !body_str.contains("base_url"),
        "base_url leaked: {body_str}"
    );

    app.cleanup().await;
}
