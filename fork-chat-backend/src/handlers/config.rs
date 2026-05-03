//! Handler for the `/api/config` endpoint.
//!
//! This endpoint returns a read-only snapshot of the server's configuration:
//! supported protocols, available providers (with their models and supported
//! protocols), and built-in tools.  The frontend calls this at startup to
//! populate the UI (model dropdowns, protocol selector, tool approval
//! policies) without hardcoding any provider information.

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::config::AppState;
use crate::config::{ModelConfig, Protocol};
use crate::tooling::PublicTool;
use crate::tooling::public_tools;

/// A provider as exposed to the frontend.  Contains the provider's name, the
/// protocols it supports (as string labels), and its model list.
#[derive(Debug, Serialize)]
pub struct PublicProvider {
    pub name: String,
    pub supported_protocols: Vec<&'static str>,
    pub models: Vec<ModelConfig>,
}

/// Response body for `GET /api/config`.
///
/// - `protocols`: all protocols the server knows about (always ["openai",
///   "anthropic"] for now).
/// - `providers`: all configured providers with their models.
/// - `tools`: all built-in tools with their default approval policies.
#[derive(Debug, Serialize)]
pub struct ConfigResponse {
    pub protocols: Vec<&'static str>,
    pub providers: Vec<PublicProvider>,
    pub tools: Vec<PublicTool>,
}

/// `GET /api/config` — returns the server's configuration for frontend
/// initialization.
///
/// This is a simple read-only handler that transforms the internal
/// `ProviderConfig` list into the public `PublicProvider` format, stripping
/// out sensitive fields like API keys.  The tool list comes from the
/// `tooling` module which defines all built-in tools.
pub async fn get_config_handler(State(state): State<AppState>) -> Json<ConfigResponse> {
    // Map internal provider configs to the public response format.
    // This strips credentials (api_key, base_url) from the response since
    // the frontend only needs the provider name, supported protocols, and
    // model list to populate its UI.
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
        // Hardcoded list of known protocols.  When a new protocol is added to
        // the `Protocol` enum, it must also be added here for the frontend to
        // discover it.
        protocols: vec![Protocol::Openai.as_str(), Protocol::Anthropic.as_str()],
        providers,
        tools: public_tools(),
    })
}
