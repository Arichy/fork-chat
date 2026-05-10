//! Liveness/readiness probe handler for local deploys and orchestration.
//!
//! We keep `/healthz` outside the `/api` namespace because load balancers and
//! container platforms often probe a short, stable root-level path.

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::config::AppState;
use crate::error::Result;

/// JSON response body returned by `GET /healthz`.
#[derive(Debug, Serialize)]
pub struct HealthzResponse {
    pub status: &'static str,
}

/// `GET /healthz` — confirms the server process is alive and the database pool
/// can still serve queries.
///
/// The extra database round-trip matters for readiness probes: if the HTTP
/// server is up but Postgres is unavailable, we would rather fail fast here
/// than advertise the process as healthy and send traffic into a broken app.
pub async fn healthz_handler(State(state): State<AppState>) -> Result<Json<HealthzResponse>> {
    // Use a tiny query instead of relying on "the pool exists" because the
    // database can disappear after startup and we want probes to detect that.
    let _: i32 = sqlx::query_scalar("SELECT 1").fetch_one(&state.db).await?;

    Ok(Json(HealthzResponse { status: "ok" }))
}
