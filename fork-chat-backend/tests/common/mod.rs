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

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use backoff::ExponentialBackoffBuilder;
use fork_chat_backend::config::{AppState, Config, ModelConfig};
use fork_chat_backend::routes::create_routes;
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres as PostgresImage;
use tokio::net::TcpListener;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
    pub openai: MockServer,
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

    pub async fn create_session(&self, system_prompt: Option<&str>) -> Uuid {
        let body = match system_prompt {
            Some(sp) => json!({ "system_prompt": sp }),
            None => json!({}),
        };
        let resp = self
            .http
            .post(self.url("/api/sessions"))
            .json(&body)
            .send()
            .await
            .expect("create_session request failed");
        assert!(
            resp.status().is_success(),
            "create_session returned {}",
            resp.status()
        );
        let v: Value = resp.json().await.expect("invalid session json");
        Uuid::parse_str(v["session"]["id"].as_str().unwrap()).unwrap()
    }

    // -------- OpenAI mocks (async-openai posts to `<base>/responses`) --------

    pub async fn mock_openai_success(&self, text: &str, response_id: &str) {
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": response_id,
                "object": "response",
                "created_at": 0,
                "model": "gpt-4o-mini",
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

    pub async fn mock_openai_failure(&self, status: u16) {
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                "error": { "message": "boom", "type": "server_error" }
            })))
            .expect(1..)
            .mount(&self.openai)
            .await;
    }
}

/// Boot a real Axum server on a random localhost port against a fresh Postgres
/// container, wired up to a wiremock-backed OpenAI endpoint.
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

    let config = Config {
        database_url: db_url,
        openai_api_key: "test-key".to_string(),
        openai_base_url: Some(openai.uri()),
        server_addr: "127.0.0.1:0".to_string(),
        models: vec![
            ModelConfig {
                id: "gpt-4o-mini".to_string(),
                name: "GPT-4o Mini".to_string(),
                provider: "openai".to_string(),
            },
            ModelConfig {
                id: "gpt-4o".to_string(),
                name: "GPT-4o".to_string(),
                provider: "openai".to_string(),
            },
        ],
    };

    // Build AppState directly so we can override async-openai's default
    // ExponentialBackoff. The default `max_elapsed_time` is 15 minutes, which
    // would make our 5xx-error tests hang while async-openai retries.
    let mut openai_config = OpenAIConfig::new().with_api_key(&config.openai_api_key);
    if let Some(base_url) = &config.openai_base_url {
        openai_config = openai_config.with_api_base(base_url);
    }
    let no_retry = ExponentialBackoffBuilder::new()
        .with_max_elapsed_time(Some(Duration::from_millis(0)))
        .build();
    let openai_client = Client::with_config(openai_config).with_backoff(no_retry);
    let state = AppState {
        db: db.clone(),
        config,
        openai_client,
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
        http: reqwest::Client::new(),
        _pg: container,
    }
}
