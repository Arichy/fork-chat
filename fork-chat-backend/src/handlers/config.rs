use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::config::AppState;
use crate::config::{ModelConfig, Protocol};
use crate::tooling::PublicTool;
use crate::tooling::public_tools;

#[derive(Debug, Serialize)]
pub struct PublicProvider {
    pub name: String,
    pub supported_protocols: Vec<&'static str>,
    pub models: Vec<ModelConfig>,
}

#[derive(Debug, Serialize)]
pub struct ConfigResponse {
    pub protocols: Vec<&'static str>,
    pub providers: Vec<PublicProvider>,
    pub tools: Vec<PublicTool>,
}

pub async fn get_config_handler(State(state): State<AppState>) -> Json<ConfigResponse> {
    let providers = state
        .config
        .providers
        .iter()
        .map(|p| PublicProvider {
            name: p.name.clone(),
            supported_protocols: p
                .supported_protocols()
                .into_iter()
                .map(|p| p.as_str())
                .collect(),
            models: p.models.clone(),
        })
        .collect();

    Json(ConfigResponse {
        protocols: vec![Protocol::Openai.as_str(), Protocol::Anthropic.as_str()],
        providers,
        tools: public_tools(),
    })
}
