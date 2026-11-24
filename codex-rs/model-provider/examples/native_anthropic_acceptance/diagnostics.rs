//! Bounded, credential-redacted diagnostics; never retain headers, URLs or body dumps.

use codex_api::AnthropicStreamError;
use codex_api::ApiError;
use codex_api::TransportError;
use serde_json::Value;
use serde_json::json;

pub(super) struct Diagnostics {
    // Deliberately has no Debug or Serialize implementation.
    bearer: String,
}

impl Diagnostics {
    pub(super) fn new(bearer: String) -> Self {
        Self { bearer }
    }

    pub(super) fn text(&self, value: &str) -> String {
        let redacted = value.replace(&self.bearer, "[REDACTED]");
        let without_urls: String = redacted
            .split_inclusive(char::is_whitespace)
            .map(|part| {
                if part.contains("https://") || part.contains("http://") {
                    "[URL OMITTED] "
                } else {
                    part
                }
            })
            .collect();
        without_urls.chars().take(2048).collect()
    }

    pub(super) fn failure(&self, error: &anyhow::Error) -> Value {
        let mut value = match error.downcast_ref::<ApiError>() {
            Some(error) => self.api_error(error),
            None => json!({"kind":"local_validation","message":self.text(&format!("{error:#}"))}),
        };
        // API failures have an explicit local phase context, not an error-body Display dump.
        if error.downcast_ref::<ApiError>().is_some() {
            value["context"] = json!(self.text(&error.to_string()));
        }
        value
    }

    pub(super) fn api_error(&self, error: &ApiError) -> Value {
        match error {
            ApiError::Transport(TransportError::Http {
                status,
                headers,
                body,
                ..
            }) => {
                let parsed = body
                    .as_ref()
                    .filter(|body| body.len() <= 65536)
                    .and_then(|body| serde_json::from_str::<Value>(body).ok());
                let request_id = headers
                    .as_ref()
                    .and_then(|headers| {
                        headers
                            .get("request-id")
                            .or_else(|| headers.get("x-request-id"))
                    })
                    .and_then(|id| id.to_str().ok())
                    .or_else(|| parsed.as_ref().and_then(|body| body["request_id"].as_str()));
                let field = |name: &str| {
                    parsed
                        .as_ref()
                        .and_then(|body| body["error"][name].as_str())
                        .map(|text| self.text(text))
                };
                json!({"kind":"terminal_rejection","http_status":status.as_u16(),
                    "upstream_request_id":request_id.map(|id| self.text(id).chars().take(128).collect::<String>()),
                    "error_type":field("type").map(|kind| kind.chars().take(128).collect::<String>()),
                    "message":field("message").unwrap_or_else(||
                        "No bounded JSON error.message; body omitted".into())})
            }
            ApiError::Api { status, message } => json!({"kind":"terminal_rejection",
                "http_status":status.as_u16(),"message":self.text(message)}),
            ApiError::AnthropicStream { progress, source } => {
                let kind = match source {
                    AnthropicStreamError::Transport(_) | AnthropicStreamError::IdleTimeout(_) => {
                        "uncertain_transport"
                    }
                    AnthropicStreamError::Truncated => "uncertain_truncation",
                    AnthropicStreamError::MalformedSse(_) => {
                        "local_stream_validation_outcome_unknown"
                    }
                };
                json!({"kind":kind,"progress":format!("{progress:?}"),
                    "message":"Anthropic stream error; upstream detail omitted"})
            }
            ApiError::NativeStream(message) => {
                if let Some((kind, text)) = message
                    .strip_prefix("Anthropic ")
                    .and_then(|message| message.split_once(": "))
                    .filter(|(kind, _)| kind.ends_with("_error"))
                {
                    return json!({"kind":"terminal_upstream_error",
                        "error_type":self.text(kind),"message":self.text(text)});
                }
                let kind = if message.contains("truncated before message_stop") {
                    "uncertain_truncation"
                } else if message.contains("outcome unknown") || message.contains("timeout") {
                    "uncertain_transport"
                } else {
                    "local_stream_validation_outcome_unknown"
                };
                json!({"kind":kind,"message":self.text(message)})
            }
            ApiError::InvalidRequest { message } | ApiError::InvalidPrompt { message } => {
                json!({"kind":"local_validation","message":self.text(message)})
            }
            ApiError::Transport(_) | ApiError::Stream(_) => {
                json!({"kind":"uncertain_transport","message":self.text(&error.to_string())})
            }
            ApiError::WebsocketMessageTooLarge => json!({"kind":"websocket_message_too_large"}),
            ApiError::ResponseIncomplete(_) => {
                json!({"kind":"response_incomplete","message":self.text(&error.to_string())})
            }
            ApiError::SessionBudgetExceeded
            | ApiError::AccessProgramNotEnabled { .. }
            | ApiError::InvalidAudioInput { .. }
            | ApiError::InferenceHookDenied { .. }
            | ApiError::ContentPolicyViolation { .. }
            | ApiError::ContextWindowExceeded
            | ApiError::ContentFilter
            | ApiError::QuotaExceeded
            | ApiError::SpendLimitExceeded(_)
            | ApiError::ModelNotFound { .. }
            | ApiError::UsageNotIncluded
            | ApiError::Retryable { .. }
            | ApiError::RateLimitExceeded { .. }
            | ApiError::RateLimit(_)
            | ApiError::CyberPolicy { .. }
            | ApiError::BioPolicy { .. }
            | ApiError::MisalignmentPolicyViolation { .. }
            | ApiError::PolicyCapacityUnavailable { .. }
            | ApiError::FlexUnavailable
            | ApiError::ServerOverloaded { .. } => {
                json!({"kind":"terminal_api_error","message":self.text(&error.to_string())})
            }
        }
    }
}
