//! LLM integration layer: wire-protocol adapters + a provider registry that
//! dispatches requests to the right concrete adapter based on a session's
//! `protocol` and its turn's `provider` name.
//!
//! Only two protocols are implemented: `openai` (via the `async-openai` crate's
//! Responses API) and `anthropic` (hand-rolled `reqwest` client). Additional
//! vendors (DeepSeek, GLM, Kimi, ...) are pure config entries under whichever
//! protocol they speak — no new adapter needed.

pub mod anthropic;
pub mod openai;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value as JsonValue;

use crate::config::{Config, Protocol};
use crate::error::AppError;
use crate::models::Turn;

/// Result of a single adapter call.
#[derive(Debug, Clone)]
pub struct SendResult {
    pub assistant_text: Option<String>,
    /// Protocol-native assistant output items/blocks for this round.
    pub assistant_content: JsonValue,
    /// Full upstream response payload for this round, when available.
    pub raw_response: Option<JsonValue>,
    /// Provider stop reason/status for this round, if exposed.
    pub stop_reason: Option<String>,
    /// Provider usage payload for this round, kept as raw JSON for audit/debug.
    pub usage: Option<JsonValue>,
    pub response_id: Option<String>,
    pub input_tokens: Option<i32>,
    pub output_tokens: Option<i32>,
    pub cached_tokens: Option<i32>,
}

/// Trait implemented once per wire protocol.
#[async_trait]
pub trait ChatAdapter: Send + Sync {
    async fn send(
        &self,
        history: &[Turn],
        new_user_text: &str,
        model: &str,
        instructions: Option<&str>,
    ) -> Result<SendResult, AppError>;
}

/// Startup-built registry that owns one concrete adapter per `(protocol,
/// provider_name)` pair present in [`Config::providers`].
pub struct ProviderRegistry {
    entries: HashMap<Protocol, HashMap<String, Arc<dyn ChatAdapter>>>,
}

impl ProviderRegistry {
    /// Build the default registry: one async-openai client per openai binding,
    /// one reqwest-based Anthropic client per anthropic binding.
    pub fn from_config(config: &Config) -> Self {
        Self::from_config_with(config, RegistryOptions::default())
    }

    /// Same as [`Self::from_config`] but with explicit backoff / timeout tuning.
    /// Useful in tests (e.g. zero-retry to avoid hangs on simulated 5xx).
    pub fn from_config_with(config: &Config, opts: RegistryOptions) -> Self {
        let mut entries: HashMap<Protocol, HashMap<String, Arc<dyn ChatAdapter>>> = HashMap::new();

        for provider in &config.providers {
            for (&protocol, binding) in &provider.protocols {
                let adapter: Arc<dyn ChatAdapter> = match protocol {
                    Protocol::Openai => Arc::new(openai::OpenaiAdapter::new(
                        &binding.base_url,
                        &binding.api_key,
                        opts.openai_no_retry,
                    )),
                    Protocol::Anthropic => Arc::new(anthropic::AnthropicAdapter::new(
                        binding.base_url.clone(),
                        binding.api_key.clone(),
                        opts.anthropic_timeout,
                    )),
                };
                entries
                    .entry(protocol)
                    .or_default()
                    .insert(provider.name.clone(), adapter);
            }
        }

        Self { entries }
    }

    pub fn get(&self, protocol: Protocol, provider_name: &str) -> Option<Arc<dyn ChatAdapter>> {
        self.entries
            .get(&protocol)
            .and_then(|m| m.get(provider_name))
            .cloned()
    }
}

/// Knobs used by [`ProviderRegistry::from_config_with`]. Tests disable the
/// async-openai default exponential backoff so simulated 5xx responses don't
/// cause long retry waits.
#[derive(Debug, Clone)]
pub struct RegistryOptions {
    pub openai_no_retry: bool,
    pub anthropic_timeout: Duration,
}

impl Default for RegistryOptions {
    fn default() -> Self {
        Self {
            openai_no_retry: false,
            anthropic_timeout: Duration::from_secs(60),
        }
    }
}
