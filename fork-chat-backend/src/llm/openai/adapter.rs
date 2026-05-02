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

pub struct OpenaiAdapter {
    client: Client<OpenAIConfig>,
}

impl OpenaiAdapter {
    /// `no_retry=true` bypasses `async-openai`'s default 15-minute exponential
    /// backoff. Only used in tests where a wiremock 5xx would otherwise hang.
    pub fn new(base_url: &str, api_key: &str, no_retry: bool) -> Self {
        let openai_config = OpenAIConfig::new()
            .with_api_key(api_key)
            .with_api_base(base_url);
        let client = Client::with_config(openai_config);
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

    /// Low-level send (kept public for tests and anything that wants to build
    /// its own `Vec<InputItem>` without going through the adapter trait).
    pub async fn send_raw(
        &self,
        input: Vec<InputItem>,
        model: &str,
        instructions: Option<&str>,
    ) -> Result<Response, AppError> {
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
            instructions: instructions.map(|s| s.to_string()),
            tools: Some(tools),
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

    pub fn extract_assistant_text(response: &Response) -> Option<String> {
        response
            .output
            .iter()
            .filter_map(|item| match item {
                OutputItem::Message(msg) => Some(msg),
                _ => None,
            })
            .flat_map(|msg| &msg.content)
            .filter_map(|content| match content {
                OutputMessageContent::OutputText(text) => Some(text.text.clone()),
                _ => None,
            })
            .next()
    }

    pub fn extract_usage(response: &Response) -> (Option<i32>, Option<i32>) {
        response
            .usage
            .as_ref()
            .map(|u| (Some(u.input_tokens as i32), Some(u.output_tokens as i32)))
            .unwrap_or((None, None))
    }

    pub fn serialize_output(output: &[OutputItem]) -> Result<JsonValue, AppError> {
        serde_json::to_value(output)
            .map_err(|e| AppError::Internal(eyre::eyre!("Failed to serialize output: {}", e)))
    }
}

#[async_trait]
impl ChatAdapter for OpenaiAdapter {
    async fn send(
        &self,
        history: &[Turn],
        new_user_text: Option<&str>,
        model: &str,
        instructions: Option<&str>,
    ) -> Result<SendResult, AppError> {
        let input = build_input_items(history, new_user_text);
        let response = self.send_raw(input, model, instructions).await?;

        let assistant_text = Self::extract_assistant_text(&response);
        let (input_tokens, output_tokens) = Self::extract_usage(&response);
        let assistant_content = Self::serialize_output(&response.output)?;
        let raw_response = serde_json::to_value(&response).map(Some).map_err(|e| {
            AppError::Internal(eyre::eyre!("Failed to serialize openai response: {e}"))
        })?;
        let usage = response
            .usage
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|e| {
                AppError::Internal(eyre::eyre!("Failed to serialize openai usage: {e}"))
            })?;

        Ok(SendResult {
            assistant_text,
            assistant_content,
            raw_response,
            stop_reason: None,
            usage,
            response_id: Some(response.id.clone()),
            input_tokens,
            output_tokens,
            cached_tokens: None,
        })
    }
}
