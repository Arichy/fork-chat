use async_openai::config::OpenAIConfig;
use async_openai::types::responses::{
    CreateResponse, InputItem, InputParam, OutputItem, OutputMessageContent, Response,
};
use async_openai::Client;
use serde_json::Value as JsonValue;
use tracing::debug;

use crate::error::AppError;

pub struct OpenaiAdapter {
    client: Client<OpenAIConfig>,
}

impl OpenaiAdapter {
    pub fn new(client: Client<OpenAIConfig>) -> Self {
        Self { client }
    }

    pub async fn send(
        &self,
        input: Vec<InputItem>,
        model: &str,
        instructions: Option<&str>,
    ) -> Result<Response, AppError> {
        let request = CreateResponse {
            input: InputParam::Items(input),
            model: Some(model.to_string()),
            instructions: instructions.map(|s| s.to_string()),
            ..Default::default()
        };

        debug!("Request to openai: {:?}", request);

        let response = self
            .client
            .responses()
            .create(request)
            .await
            .map_err(|e| AppError::LlmApiError(e.to_string()))?;

        Ok(response)
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
        serde_json::to_value(output).map_err(|e| AppError::Internal(eyre::eyre!("Failed to serialize output: {}", e)))
    }
}
