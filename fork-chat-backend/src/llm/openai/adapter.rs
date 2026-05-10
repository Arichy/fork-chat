//! OpenAI Responses-API adapter. Uses `async-openai` for wire structs and
//! `reqwest` for transport so upstream status/body diagnostics remain visible.
//! Stays compatible with older turns whose `turn_messages` is an empty array by
//! falling back to the plain user/assistant text pair.

use std::time::Duration;

use async_openai::types::responses::{
    CreateResponse, FunctionTool, InputItem, InputParam, OutputItem, OutputMessageContent,
    Response, Tool,
};
use async_trait::async_trait;
use serde_json::Value as JsonValue;
use tracing::debug;

use crate::error::AppError;
use crate::llm::{ChatAdapter, SendResult};
use crate::models::Turn;
use crate::tooling::tool_definitions;

use super::message_builder::build_input_items;

/// Concrete `ChatAdapter` that talks to the OpenAI Responses API. A single
/// instance is created per (protocol, provider) pair at startup and shared
/// through `Arc` inside the `ProviderRegistry`.
pub struct OpenaiAdapter {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl OpenaiAdapter {
    /// Create a new adapter pointed at the given `base_url`.
    ///
    /// The `_no_retry` argument is kept for registry-level test configuration.
    /// This direct `reqwest` path performs one request per send, so wiremock
    /// tests already fail quickly without SDK retry tuning.
    pub fn new(base_url: &str, api_key: &str, _no_retry: bool) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("failed to build reqwest client");

        Self {
            http,
            base_url: base_url.to_string(),
            api_key: api_key.to_string(),
        }
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

        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                AppError::llm_api_with_source(
                    format!("openai-compatible request failed: POST {url}"),
                    e,
                )
            })?;

        let status = resp.status();
        let request_id = response_request_id(resp.headers());
        let body = resp.text().await.map_err(|e| {
            AppError::llm_api_with_source(
                format!("openai-compatible response body read failed: POST {url}"),
                e,
            )
        })?;

        if !status.is_success() {
            return Err(AppError::llm_api(format!(
                "openai-compatible upstream returned {status}{} from POST {url}: {}",
                format_request_id(&request_id),
                body_preview(&body)
            )));
        }

        serde_json::from_str::<Response>(&body).map_err(|e| {
            AppError::llm_api_with_source(
                format!(
                    "openai-compatible /responses decode failed (status {status}{}, body_len={}, body_preview={})",
                    format_request_id(&request_id),
                    body.len(),
                    body_preview(&body)
                ),
                e,
            )
        })
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

fn response_request_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .or_else(|| headers.get("request-id"))
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

fn format_request_id(request_id: &Option<String>) -> String {
    request_id
        .as_ref()
        .map(|id| format!(", request_id={id}"))
        .unwrap_or_default()
}

fn body_preview(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return "<empty>".to_string();
    }

    let max_chars = 2_000;
    let mut preview: String = body.chars().take(max_chars).collect();

    // Provider errors can include long HTML or gateway pages; cap the copy we
    // store in the turn so diagnostics stay readable and database rows small.
    if body.chars().count() > max_chars {
        preview.push_str("...<truncated>");
    }

    preview
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
        tracing::info!("Sending request to openai");
        // Step 1: reconstruct the conversation history as OpenAI InputItems.
        let input = build_input_items(history, new_user_text);

        // Step 2: send the request to the Responses API.
        let response = self.send_raw(input, model, instructions).await?;
        debug!("Response from openai: {:?}", response);

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
