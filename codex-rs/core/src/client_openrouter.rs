use super::CurrentClientSetup;
use super::ModelClient;
use crate::agent::control::ProviderAccountSelection;
use crate::client_common::Prompt;
use codex_api::ApiError;
use codex_api::ResponsesApiRequest;
use codex_api::TransportError;
use codex_api::openrouter::OpenRouterRouting;
use codex_config::read_openrouter_config;
use codex_login::default_client::ClientRedirectPolicy;
use codex_model_provider::openrouter::credentials;
use codex_model_provider_info::ModelProviderAdapter;
use codex_protocol::EndpointFilter;
use codex_protocol::ModelSlug;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort;
use codex_tools::ToolSpec;
use http::StatusCode;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

impl ModelClient {
    pub(super) fn reasoning_effort_for_request(
        &self,
        model: &ModelInfo,
        effort: ReasoningEffort,
    ) -> ReasoningEffort {
        if self.uses_openrouter() {
            effort
        } else {
            model.resolve_reasoning_effort(effort)
        }
    }

    pub(super) fn service_tier_for_request(
        &self,
        model: &ModelInfo,
        tier: Option<String>,
    ) -> Option<String> {
        if self.uses_openrouter() {
            tier.filter(|tier| tier != SERVICE_TIER_DEFAULT_REQUEST_VALUE)
        } else {
            model.service_tier_for_request(tier)
        }
    }

    pub(super) fn validate_openrouter_tools(&self, prompt: &Prompt) -> Result<()> {
        if self.uses_openrouter()
            && prompt.tools.iter().any(|tool| match tool {
                ToolSpec::WebSearch { .. } | ToolSpec::ToolSearch { .. } => true,
                ToolSpec::Function(_) | ToolSpec::Namespace(_) | ToolSpec::Freeform(_) => false,
            })
        {
            return Err(CodexErr::InvalidRequest(
                "OpenAI hosted tools cannot be sent to OpenRouter".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn uses_openrouter(&self) -> bool {
        match self.state.provider.info().adapter {
            ModelProviderAdapter::OpenRouter => true,
            ModelProviderAdapter::OpenaiCompatible | ModelProviderAdapter::ClaudeSubscription => {
                false
            }
        }
    }

    fn openrouter_home(&self) -> Result<PathBuf> {
        self.state
            .provider
            .auth_manager()
            .map(|manager| manager.runtime_config().codex_home)
            .ok_or_else(|| {
                CodexErr::InvalidRequest("OpenRouter authentication runtime is unavailable".into())
            })
    }

    pub(super) async fn openrouter_setup(&self) -> Result<CurrentClientSetup> {
        let route = self.state.provider_account.load_full();
        let Some(route) = route else {
            return Err(CodexErr::InvalidRequest(
                "Choose an OpenRouter key with /account".into(),
            ));
        };
        let ProviderAccountSelection::OpenRouter { key } = &route.selection else {
            return Err(CodexErr::InvalidRequest(
                "Choose an OpenRouter key with /account".into(),
            ));
        };
        let bearer = credentials(&self.openrouter_home()?, key).await?;
        Ok(CurrentClientSetup {
            auth: None,
            auth_owner_generation: Some(route.generation),
            auth_revision: None,
            api_provider: self.state.provider.info().to_api_provider(None)?,
            redirect_policy: ClientRedirectPolicy::Reject,
            api_auth: Arc::new(bearer),
            agent_identity_telemetry: None,
        })
    }

    pub(super) async fn route_openrouter_request(
        &self,
        request: &mut ResponsesApiRequest,
    ) -> Result<()> {
        if !self.uses_openrouter() {
            return Ok(());
        }
        let slug = request
            .model
            .parse::<ModelSlug>()
            .map_err(CodexErr::InvalidRequest)?;
        request.openrouter = match slug {
            ModelSlug::Preset(_) => None,
            ModelSlug::Model(_) => {
                let preferences = read_openrouter_config(&self.openrouter_home()?)
                    .await
                    .map_err(|_| {
                        CodexErr::InvalidRequest("Cannot read OpenRouter model preferences".into())
                    })?;
                Some(OpenRouterRouting(
                    preferences
                        .models
                        .get(&slug)
                        .cloned()
                        .unwrap_or(EndpointFilter::All),
                ))
            }
        };
        // OpenRouter's Responses API is stateless; hosted tools and access programs
        // belong to OpenAI, regardless of configured or inherited model metadata.
        request.store = false;
        request.access_programs = None;
        Ok(())
    }

    pub(super) fn map_request_error(&self, mut error: ApiError, model: &str) -> CodexErr {
        if !self.uses_openrouter() {
            return self.state.provider.map_api_error(error);
        }
        let (status, message) = match &error {
            ApiError::Transport(TransportError::Http { status, body, .. }) => (
                *status,
                body.as_deref().and_then(remote_message).unwrap_or_default(),
            ),
            ApiError::Api { status, message } => (*status, message.clone()),
            ApiError::QuotaExceeded => (StatusCode::PAYMENT_REQUIRED, String::new()),
            _ => (StatusCode::OK, String::new()),
        };
        if status == StatusCode::PAYMENT_REQUIRED
            && let Some(ProviderAccountSelection::OpenRouter { key }) =
                self.provider_account_selection()
        {
            return CodexErrorDetails::OpenRouterKeyExhausted { key }.into();
        }
        if status == StatusCode::NOT_FOUND && message.contains("No allowed providers are available")
        {
            return CodexErr::InvalidRequest(format!(
                "No allowed endpoints are available for {model}; change its endpoints with /model → ctrl+p"
            ));
        }
        if status == StatusCode::BAD_REQUEST && message.contains("is not a valid model ID") {
            return CodexErr::InvalidRequest(format!("OpenRouter has no model {model}"));
        }
        match &mut error {
            ApiError::Transport(TransportError::Http {
                url, headers, body, ..
            }) => {
                *url = None;
                *headers = None;
                *body = None;
            }
            ApiError::Api { message, .. } => *message = "OpenRouter request failed".into(),
            _ => {}
        }
        self.state.provider.map_api_error(error)
    }
}

fn remote_message(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Envelope {
        error: RemoteError,
    }
    #[derive(Deserialize)]
    struct RemoteError {
        message: String,
    }
    serde_json::from_str::<Envelope>(body)
        .ok()
        .map(|envelope| envelope.error.message)
}
