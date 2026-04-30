//! `ChatAdapter` impl for the Anthropic Messages API.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value as JsonValue;

use crate::error::AppError;
use crate::llm::{ChatAdapter, SendResult};
use crate::models::Turn;

use super::client::AnthropicClient;
use super::message_builder::build_messages;
use super::types::MessagesRequest;

/// Fallback cap sent as `max_tokens` when no per-call override is supplied.
/// Anthropic requires this field; 4096 is a reasonable middle ground.
const DEFAULT_MAX_TOKENS: u32 = 4096;

pub struct AnthropicAdapter {
    client: AnthropicClient,
}

impl AnthropicAdapter {
    pub fn new(base_url: String, api_key: String, timeout: Duration) -> Self {
        Self {
            client: AnthropicClient::new(base_url, api_key, timeout),
        }
    }
}

#[async_trait]
impl ChatAdapter for AnthropicAdapter {
    async fn send(
        &self,
        history: &[Turn],
        new_user_text: &str,
        model: &str,
        instructions: Option<&str>,
    ) -> Result<SendResult, AppError> {
        let messages = build_messages(history, new_user_text);

        let request = MessagesRequest {
            model: model.to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
            system: instructions.map(|s| s.to_string()),
            messages,
        };

        let response = self.client.messages(&request).await?;

        let assistant_text = extract_text(&response.parsed.content);
        let assistant_content = serde_json::to_value(&response.parsed.content).map_err(|e| {
            AppError::Internal(eyre::eyre!(
                "failed to serialize anthropic assistant content: {e}"
            ))
        })?;
        let usage = serde_json::to_value(&response.parsed.usage).map_err(|e| {
            AppError::Internal(eyre::eyre!("failed to serialize anthropic usage: {e}"))
        })?;

        Ok(SendResult {
            assistant_text,
            assistant_content,
            raw_response: Some(response.raw),
            stop_reason: response.parsed.stop_reason.clone(),
            usage: Some(usage),
            response_id: Some(response.parsed.id),
            input_tokens: Some(response.parsed.usage.input_tokens as i32),
            output_tokens: Some(response.parsed.usage.output_tokens as i32),
            cached_tokens: response
                .parsed
                .usage
                .cache_read_input_tokens
                .map(|v| v as i32),
        })
    }
}

fn extract_text(blocks: &[JsonValue]) -> Option<String> {
    let mut out = String::new();
    for block in blocks {
        let is_text = block.get("type").and_then(|v| v.as_str()) == Some("text");
        let Some(text) = block.get("text").and_then(|v| v.as_str()) else {
            continue;
        };
        if !is_text {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(text);
    }
    if out.is_empty() { None } else { Some(out) }
}
