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

/// The result of a Messages API call, carrying both the typed response and
/// the raw JSON.  The raw JSON is needed for protocol-native storage in
/// `turn_messages`, while the typed response provides convenient field access.
pub struct MessagesResult {
    /// The deserialized, typed response for convenient field access.
    pub parsed: MessagesResponse,
    /// The raw JSON body as received from the API.  Stored as-is for
    /// protocol-native persistence so no information is lost in serialization.
    pub raw: JsonValue,
}

/// Thin HTTP client for the Anthropic Messages API.  Uses `reqwest` directly
/// instead of a vendor SDK, giving us full control over serialization and
/// avoiding unwanted retry behavior.
pub struct AnthropicClient {
    http: reqwest::Client,
    base_url: String,
}

impl AnthropicClient {
    /// Create a new client configured with the required Anthropic headers.
    ///
    /// # Required headers
    ///
    /// - `x-api-key`: The Anthropic API key for authentication.
    /// - `anthropic-version`: The API version to use.  We pin to `"2023-06-01"`
    ///   which is the stable Messages API version.  This must be updated if
    ///   we want to use newer features like extended thinking or streaming.
    /// - `content-type`: Always `application/json` for the Messages API.
    ///
    /// These are set as default headers on the `reqwest::Client` so they are
    /// included on every request automatically.
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

    /// Send a Messages API request and return both raw and parsed responses.
    ///
    /// # The double-parse pattern
    ///
    /// We deserialize the response body twice:
    ///
    /// 1. **Raw JSON** (`JsonValue`): stored as `raw` for protocol-native
    ///    persistence.  This preserves every field from the API response,
    ///    including fields we don't explicitly model in `MessagesResponse`.
    ///
    /// 2. **Typed struct** (`MessagesResponse`): stored as `parsed` for
    ///    convenient field access by the adapter layer.
    ///
    /// This pattern ensures that even if `MessagesResponse` doesn't model
    /// every API field, the raw JSON still captures everything for storage
    /// and later analysis.
    pub async fn messages(&self, req: &MessagesRequest) -> Result<MessagesResult, AppError> {
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));

        debug!("Anthropic POST {url}: {req:?}");

        let resp = self
            .http
            .post(&url)
            .json(req)
            .send()
            .await
            .map_err(|e| AppError::llm_api_with_source("anthropic request failed", e))?;

        let status = resp.status();
        if !status.is_success() {
            // Read the error body as text for a more informative error message.
            // Anthropic returns structured error JSON, but we just surface it
            // as-is since the caller can parse it if needed.
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::llm_api(format!("anthropic {status}: {body}")));
        }

        // First parse: raw JSON for storage (preserves all fields).
        let raw = resp
            .json::<JsonValue>()
            .await
            .map_err(|e| AppError::llm_api_with_source("anthropic decode failed", e))?;

        // Second parse: typed struct for convenient access.  We clone the
        // raw value here since `serde_json::from_value` consumes its input.
        let parsed = serde_json::from_value::<MessagesResponse>(raw.clone())
            .map_err(|e| AppError::llm_api_with_source("anthropic parse failed", e))?;

        Ok(MessagesResult { parsed, raw })
    }
}
