use super::AnthropicClient;
use super::AnthropicRequest;
use super::usage::Usage;
use crate::ApiError;
use codex_client::EncodedJsonBody;
use codex_client::HttpTransport;
use codex_client::RequestBody;
use codex_protocol::protocol::TokenUsage;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;

/// An inference request re-encoded to replay its prefix without output or streaming.
#[derive(Clone)]
pub struct AnthropicWarmingRequest {
    body: EncodedJsonBody,
    headers: HeaderMap,
}

impl AnthropicWarmingRequest {
    pub fn from_inference(request: &AnthropicRequest) -> Result<Option<Self>, ApiError> {
        let mut body: Value = serde_json::from_str(request.body().get()).map_err(|error| {
            ApiError::InvalidRequest {
                message: format!("invalid Anthropic inference request: {error}"),
            }
        })?;
        if body["thinking"]["type"] == "enabled"
            || body["output_config"].get("format").is_some()
            || body.get("output_format").is_some()
            || body["tool_choice"]["type"]
                .as_str()
                .is_some_and(|choice| matches!(choice, "any" | "tool"))
            || body.get("context_management").is_some()
        {
            return Ok(None);
        }
        body["max_tokens"] = json!(0);
        body["stream"] = json!(false);
        let mut headers = request.api_headers(HeaderMap::new())?;
        headers.insert(
            http::header::ACCEPT,
            HeaderValue::from_static("application/json"),
        );
        Ok(Some(Self {
            body: EncodedJsonBody::encode(&body).map_err(|error| ApiError::InvalidRequest {
                message: format!("failed to encode Anthropic warming request: {error}"),
            })?,
            headers,
        }))
    }

    pub fn retained_bytes(&self) -> usize {
        self.body.as_bytes().len()
    }
}

impl<T: HttpTransport> AnthropicClient<T> {
    /// Sends once: uncertain POSTs, rate limits and authentication rejection are never retried.
    #[tracing::instrument(name = "anthropic.warm", level = "debug", skip_all)]
    pub async fn warm(&self, request: &AnthropicWarmingRequest) -> Result<TokenUsage, ApiError> {
        let response = tokio::time::timeout(
            Duration::from_mins(/*mins*/ 1),
            self.session.execute_with(
                Method::POST,
                "/messages",
                request.headers.clone(),
                /*body*/ None,
                |req| {
                    req.body = Some(RequestBody::EncodedJson(request.body.clone()));
                    req.response_body_limit_bytes = Some(1_000_000);
                },
            ),
        )
        .await
        .map_err(|_| {
            ApiError::NativeStream("Anthropic warming request outcome unknown: timeout".into())
        })??;
        if !response.status.is_success() {
            return Err(ApiError::Api {
                status: response.status,
                message: "Anthropic warming request rejected".into(),
            });
        }
        if response.headers.contains_key(http::header::RETRY_AFTER) {
            return Err(ApiError::NativeStream(
                "Anthropic requested backoff for the warming request".into(),
            ));
        }
        let body: Value = serde_json::from_slice(&response.body).map_err(|error| {
            ApiError::NativeStream(format!("invalid Anthropic warming response: {error}"))
        })?;
        let mut usage = Usage::default();
        usage.update(&body["usage"])?;
        let usage = usage.finish()?;
        tracing::debug!(
            cache_read = usage.cached_input_tokens,
            cache_write = usage.cache_write_input_tokens,
            input = usage.input_tokens,
            output = usage.output_tokens,
            "Anthropic warming request usage"
        );
        if usage.output_tokens != 0 {
            return Err(ApiError::NativeStream(
                "Anthropic warming request unexpectedly generated output".into(),
            ));
        }
        Ok(usage)
    }
}

impl AnthropicRequest {
    pub(super) fn api_headers(&self, mut headers: HeaderMap) -> Result<HeaderMap, ApiError> {
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        let betas = std::iter::once("oauth-2025-04-20")
            .chain(self.required_betas().iter().copied())
            .collect::<Vec<_>>()
            .join(",");
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_str(&betas).map_err(|error| ApiError::InvalidRequest {
                message: format!("invalid Anthropic beta header: {error}"),
            })?,
        );
        Ok(headers)
    }
}
