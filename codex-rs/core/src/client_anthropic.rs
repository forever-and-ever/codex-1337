//! Native Messages orchestration; history, tools, and events retain their shared owners.

use super::AuthRequestTelemetryContext;
use super::ModelClient;
use super::ModelClientSession;
use super::PendingUnauthorizedRetry;
use super::RequestRouteTelemetry;
use super::anthropic_tools::project_tools;
use super::map_response_stream;
use crate::agent::control::ProviderAccountSelection;
use crate::client_common::Prompt;
use crate::client_common::ResponseStream;
use crate::responses_metadata::CodexResponsesMetadata;
use crate::responses_metadata::CodexResponsesRequestKind;
use codex_api::AnthropicCachePolicy;
use codex_api::AnthropicClient;
use codex_api::AnthropicOperation;
use codex_api::AnthropicPrompt;
use codex_api::AnthropicRequest;
use codex_api::AnthropicRequestOptions;
use codex_api::AnthropicStreamOptions;
use codex_api::ApiError;
use codex_api::ReqwestTransport;
use codex_api::TransportError;
use codex_api::encode_anthropic_request;
use codex_config::AnthropicPromptCache;
use codex_http_client::ClientRouteClass;
use codex_login::anthropic::AccountId;
use codex_login::default_client::ClientRedirectPolicy;
use codex_login::default_client::create_client_for_route;
use codex_model_provider::AnthropicRequestAuth;
use codex_otel::SessionTelemetry;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_protocol::models::AnthropicSignedCompaction;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::TruncationPolicy;
use codex_rollout_trace::InferenceTraceContext;
use http::StatusCode;
use std::sync::Arc;

impl ModelClient {
    pub fn with_anthropic_prompt_cache(mut self, policy: AnthropicPromptCache) -> Self {
        self.anthropic_prompt_cache = policy;
        self
    }

    /// Binds an unshared native client to a captured conversation identity.
    /// The credential registry validates the saved account when a request captures its grant.
    pub fn with_anthropic_account(mut self, account: AccountId) -> Result<Self> {
        if !self.uses_claude_subscription() {
            return Err(CodexErr::InvalidRequest(
                "Anthropic account binding requires a native subscription provider".into(),
            ));
        }
        if Arc::get_mut(&mut self.state).is_none() {
            return Err(CodexErr::InvalidRequest(
                "Detached account binding requires an unshared model client".into(),
            ));
        }
        Ok(self.with_provider_account(Some(Arc::new(
            ProviderAccountSelection::ClaudeSubscription { id: account.into() },
        ))))
    }

    pub(super) async fn capture_anthropic_auth(&self) -> Result<AnthropicRequestAuth> {
        // Capture before any await. Refresh and dispatch retain this identity even
        // when an idle/active selection changes the next request's committed route.
        self.validate_captured_provider()?;
        let route = self.state.provider_account.load_full();
        let id = match route.as_ref().map(|route| &route.selection) {
            Some(ProviderAccountSelection::ClaudeSubscription { id, .. }) => id,
            Some(
                ProviderAccountSelection::Chatgpt { .. }
                | ProviderAccountSelection::OpenRouter { .. },
            )
            | None => {
                return Err(CodexErr::InvalidRequest(
                    "Select an Anthropic subscription account before sending a message".into(),
                ));
            }
        };
        self.state
            .provider
            .anthropic_auth_context()?
            .ok_or_else(|| {
                CodexErr::InvalidRequest("Native Anthropic authentication is unavailable".into())
            })?
            .capture(id)
            .await
    }

    fn build_anthropic_request(
        &self,
        prompt: &Prompt,
        model: &ModelInfo,
        effort: Option<&ReasoningEffort>,
        operation: AnthropicOperation,
    ) -> Result<AnthropicRequest> {
        let capabilities = model.anthropic.as_ref().ok_or_else(|| {
            CodexErr::InvalidRequest("Native Anthropic model capabilities are unavailable".into())
        })?;
        let tools = project_tools(&prompt.tools)
            .map_err(|error| self.state.provider.map_api_error(error))?;
        let mut input = prompt.get_formatted_input_for_request(model);
        self.prepare_response_items_for_request(&mut input);
        let effort = effort.or(model.default_reasoning_level.as_ref());
        let ttl = self.anthropic_cache_ttl(model, effort);
        let max_output_tokens = match operation {
            AnthropicOperation::Inference => capabilities.max_output_tokens,
            AnthropicOperation::Compact => capabilities
                .max_output_tokens
                .min(AnthropicSignedCompaction::MAX_OUTPUT_TOKENS),
        };
        encode_anthropic_request(
            &AnthropicPrompt {
                input: &input,
                instructions: &prompt.base_instructions.text,
                tools: &tools,
                parallel_tool_calls: prompt.parallel_tool_calls,
                output_schema: prompt.output_schema.as_ref(),
            },
            &AnthropicRequestOptions {
                model,
                reasoning_effort: effort,
                max_output_tokens,
                cache: AnthropicCachePolicy::Ephemeral(ttl),
                operation,
            },
        )
        .map_err(|error| self.state.provider.map_api_error(error))
    }

    pub(crate) fn estimate_model_request_tokens(
        &self,
        prompt: &Prompt,
        model: &ModelInfo,
        metadata: &CodexResponsesMetadata,
    ) -> Result<usize> {
        if self.uses_claude_subscription() {
            let request = self.build_anthropic_request(
                prompt,
                model,
                /*effort*/ None,
                AnthropicOperation::Inference,
            )?;
            return Ok(TruncationPolicy::Bytes(request.body().get().len()).token_budget());
        }
        let request = self.build_responses_request(
            prompt,
            model,
            /*effort*/ None,
            codex_protocol::config_types::ReasoningSummary::None,
            /*service_tier*/ None,
            metadata,
            /*include_internal*/ true,
        )?;
        Ok(crate::guardian::estimate_request_tokens(&request))
    }
}

impl ModelClientSession {
    #[tracing::instrument(name = "model_client.stream_anthropic", level = "info", skip_all,
        fields(model = %model.slug, transport = "anthropic_http", api.path = "/messages"))]
    pub(super) async fn stream_anthropic(
        &self,
        prompt: &Prompt,
        model: &ModelInfo,
        telemetry: &SessionTelemetry,
        effort: Option<ReasoningEffort>,
        metadata: &CodexResponsesMetadata,
        trace: &InferenceTraceContext,
    ) -> Result<ResponseStream> {
        let warm_account = self.client.provider_account_id();
        let mut captured = self.client.capture_anthropic_auth().await?;
        let operation = match metadata.request_kind {
            Some(CodexResponsesRequestKind::Compaction(compaction))
                if matches!(
                    compaction.implementation(),
                    codex_analytics::CompactionImplementation::AnthropicNative
                ) =>
            {
                AnthropicOperation::Compact
            }
            Some(
                CodexResponsesRequestKind::Compaction(_)
                | CodexResponsesRequestKind::Turn
                | CodexResponsesRequestKind::Prewarm
                | CodexResponsesRequestKind::Memory,
            )
            | None => AnthropicOperation::Inference,
        };
        let request =
            self.client
                .build_anthropic_request(prompt, model, effort.as_ref(), operation)?;
        let provider = self.client.state.provider.api_provider().await?;
        let mut client_metadata = None;
        let interceptors = crate::model_request::prepare(
            &self.client.request_contributors,
            &self.client.state.thread_id.to_string(),
            &model.slug,
            codex_extension_api::ModelRequestKind::Generation,
            &mut client_metadata,
        );
        let request_telemetry = telemetry
            .clone()
            .with_inference_request(/*service_tier*/ None, effort.as_ref());
        let mut recovered = false;
        loop {
            let auth = captured.auth();
            let (http_telemetry, sse_telemetry) = Self::build_streaming_telemetry(
                &request_telemetry,
                AuthRequestTelemetryContext::new(
                    /*auth_mode*/ None,
                    auth.as_ref(),
                    /*agent_identity_telemetry*/ None,
                    PendingUnauthorizedRetry::default(),
                )
                .map_err(|error| self.client.state.provider.map_api_error(error))?,
                RequestRouteTelemetry::for_endpoint("/messages"),
                self.client.state.auth_env_telemetry.clone(),
            );
            let transport = ReqwestTransport::from_http_client(
                create_client_for_route(
                    &self.client.http_client_factory,
                    &provider.url_for_path("/messages"),
                    ClientRouteClass::Api,
                    ClientRedirectPolicy::Reject,
                )
                .map_err(std::io::Error::from)?,
            );
            let dispatch = matches!(metadata.request_kind, Some(CodexResponsesRequestKind::Turn))
                .then(|| {
                    self.client
                        .prompt_cache_dispatch(model, effort.as_ref(), || {
                            self.client.warming_request(
                                &request,
                                transport.clone(),
                                provider.clone(),
                                warm_account.clone(),
                            )
                        })
                })
                .flatten();
            let client = AnthropicClient::new(transport, provider.clone(), auth)
                .with_telemetry(Some(http_telemetry), Some(sse_telemetry));
            let attempt = trace.start_attempt();
            let mut options = AnthropicStreamOptions::default();
            attempt.add_request_headers(&mut options.extra_headers);
            attempt.record_started_with_protocol(
                &request.body(),
                codex_rollout_trace::ModelRequestProtocol::Messages,
            );
            match client.stream_request(request.clone(), options).await {
                Ok(stream) => {
                    let stream = map_response_stream(
                        stream,
                        request_telemetry,
                        attempt,
                        &self.client.state,
                        interceptors,
                        /*last_response_tx*/ None,
                    );
                    return Ok(self.client.observe_prompt_cache_stream(stream, dispatch));
                }
                Err(
                    error @ (ApiError::Transport(TransportError::Http {
                        status: StatusCode::UNAUTHORIZED,
                        ..
                    })
                    | ApiError::Api {
                        status: StatusCode::UNAUTHORIZED,
                        ..
                    }),
                ) if !recovered => {
                    attempt.record_failed(&error, /*upstream_request_id*/ None, &[]);
                    captured = captured.refresh_after_rejection().await?;
                    recovered = true;
                }
                Err(error) => {
                    attempt.record_failed(&error, /*upstream_request_id*/ None, &[]);
                    return Err(self.client.state.provider.map_api_error(error));
                }
            }
        }
    }
}
