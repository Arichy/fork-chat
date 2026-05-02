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
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fork_chat_backend=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = config::Config::load()?;
    tracing::info!("Loaded configuration");

    let db = db::create_pool(&config.database_url).await?;
    tracing::info!("Connected to database");
    let abandoned = db::turns::fail_abandoned_turns(&db).await?;
    if abandoned > 0 {
        tracing::warn!("Marked {} abandoned turns as failed", abandoned);
    }

    let addr: SocketAddr = config.server_addr.parse()?;
    let state = AppState::new(db, config);

    let app = routes::create_routes(state).layer(
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any),
    );

    tracing::info!("Listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
