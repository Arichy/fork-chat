//! OpenAI Responses-API adapter. Uses the `async-openai` crate. Stays
//! compatible with older turns whose `turn_messages` is an empty array by falling
//! back to the plain user/assistant text pair.

use std::time::Duration;

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::responses::{
    CreateResponse, FunctionTool, InputItem, InputParam, OutputItem, OutputMessageContent,
    Response, Tool,
};
use async_trait::async_trait;
use backoff::ExponentialBackoffBuilder;
use serde_json::Value as JsonValue;
use tracing::debug;

use crate::error::AppError;
use crate::llm::{ChatAdapter, SendResult};
use crate::models::Turn;
use crate::tooling::tool_definitions;

use super::message_builder::build_input_items;

/// Concrete `ChatAdapter` that talks to the OpenAI Responses API via the
/// `async-openai` crate.  A single instance is created per (protocol, provider)
/// pair at startup and shared through `Arc` inside the `ProviderRegistry`.
pub struct OpenaiAdapter {
    client: Client<OpenAIConfig>,
}

impl OpenaiAdapter {
    /// Create a new adapter pointed at the given `base_url`.
    ///
    /// # The `no_retry` flag
    ///
    /// The `async-openai` crate ships with exponential-backoff retry logic that
    /// defaults to a 15-minute window on transient failures.  In tests that use
    /// wiremock to simulate 5xx responses, this retry loop would cause the test
    /// to hang.  When `no_retry` is `true`, we replace the backoff with a
    /// zero-elapsed-time policy so the crate returns the error immediately
    /// instead of retrying.
    pub fn new(base_url: &str, api_key: &str, no_retry: bool) -> Self {
        let openai_config = OpenAIConfig::new()
            .with_api_key(api_key)
            .with_api_base(base_url);
        let client = Client::with_config(openai_config);

        // Zero-out the retry backoff when requested (tests only).  In
        // production the default retry policy is desirable to ride out
        // transient network blips or rate-limit 429s.
        let client = if no_retry {
            let zero = ExponentialBackoffBuilder::new()
                .with_max_elapsed_time(Some(Duration::from_millis(0)))
                .build();
            client.with_backoff(zero)
        } else {
            client
        };
        Self { client }
    }

    /// Low-level send that accepts a pre-built `Vec<InputItem>` and returns the
    /// raw `async-openai` `Response`.  Kept public so tests (and any future
    /// callers that want fine-grained control over input construction) can
    /// bypass the higher-level `ChatAdapter::send` flow.
    pub async fn send_raw(
        &self,
        input: Vec<InputItem>,
        model: &str,
        instructions: Option<&str>,
    ) -> Result<Response, AppError> {
        // Convert our provider-agnostic tool definitions into the OpenAI
        // FunctionTool shape that the Responses API expects.  We set
        // `strict: true` so the model outputs conform to the JSON schema
        // exactly — this avoids ambiguous tool-call parsing on our end.
        let tools: Vec<Tool> = tool_definitions()
            .into_iter()
            .map(|tool| {
                Tool::Function(FunctionTool {
                    name: tool.name.to_string(),
                    parameters: Some(tool.input_schema),
                    strict: Some(true),
                    description: Some(tool.description.to_string()),
                    defer_loading: None,
                })
            })
            .collect();

        let request = CreateResponse {
            input: InputParam::Items(input),
            model: Some(model.to_string()),
            // `instructions` maps to the Responses API "system prompt" field.
            instructions: instructions.map(|s| s.to_string()),
            tools: Some(tools),
            // Allow the model to invoke multiple tools in a single turn so it
            // can e.g. read several files in parallel.
            parallel_tool_calls: Some(true),
            ..Default::default()
        };

        debug!("Request to openai: {:?}", request);

        self.client
            .responses()
            .create(request)
            .await
            .map_err(|e| AppError::LlmApiError(e.to_string()))
    }

    /// Walk the OpenAI Responses API output and pull out the first piece of
    /// assistant text.
    ///
    /// The output is a flat list of `OutputItem` variants.  We only care about
    /// `OutputItem::Message` (the actual text/function-call messages); other
    /// variants like file-search results are ignored.  Within each message we
    /// extract the first `OutputText` content block and return its string.
    pub fn extract_assistant_text(response: &Response) -> Option<String> {
        response
            .output
            .iter()
            // Filter down to only Message variants — skip function calls, file
            // search results, and other non-text output items.
            .filter_map(|item| match item {
                OutputItem::Message(msg) => Some(msg),
                _ => None,
            })
            .flat_map(|msg| &msg.content)
            // Extract only OutputText blocks (skip function_call_output, etc.)
            .filter_map(|content| match content {
                OutputMessageContent::OutputText(text) => Some(text.text.clone()),
                _ => None,
            })
            .next()
    }

    /// Extract token counts from the response.  Returns `(input, output)`.
    ///
    /// The `usage` field is optional in the Responses API — it may be absent on
    /// streamed partial responses or error responses — so we fall back to
    /// `(None, None)` rather than panicking.
    pub fn extract_usage(response: &Response) -> (Option<i32>, Option<i32>) {
        response
            .usage
            .as_ref()
            // Cast from u64 to i32 for compatibility with our DB schema.
            .map(|u| (Some(u.input_tokens as i32), Some(u.output_tokens as i32)))
            .unwrap_or((None, None))
    }

    /// Serialize the response's output items into a JSON value for
    /// protocol-native storage in `turn_messages`.
    ///
    /// This is the key architectural decision: we store the *raw OpenAI output
    /// items* (not a unified format) so that when we reconstruct the input for
    /// a subsequent turn, we can replay them back verbatim.  This avoids any
    /// lossy translation between protocols.
    pub fn serialize_output(output: &[OutputItem]) -> Result<JsonValue, AppError> {
        serde_json::to_value(output)
            .map_err(|e| AppError::Internal(eyre::eyre!("Failed to serialize output: {}", e)))
    }
}

#[async_trait]
impl ChatAdapter for OpenaiAdapter {
    /// Full send flow for the OpenAI Responses API:
    ///
    /// 1. **Build input items** — the `message_builder` reconstructs
    ///    protocol-native `InputItem`s from the stored turn history.  This is
    ///    where protocol-native storage pays off: we replay the exact items
    ///    that were saved, not a lossy translation.
    ///
    /// 2. **Call the API** — delegates to `send_raw()` which handles tool
    ///    definition injection and the actual HTTP call.
    ///
    /// 3. **Extract results** — pull out the assistant's text reply, token
    ///    usage, and serialize the output items for storage.
    async fn send(
        &self,
        history: &[Turn],
        new_user_text: Option<&str>,
        model: &str,
        instructions: Option<&str>,
    ) -> Result<SendResult, AppError> {
        // Step 1: reconstruct the conversation history as OpenAI InputItems.
        let input = build_input_items(history, new_user_text);

        // Step 2: send the request to the Responses API.
        let response = self.send_raw(input, model, instructions).await?;

        // Step 3: extract the various result fields from the response.
        let assistant_text = Self::extract_assistant_text(&response);
        let (input_tokens, output_tokens) = Self::extract_usage(&response);

        // Serialize the output items for protocol-native storage.  This is
        // what gets written into `turn_messages` so subsequent turns can
        // replay it.
        let assistant_content = Self::serialize_output(&response.output)?;

        // Keep the full raw response JSON for debugging/audit purposes.
        let raw_response = serde_json::to_value(&response).map(Some).map_err(|e| {
            AppError::Internal(eyre::eyre!("Failed to serialize openai response: {e}"))
        })?;

        // Usage is optional in the response — guard against its absence.
        let usage = response
            .usage
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|e| {
                AppError::Internal(eyre::eyre!("Failed to serialize openai usage: {e}"))
            })?;

        // OpenAI Responses API does not currently expose a "cached tokens"
        // field, so we always set this to None.
        Ok(SendResult {
            assistant_text,
            assistant_content,
            raw_response,
            // The Responses API doesn't expose a single stop_reason string in
            // the same way the Chat Completions API does; it's embedded in the
            // output items themselves, so we leave this as None.
            stop_reason: None,
            usage,
            response_id: Some(response.id.clone()),
            input_tokens,
            output_tokens,
            cached_tokens: None,
        })
    }
}
