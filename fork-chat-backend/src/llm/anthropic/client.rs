//! Thin `reqwest` client for the Anthropic Messages API. No retry layer —
//! upstream retries (like async-openai's) tend to hang wiremock-based tests on
//! simulated 5xx, and retry belongs in the calling layer anyway if we ever
//! want it.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::Value as JsonValue;
use tracing::debug;

use crate::error::AppError;

use super::types::{MessagesRequest, MessagesResponse};

pub struct MessagesResult {
    pub parsed: MessagesResponse,
    pub raw: JsonValue,
}

pub struct AnthropicClient {
    http: reqwest::Client,
    base_url: String,
}

impl AnthropicClient {
    pub fn new(base_url: String, api_key: String, timeout: Duration) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&api_key).expect("invalid api_key header value"),
        );
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        headers.insert("content-type", HeaderValue::from_static("application/json"));

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(timeout)
            .build()
            .expect("failed to build reqwest client");

        Self { http, base_url }
    }

    pub async fn messages(&self, req: &MessagesRequest) -> Result<MessagesResult, AppError> {
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));

        debug!("Anthropic POST {url}: {req:?}");

        let resp = self
            .http
            .post(&url)
            .json(req)
            .send()
            .await
            .map_err(|e| AppError::LlmApiError(format!("anthropic request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::LlmApiError(format!("anthropic {status}: {body}")));
        }

        let raw = resp
            .json::<JsonValue>()
            .await
            .map_err(|e| AppError::LlmApiError(format!("anthropic decode failed: {e}")))?;

        let parsed = serde_json::from_value::<MessagesResponse>(raw.clone())
            .map_err(|e| AppError::LlmApiError(format!("anthropic parse failed: {e}")))?;

        Ok(MessagesResult { parsed, raw })
    }
}
