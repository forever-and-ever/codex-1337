//! Native classifications retain plaintext evidence and the owning account.
//! A decision is publishable only after a complete, bounded structured response.

use super::LunaSampler;
use super::LunaSamplerError;
use super::MAX_OUTPUT_BYTES;
use super::execution::record_token_usage;
use codex_api::AnthropicCachePolicy;
use codex_api::AnthropicCacheTtl;
use codex_api::AnthropicClient;
use codex_api::AnthropicOperation;
use codex_api::AnthropicPrompt;
use codex_api::AnthropicRequestOptions;
use codex_api::AnthropicStopReason;
use codex_api::AnthropicStreamOptions;
use codex_api::AnthropicTool;
use codex_api::ApiError;
use codex_api::ReqwestTransport;
use codex_api::ResponseEvent;
use codex_api::ResponseStream;
use codex_api::TransportError;
use codex_api::anthropic_stop_reason;
use codex_api::encode_anthropic_request;
use codex_extension_api::ExtensionMetrics;
use codex_http_client::ClientRouteClass;
use codex_login::default_client::ClientRedirectPolicy;
use codex_login::default_client::create_client_for_route_async;
use codex_model_provider::AnthropicAuthContext;
use codex_models_manager::manager::RefreshStrategy;
use codex_protocol::error::CodexErr;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::GuardianRiskLevel;
use http::StatusCode;
use serde_json::json;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tokio::sync::OnceCell;

pub(super) struct Catalog {
    account_id: String,
    model: OnceCell<ModelInfo>,
}

pub(super) async fn model(
    sampler: &LunaSampler,
    account_id: &str,
) -> Result<ModelInfo, LunaSamplerError> {
    // A single retained scope bounds memory; only successful discovery is cached.
    // Replacing it never mutates a catalogue already borrowed by a dispatched request.
    let catalog = {
        let mut catalog = sampler.native_catalog.lock().await;
        if let Some(catalog) = catalog
            .as_ref()
            .filter(|catalog| catalog.account_id == account_id) { Arc::clone(catalog) } else {
            let scope = Arc::new(Catalog {
                account_id: account_id.into(),
                model: OnceCell::new(),
            });
            *catalog = Some(Arc::clone(&scope));
            scope
        }
    };
    catalog
        .model
        .get_or_try_init(|| async {
            let provider = sampler
                .config
                .provider
                .for_anthropic_account(&catalog.account_id)
                .map_err(LunaSamplerError::Provider)?;
            let models = provider.models_manager_without_cache(/*config_model_catalog*/ None);
            models
                .raw_model_catalog(
                    RefreshStrategy::OnlineIfUncached,
                    sampler.config.http_client_factory.clone(),
                )
                .await
                .models
                .into_iter()
                .find(|model| {
                    model.slug == sampler.config.provider.approval_review_preferred_model()
                        && model.anthropic.is_some()
                })
                .ok_or_else(|| {
                    LunaSamplerError::Provider(CodexErr::InvalidRequest(
                        "Native Guardian classifier model is unavailable".into(),
                    ))
                })
        })
        .await
        .cloned()
}

pub(super) async fn sample(
    sampler: &LunaSampler,
    context: Arc<AnthropicAuthContext>,
    model: &ModelInfo,
    input: &[ResponseItem],
    account_id: &str,
    scored: Arc<AtomicBool>,
) -> Result<String, LunaSamplerError> {
    // Never reinterpret parent signatures for a separate classifier or provider.
    if input.iter().any(|item| {
        matches!(
            item,
            ResponseItem::Reasoning { .. }
                | ResponseItem::Compaction { .. }
                | ResponseItem::NativeCompaction { .. }
                | ResponseItem::ContextCompaction { .. }
        )
    }) {
        return Err(LunaSamplerError::IncompatibleCompaction);
    }
    let tools: Arc<[AnthropicTool]> = Arc::from([]);
    let schema = json!({
        "type": "object", "properties": {"risk": {"type": "string", "enum": ["low", "high"]}},
        "required": ["risk"], "additionalProperties": false
    });
    let request = encode_anthropic_request(
        &AnthropicPrompt {
            input,
            instructions: "",
            tools: &tools,
            parallel_tool_calls: false,
            output_schema: Some(&schema),
        },
        &AnthropicRequestOptions {
            model,
            reasoning_effort: Some(&ReasoningEffort::None),
            max_output_tokens: NonZeroU32::MIN.saturating_add(/*rhs*/ 255),
            cache: AnthropicCachePolicy::Ephemeral(AnthropicCacheTtl::FiveMinutes),
            operation: AnthropicOperation::Inference,
        },
    )
    .map_err(LunaSamplerError::Api)?;
    let mut captured = context
        .capture(account_id)
        .await
        .map_err(LunaSamplerError::Provider)?;
    let _permit = Arc::clone(&sampler.connections.classifications)
        .acquire_owned()
        .await
        .map_err(|_| LunaSamplerError::Superseded)?;
    let provider = sampler
        .config
        .provider
        .info()
        .to_api_provider(/*auth_mode*/ None)
        .map_err(LunaSamplerError::Provider)?;
    let http = create_client_for_route_async(
        sampler.config.http_client_factory.clone(),
        provider.url_for_path("/messages"),
        ClientRouteClass::Api,
        ClientRedirectPolicy::Reject,
    )
    .await
    .map_err(|error| {
        LunaSamplerError::Api(ApiError::Transport(TransportError::Build(
            error.to_string(),
        )))
    })?;
    let transport = ReqwestTransport::from_http_client(http.without_request_logging());
    let client = AnthropicClient::new(transport.clone(), provider.clone(), captured.auth());
    let stream = match client
        .stream_request(request.clone(), AnthropicStreamOptions::default())
        .await
    {
        Ok(stream) => stream,
        Err(
            ApiError::Transport(TransportError::Http {
                status: StatusCode::UNAUTHORIZED,
                ..
            })
            | ApiError::Api {
                status: StatusCode::UNAUTHORIZED,
                ..
            },
        ) => {
            // Only an explicit pre-stream rejection permits one same-account replay.
            captured = captured
                .refresh_after_rejection()
                .await
                .map_err(LunaSamplerError::Provider)?;
            AnthropicClient::new(transport, provider, captured.auth())
                .stream_request(request, AnthropicStreamOptions::default())
                .await
                .map_err(LunaSamplerError::Api)?
        }
        Err(error) => return Err(LunaSamplerError::Api(error)),
    };
    decision(stream, sampler.config.metrics.as_deref(), scored).await
}

async fn decision(
    mut stream: ResponseStream,
    metrics: Option<&dyn ExtensionMetrics>,
    scored: Arc<AtomicBool>,
) -> Result<String, LunaSamplerError> {
    let mut output = String::new();
    while let Some(event) = stream.rx_event.recv().await {
        match event.map_err(LunaSamplerError::Api)? {
            ResponseEvent::OutputTextDelta { delta, .. } => {
                if output.len().saturating_add(delta.len()) > MAX_OUTPUT_BYTES {
                    return Err(LunaSamplerError::OutputTooLarge);
                }
                output.push_str(&delta);
            }
            ResponseEvent::Completed {
                token_usage,
                usage_metadata,
                end_turn,
                ..
            } => {
                record_token_usage(metrics, token_usage.as_ref());
                if end_turn != Some(true)
                    || !matches!(
                        usage_metadata.as_ref().and_then(anthropic_stop_reason),
                        Some(AnthropicStopReason::EndTurn)
                    )
                {
                    return Err(LunaSamplerError::MissingOutput);
                }
                let value: serde_json::Value =
                    serde_json::from_str(&output).map_err(|_| LunaSamplerError::MissingOutput)?;
                let object = value
                    .as_object()
                    .filter(|object| object.len() == 1)
                    .ok_or(LunaSamplerError::MissingOutput)?;
                let risk: GuardianRiskLevel = serde_json::from_value(
                    object
                        .get("risk")
                        .cloned()
                        .ok_or(LunaSamplerError::MissingOutput)?,
                )
                .map_err(|_| LunaSamplerError::MissingOutput)?;
                let decision = match risk {
                    GuardianRiskLevel::Low => "low",
                    GuardianRiskLevel::High => "high",
                    GuardianRiskLevel::Medium | GuardianRiskLevel::Critical => {
                        return Err(LunaSamplerError::MissingOutput);
                    }
                };
                scored.store(true, Ordering::Relaxed);
                return Ok(decision.into());
            }
            _ => {}
        }
    }
    Err(LunaSamplerError::MissingOutput)
}

#[cfg(test)]
#[path = "../sampler_native_tests.rs"]
mod tests;

impl LunaSampler {
    pub(crate) fn supports_retained_history(&self) -> bool {
        self.config.provider.info().is_openai()
            && matches!(self.config.provider.anthropic_auth_context(), Ok(None))
    }
}
