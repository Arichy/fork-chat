//! `ChatAdapter` impl for the Anthropic Messages API.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value as JsonValue;

use crate::error::AppError;
use crate::llm::{ChatAdapter, SendResult};
use crate::models::Turn;
use crate::tooling::tool_definitions;

use super::client::AnthropicClient;
use super::message_builder::build_messages;
use super::types::{AnthropicTool, MessagesRequest};

/// Fallback cap sent as `max_tokens` when no per-call override is supplied.
/// Anthropic requires this field; 4096 is a reasonable middle ground.
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Concrete `ChatAdapter` for the Anthropic Messages API.  Uses a hand-rolled
/// `reqwest` client rather than a vendor SDK, giving us full control over
/// request/response serialization and avoiding the retry-hanging issues that
/// the `async-openai` crate's built-in backoff causes in tests.
pub struct AnthropicAdapter {
    client: AnthropicClient,
}

impl AnthropicAdapter {
    /// Create a new adapter with the given API endpoint and credentials.
    pub fn new(base_url: String, api_key: String, timeout: Duration) -> Self {
        Self {
            client: AnthropicClient::new(base_url, api_key, timeout),
        }
    }
}

#[async_trait]
impl ChatAdapter for AnthropicAdapter {
    /// Full send flow for the Anthropic Messages API:
    ///
    /// 1. **Build messages** — reconstruct the Anthropic message array from
    ///    stored turn history via the message builder.
    /// 2. **Build tool definitions** — convert our provider-agnostic tool
    ///    definitions into Anthropic's tool schema format.
    /// 3. **Send request** — the `AnthropicClient` handles the HTTP call.
    /// 4. **Extract results** — pull text, usage, and raw JSON from the
    ///    response for protocol-native storage.
    async fn send(
        &self,
        history: &[Turn],
        new_user_text: Option<&str>,
        model: &str,
        instructions: Option<&str>,
    ) -> Result<SendResult, AppError> {
        // Step 1: reconstruct conversation history as Anthropic messages.
        let messages = build_messages(history, new_user_text);

        // Step 2: convert our shared tool definitions to Anthropic's schema.
        // Anthropic uses a simpler tool shape than OpenAI — just name,
        // description, and input_schema (JSON Schema).
        let tools = tool_definitions()
            .into_iter()
            .map(|tool| AnthropicTool {
                name: tool.name.to_string(),
                description: Some(tool.description.to_string()),
                input_schema: tool.input_schema,
            })
            .collect();

        // Step 3: assemble the request.  `system` is sent as a top-level
        // field (not embedded in messages), per the Anthropic API spec.
        let request = MessagesRequest {
            model: model.to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
            system: instructions.map(|s| s.to_string()),
            messages,
            tools: Some(tools),
        };

        // The client returns both the parsed typed response and the raw JSON,
        // so we can store the raw version for protocol-native persistence.
        let response = self.client.messages(&request).await?;

        // Step 4: extract the results.
        let assistant_text = extract_text(&response.parsed.content);

        // Serialize content blocks as raw JSON for protocol-native storage.
        // Anthropic content blocks can be text, tool_use, tool_result, etc. —
        // we keep them as-is so replay is lossless.
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
            // Store the full raw response JSON for debugging/audit.
            raw_response: Some(response.raw),
            stop_reason: response.parsed.stop_reason.clone(),
            usage: Some(usage),
            response_id: Some(response.parsed.id),
            // Cast from u32 to i32 for DB compatibility.
            input_tokens: Some(response.parsed.usage.input_tokens as i32),
            output_tokens: Some(response.parsed.usage.output_tokens as i32),
            // Anthropic supports prompt caching; the API reports how many input
            // tokens were served from cache, which we pass through for metrics.
            cached_tokens: response
                .parsed
                .usage
                .cache_read_input_tokens
                .map(|v| v as i32),
        })
    }
}

/// Extract the assistant's text reply from Anthropic's content block array.
///
/// Anthropic returns content as an array of typed blocks (text, tool_use,
/// tool_result, etc.).  We walk the array and concatenate all `"type": "text"`
/// blocks, separated by newlines.  Non-text blocks (e.g. tool_use) are
/// ignored — they are still preserved in `assistant_content` for storage, but
/// the UI only needs the text portion.
fn extract_text(blocks: &[JsonValue]) -> Option<String> {
    let mut out = String::new();
    for block in blocks {
        // Only extract blocks explicitly typed as "text".
        let is_text = block.get("type").and_then(|v| v.as_str()) == Some("text");
        let Some(text) = block.get("text").and_then(|v| v.as_str()) else {
            continue;
        };
        if !is_text {
            continue;
        }
        // Multiple text blocks are joined with newlines.  This can happen when
        // the model interleaves text with thinking blocks (extended thinking).
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(text);
    }
    if out.is_empty() { None } else { Some(out) }
}
