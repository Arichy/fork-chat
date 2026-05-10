mod config;
mod db;
mod error;
mod handlers;
mod llm;
mod models;
mod routes;
mod tooling;
mod turn_lifecycle;
mod turn_runtime;
mod turn_stream;
mod turn_task_manager;

use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use config::AppState;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    // --- Step 1: Initialize structured logging ---
    // Reads `RUST_LOG` env var for filter configuration. Falls back to
    // showing debug-level output from our crate and tower_http so developers
    // see useful request traces without any configuration.
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fork_chat_backend=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // --- Step 2: Load configuration ---
    // Reads config.json + FORK_CHAT_* env overrides.  Fails fast if the file
    // is missing or validation fails.
    let config = config::Config::load()?;
    tracing::info!("Loaded configuration");

    // --- Step 3: Connect to the database ---
    // Creates a sqlx connection pool.  The pool size is determined by sqlx
    // defaults (based on num_cpus) unless overridden by the connection string.
    let db = db::create_pool(&config.database_url).await?;
    tracing::info!("Connected to database");

    // --- Step 3.5: Apply migrations ---
    // Local Docker deploys should be able to come up from an empty Postgres
    // volume without any manual `sqlx migrate run` step. Running migrations on
    // startup keeps that path deterministic while remaining a no-op when the
    // schema is already current.
    sqlx::migrate!("./migrations").run(&db).await?;
    tracing::info!("Applied database migrations");

    // --- Step 4: Recover from prior crashes ---
    // Any turn that was `running` or `awaiting_approval` when the process
    // stopped (crash, SIGKILL, OOM, etc.) is now orphaned — no task is driving
    // it and no SSE hub subscription exists.  We mark these as `failed` so
    // they don't appear stuck in the UI forever and the user can retry them.
    let abandoned = db::turns::fail_abandoned_turns(&db).await?;
    if abandoned > 0 {
        tracing::warn!("Marked {} abandoned turns as failed", abandoned);
    }

    // --- Step 5: Build application state and router ---
    let addr: SocketAddr = config.server_addr.parse()?;
    let state = AppState::new(db, config);

    let app = routes::create_routes(state).layer(
        // CORS is fully permissive (`allow_origin(Any)`, etc.) because this
        // server is intended for local development only.  In production, the
        // frontend would be served from the same origin or the allowed origins
        // would be explicitly listed.
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any),
    );

    // --- Step 6: Bind and serve ---
    tracing::info!("Listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    // `axum::serve` runs until the listener is closed or a fatal error occurs.
    // It gracefully drains in-flight connections on shutdown signals.
    axum::serve(listener, app).await?;

    Ok(())
}
