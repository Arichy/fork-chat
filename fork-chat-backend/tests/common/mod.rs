//! Shared integration-test harness, modelled on the zero-to-production-in-rust
//! `spawn_app` pattern.
//!
//! Each test owns its own Postgres container via `testcontainers`. The container
//! handle lives inside `TestApp`, so when the test function returns and `TestApp`
//! is dropped, `ContainerAsync::drop` tears the container down automatically —
//! including the anonymous Postgres data volume (`docker rm -v`).
//!
//! We deliberately do NOT share a container across tests via a static `OnceCell`:
//! `cargo-nextest` runs each test in its own process, so a static would spawn
//! one container per test AND never get `Drop`-ed at process exit (Rust does not
//! drop statics on normal exit), leaking containers + volumes indefinitely.
//!
//! The `watchdog` feature on `testcontainers` is also enabled so that Ctrl-C
//! during a test run still tears containers down instead of leaving them behind.

#![allow(dead_code)]

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;

use fork_chat_backend::config::{
    AppState, Config, ModelConfig, Protocol, ProtocolBinding, ProviderConfig,
};
use fork_chat_backend::llm::{ProviderRegistry, RegistryOptions};
use fork_chat_backend::routes::create_routes;
use fork_chat_backend::turn_stream::TurnStreamHub;
use fork_chat_backend::turn_task_manager::TurnTaskManager;
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres as PostgresImage;
use tokio::net::TcpListener;
use tokio::time::sleep;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Auto-detect the Docker socket once per process when `DOCKER_HOST` is unset
/// and the standard `/var/run/docker.sock` path isn't available. This lets
/// plain `cargo test` / `cargo nextest` / IDE test runners work out of the box
/// on macOS setups (OrbStack, Colima, rootless Docker Desktop) where the
/// canonical socket lives under `$HOME`.
///
/// If the user already set `DOCKER_HOST`, we never override it.
fn ensure_docker_host() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if std::env::var_os("DOCKER_HOST").is_some() {
            return;
        }
        // `exists()` follows symlinks, so a broken `/var/run/docker.sock`
        // symlink (common on OrbStack) returns false and we fall through.
        if Path::new("/var/run/docker.sock").exists() {
            return;
        }

        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        // Checked in priority order. First hit wins.
        let candidates = [
            home.join(".orbstack/run/docker.sock"),
            home.join(".colima/default/docker.sock"),
            home.join(".docker/run/docker.sock"),
            home.join(".docker/desktop/docker.sock"),
        ];
        for c in candidates {
            if c.exists() {
                // SAFETY: called exactly once at process start before any
                // testcontainers client is constructed.
                unsafe {
                    std::env::set_var("DOCKER_HOST", format!("unix://{}", c.display()));
                }
                return;
            }
        }
    });
}

/// The public handle each integration test interacts with.
///
/// Owns the Postgres container — dropping `TestApp` tears the container down
/// and removes its anonymous data volume.
pub struct TestApp {
    pub address: String,
    pub db: PgPool,
    /// Wiremock instance backing the `openai` provider binding.
    pub openai: MockServer,
    /// Wiremock instance backing the `anthropic` provider binding.
    pub anthropic: MockServer,
    /// Process-local hub used by SSE tests to publish synthetic live events.
    pub turn_stream_hub: Arc<TurnStreamHub>,
    pub http: reqwest::Client,
    // Kept last so it is dropped after `db` (pool closes before container goes
    // away, avoiding a noisy "connection refused" log during teardown).
    _pg: ContainerAsync<PostgresImage>,
}

impl TestApp {
    /// Back-compat no-op. Previously issued an explicit `DROP DATABASE`; now the
    /// per-test container is removed automatically when `TestApp` drops, so all
    /// this does is consume `self` a little earlier if a test wants that.
    pub async fn cleanup(self) {
        // Intentionally empty. Drop impl of `ContainerAsync` removes the
        // container (and its anonymous volume) when `self` goes out of scope.
    }

    // -------- HTTP helpers --------

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.address, path)
    }

    pub async fn get_turn(&self, session_id: Uuid, turn_id: Uuid) -> Value {
        self.http
            .get(self.url(&format!("/api/sessions/{session_id}/turns/{turn_id}")))
            .send()
            .await
            .expect("get turn request failed")
            .json()
            .await
            .expect("invalid turn json")
    }

    pub async fn wait_turn_status(
        &self,
        session_id: Uuid,
        turn_id: Uuid,
        expected: &[&str],
    ) -> Value {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let body = self.get_turn(session_id, turn_id).await;
            let status = body["turn"]["status"].as_str().unwrap_or_default();
            if expected.contains(&status) {
                return body;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for turn {turn_id} status in {:?}, got {} body={}",
                expected,
                status,
                body
            );
            sleep(Duration::from_millis(50)).await;
        }
    }

    /// Create a session locked to the given protocol. Defaults to `openai` when
    /// callers don't care (preserves behaviour of the old single-protocol tests).
    pub async fn create_session(&self, system_prompt: Option<&str>) -> Uuid {
        self.create_session_with(Protocol::Openai, system_prompt)
            .await
    }

    pub async fn create_session_with(
        &self,
        protocol: Protocol,
        system_prompt: Option<&str>,
    ) -> Uuid {
        let mut body = json!({ "protocol": protocol });
        if let Some(sp) = system_prompt {
            body["system_prompt"] = json!(sp);
        }
        let resp = self
            .http
            .post(self.url("/api/sessions"))
            .json(&body)
            .send()
            .await
            .expect("create_session request failed");
        assert!(
            resp.status().is_success(),
            "create_session returned {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
        let v: Value = resp.json().await.expect("invalid session json");
        Uuid::parse_str(v["session"]["id"].as_str().unwrap()).unwrap()
    }

    // -------- OpenAI mocks (async-openai posts to `<base>/responses`) --------

    pub async fn mock_openai_success(&self, text: &str, response_id: &str) {
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(|req: &Request| request_has_openai_tools(req))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": response_id,
                "object": "response",
                "created_at": 0,
                "model": "gpt-5.4-mini",
                "status": "completed",
                "output": [{
                    "type": "message",
                    "id": "msg_test",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{
                        "type": "output_text",
                        "text": text,
                        "annotations": []
                    }]
                }],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 20,
                    "total_tokens": 30,
                    "input_tokens_details": { "cached_tokens": 0 },
                    "output_tokens_details": { "reasoning_tokens": 0 }
                }
            })))
            .expect(1..)
            .mount(&self.openai)
            .await;
    }

    /// Mock a successful OpenAI response that is intentionally delayed.
    ///
    /// Useful for race tests that need a deterministic "in-flight LLM call"
    /// window so cancel can arrive before the upstream response resolves.
    pub async fn mock_openai_delayed_success(
        &self,
        text: &str,
        response_id: &str,
        delay: Duration,
    ) {
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(|req: &Request| request_has_openai_tools(req))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(delay)
                    .set_body_json(json!({
                        "id": response_id,
                        "object": "response",
                        "created_at": 0,
                        "model": "gpt-5.4-mini",
                        "status": "completed",
                        "output": [{
                            "type": "message",
                            "id": "msg_test_delayed",
                            "role": "assistant",
                            "status": "completed",
                            "content": [{
                                "type": "output_text",
                                "text": text,
                                "annotations": []
                            }]
                        }],
                        "usage": {
                            "input_tokens": 10,
                            "output_tokens": 20,
                            "total_tokens": 30,
                            "input_tokens_details": { "cached_tokens": 0 },
                            "output_tokens_details": { "reasoning_tokens": 0 }
                        }
                    })),
            )
            .expect(1..)
            .mount(&self.openai)
            .await;
    }

    pub async fn mock_openai_failure(&self, status: u16) {
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(|req: &Request| request_has_openai_tools(req))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                "error": { "message": "boom", "type": "server_error" }
            })))
            .expect(1..)
            .mount(&self.openai)
            .await;
    }

    pub async fn mock_openai_tool_call(
        &self,
        response_id: &str,
        call_id: &str,
        tool_name: &str,
        arguments_json: &str,
    ) {
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(|req: &Request| request_has_openai_tools(req))
            .and(|req: &Request| !request_has_openai_function_call_output(req))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": response_id,
                "object": "response",
                "created_at": 0,
                "model": "openai/gpt-oss-120b",
                "status": "completed",
                "output": [
                    {
                        "type": "reasoning",
                        "id": "rs_tool",
                        "summary": []
                    },
                    {
                        "type": "function_call",
                        "id": call_id,
                        "call_id": call_id,
                        "name": tool_name,
                        "arguments": arguments_json,
                        "status": "completed"
                    }
                ],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 20,
                    "total_tokens": 30,
                    "input_tokens_details": { "cached_tokens": 0 },
                    "output_tokens_details": { "reasoning_tokens": 0 }
                }
            })))
            .expect(1..)
            .mount(&self.openai)
            .await;
    }

    pub async fn mock_openai_success_after_function_output(&self, text: &str, response_id: &str) {
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(|req: &Request| request_has_openai_tools(req))
            .and(|req: &Request| request_has_openai_function_call_output(req))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": response_id,
                "object": "response",
                "created_at": 0,
                "model": "openai/gpt-oss-120b",
                "status": "completed",
                "output": [{
                    "type": "message",
                    "id": "msg_final",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{
                        "type": "output_text",
                        "text": text,
                        "annotations": []
                    }]
                }],
                "usage": {
                    "input_tokens": 11,
                    "output_tokens": 22,
                    "total_tokens": 33,
                    "input_tokens_details": { "cached_tokens": 0 },
                    "output_tokens_details": { "reasoning_tokens": 0 }
                }
            })))
            .expect(1..)
            .mount(&self.openai)
            .await;
    }

    // -------- Anthropic mocks (posts to `<base>/v1/messages`) --------

    pub async fn mock_anthropic_success(&self, text: &str, response_id: &str) {
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(|req: &Request| request_has_anthropic_tools(req))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": response_id,
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-6",
                "content": [{ "type": "text", "text": text }],
                "stop_reason": "end_turn",
                "usage": {
                    "input_tokens": 11,
                    "output_tokens": 22
                }
            })))
            .expect(1..)
            .mount(&self.anthropic)
            .await;
    }

    pub async fn mock_anthropic_failure(&self, status: u16) {
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(|req: &Request| request_has_anthropic_tools(req))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                "type": "error",
                "error": { "type": "api_error", "message": "boom" }
            })))
            .expect(1..)
            .mount(&self.anthropic)
            .await;
    }

    pub async fn mock_anthropic_tool_use(
        &self,
        response_id: &str,
        tool_use_id: &str,
        tool_name: &str,
        input: Value,
    ) {
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(|req: &Request| request_has_anthropic_tools(req))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": response_id,
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-6",
                "content": [{
                    "type": "tool_use",
                    "id": tool_use_id,
                    "name": tool_name,
                    "input": input
                }],
                "stop_reason": "tool_use",
                "usage": {
                    "input_tokens": 11,
                    "output_tokens": 22
                }
            })))
            .expect(1..)
            .mount(&self.anthropic)
            .await;
    }
}

fn has_builtin_tool_names(tools: &[Value]) -> bool {
    let mut names = tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(|v| v.as_str()))
        .collect::<Vec<_>>();
    names.sort_unstable();
    names == vec!["bash", "read", "write"]
}

fn request_has_openai_tools(req: &Request) -> bool {
    let Ok(body) = req.body_json::<Value>() else {
        return false;
    };
    let Some(tools) = body.get("tools").and_then(|v| v.as_array()) else {
        return false;
    };
    if !has_builtin_tool_names(tools) {
        return false;
    }
    tools.iter().all(|tool| {
        tool.get("type").and_then(|v| v.as_str()) == Some("function")
            && tool.get("parameters").is_some()
    })
}

fn request_has_openai_function_call_output(req: &Request) -> bool {
    let Ok(body) = req.body_json::<Value>() else {
        return false;
    };
    body.get("input")
        .and_then(|v| v.as_array())
        .is_some_and(|items| {
            items
                .iter()
                .any(|it| it.get("type").and_then(|v| v.as_str()) == Some("function_call_output"))
        })
}

fn request_has_anthropic_tools(req: &Request) -> bool {
    let Ok(body) = req.body_json::<Value>() else {
        return false;
    };
    let Some(tools) = body.get("tools").and_then(|v| v.as_array()) else {
        return false;
    };
    if !has_builtin_tool_names(tools) {
        return false;
    }
    tools
        .iter()
        .all(|tool| tool.get("input_schema").is_some() && tool.get("description").is_some())
}

/// Boot a real Axum server on a random localhost port against a fresh Postgres
/// container, wired up to a wiremock-backed OpenAI endpoint plus an
/// Anthropic-shaped one.
pub async fn spawn_app() -> TestApp {
    ensure_docker_host();

    // Use Postgres 17 to match the project's dev database. The default
    // testcontainers-modules tag (11-alpine) lacks built-in gen_random_uuid().
    let container = PostgresImage::default()
        .with_tag("17-alpine")
        .start()
        .await
        .expect("failed to start postgres testcontainer");

    let host = container
        .get_host()
        .await
        .expect("failed to get container host")
        .to_string();
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("failed to get container port");

    // testcontainers-modules::postgres seeds the default `postgres` database
    // with user/password `postgres`/`postgres`. We use that db directly and
    // rely on container isolation for test isolation (one container per test).
    let db_url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
    let db = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("failed to connect to test postgres");

    sqlx::migrate!("./migrations")
        .run(&db)
        .await
        .expect("failed to run migrations on test database");

    let openai = MockServer::start().await;
    let anthropic = MockServer::start().await;

    let config = Config {
        database_url: db_url,
        server_addr: "127.0.0.1:0".to_string(),
        providers: vec![
            ProviderConfig {
                name: "openai".to_string(),
                models: vec![
                    ModelConfig {
                        id: "gpt-5.4-mini".to_string(),
                        name: Some("GPT-5.4 Mini".to_string()),
                    },
                    ModelConfig {
                        id: "gpt-5.5".to_string(),
                        name: Some("GPT-5.5".to_string()),
                    },
                ],
                protocols: {
                    let mut m = HashMap::new();
                    m.insert(
                        Protocol::Openai,
                        ProtocolBinding {
                            base_url: openai.uri(),
                            api_key: "test-key".to_string(),
                        },
                    );
                    m
                },
            },
            ProviderConfig {
                name: "anthropic".to_string(),
                models: vec![
                    ModelConfig {
                        id: "claude-sonnet-4-6".to_string(),
                        name: Some("Claude Sonnet 4.6".to_string()),
                    },
                    ModelConfig {
                        id: "claude-opus-4-7".to_string(),
                        name: Some("Claude Opus 4.7".to_string()),
                    },
                ],
                protocols: {
                    let mut m = HashMap::new();
                    m.insert(
                        Protocol::Anthropic,
                        ProtocolBinding {
                            base_url: anthropic.uri(),
                            api_key: "test-anth-key".to_string(),
                        },
                    );
                    m
                },
            },
        ],
    };

    // Build AppState manually so we can disable async-openai's 15-minute
    // default exponential backoff and cap Anthropic's HTTP timeout: both make
    // our 5xx-error tests would otherwise hang.
    let config_arc = Arc::new(config);
    let registry = ProviderRegistry::from_config_with(
        &config_arc,
        RegistryOptions {
            openai_no_retry: true,
            anthropic_timeout: Duration::from_secs(5),
        },
    );
    let turn_stream_hub = Arc::new(TurnStreamHub::new());
    let state = AppState {
        db: db.clone(),
        config: config_arc,
        registry: Arc::new(registry),
        turn_stream_hub: turn_stream_hub.clone(),
        turn_task_manager: Arc::new(TurnTaskManager::new()),
    };
    let app = create_routes(state);

    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .await
        .expect("failed to bind test listener");
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server crashed");
    });

    TestApp {
        address: format!("http://127.0.0.1:{port}"),
        db,
        openai,
        anthropic,
        turn_stream_hub,
        http: reqwest::Client::new(),
        _pg: container,
    }
}
