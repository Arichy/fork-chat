use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::config::AppState;
use crate::config::ModelConfig;

#[derive(Debug, Serialize)]
pub struct ConfigResponse {
    pub models: Vec<ModelConfig>,
}

pub async fn get_config_handler(State(state): State<AppState>) -> Json<ConfigResponse> {
    Json(ConfigResponse {
        models: state.config.models.clone(),
    })
}
