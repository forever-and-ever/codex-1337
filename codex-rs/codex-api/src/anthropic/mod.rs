mod cache;
mod history;
pub(crate) mod limits;
mod request;
mod salvage;
mod tool_alias;
mod tool_schema;

#[cfg(test)]
#[path = "history_tests.rs"]
mod history_tests;

pub use request::encode_anthropic_request;
pub use salvage::AnthropicRepair;
pub use salvage::AnthropicSalvage;
pub use salvage::salvage_anthropic_history;
pub use tool_schema::anthropic_function_input_schema;

use crate::ApiError;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort;
use serde::Serialize;
use serde_json::Value;
use serde_json::value::RawValue;
use std::num::NonZeroU32;
use std::sync::Arc;

mod client;
mod decoder;
mod stream;
mod usage;

pub use client::AnthropicClient;
pub use client::AnthropicStreamOptions;
pub use decoder::AnthropicStopReason;
pub use decoder::anthropic_context_window_exceeded;
pub use decoder::anthropic_output_limit_exceeded;
pub use decoder::anthropic_refused;
pub use decoder::anthropic_stop_reason;

/// Canonical history borrowed by the native Messages encoder, without a Responses payload.
pub struct AnthropicPrompt<'a> {
    pub input: &'a [ResponseItem],
    pub instructions: &'a str,
    pub tools: &'a Arc<[AnthropicTool]>,
    pub parallel_tool_calls: bool,
    pub output_schema: Option<&'a Value>,
}

pub struct AnthropicRequestOptions<'a> {
    pub model: &'a ModelInfo,
    pub reasoning_effort: Option<&'a ReasoningEffort>,
    pub max_output_tokens: NonZeroU32,
    pub cache: AnthropicCachePolicy,
    pub operation: AnthropicOperation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicOperation {
    Inference,
    Compact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicCachePolicy {
    Disabled,
    Ephemeral(AnthropicCacheTtl),
}

pub use codex_protocol::openai_models::AnthropicCacheTtl;

/// The logical tool address remains independent of its flattened upstream alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnthropicToolIdentity {
    Function {
        name: String,
        namespace: Option<String>,
    },
    Custom {
        name: String,
        namespace: Option<String>,
    },
}

/// Client-executed tool projected outside codex-api from the canonical tool catalogue.
/// Identity is captured with its declaration but never sent to the upstream service.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnthropicTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(skip)]
    pub identity: AnthropicToolIdentity,
}

/// Native Messages JSON paired with the exact tool identities used by this request.
/// Only the encoder can construct it; callers cannot mutate the body or alias metadata.
#[derive(Clone)]
pub struct AnthropicRequest {
    body: Arc<RawValue>,
    tools: Arc<[AnthropicTool]>,
    required_betas: Arc<[&'static str]>,
}

impl AnthropicRequest {
    // Bound the wire representation before transport or differential inspection.
    const MAX_BODY_BYTES: usize = 32_000_000;

    pub(crate) fn from_body(
        body: Value,
        tools: Arc<[AnthropicTool]>,
        required_betas: Arc<[&'static str]>,
    ) -> Result<Self, ApiError> {
        let body =
            serde_json::value::to_raw_value(&body).map_err(|err| ApiError::InvalidRequest {
                message: format!("failed to encode Anthropic request: {err}"),
            })?;
        if body.get().len() > Self::MAX_BODY_BYTES {
            return Err(ApiError::InvalidRequest {
                message: format!("Anthropic request exceeds {} bytes", Self::MAX_BODY_BYTES),
            });
        }
        Ok(Self {
            body: Arc::from(body),
            tools,
            required_betas,
        })
    }

    pub fn body(&self) -> &RawValue {
        &self.body
    }

    pub fn tools(&self) -> &[AnthropicTool] {
        &self.tools
    }

    pub(crate) fn required_betas(&self) -> &[&'static str] {
        &self.required_betas
    }
}
