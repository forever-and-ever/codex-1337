use codex_api::ResponseStream as ApiResponseStream;
use tokio::sync::mpsc;

#[path = "client_openrouter_tools.rs"]
pub(super) mod tools;

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
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
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

    pub(super) fn openrouter_home(&self) -> Result<PathBuf> {
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
            auth_owner: None,
            auth_revision: Some(route.generation),
            api_provider: self.state.provider.info().to_api_provider(None)?,
            redirect_policy: ClientRedirectPolicy::Reject,
            api_auth: Arc::new(bearer),
            agent_identity_telemetry: None,
            http_client_factory: self.http_client_factory.clone(),
            runtime_id: None,
        })
    }

    pub(super) async fn route_openrouter_request(
        &self,
        request: &mut ResponsesApiRequest,
        prompt: &Prompt,
    ) -> Result<Option<tools::Projection>> {
        if !self.uses_openrouter() {
            return Ok(None);
        }
        let slug = request
            .model
            .parse::<ModelSlug>()
            .map_err(CodexErr::InvalidRequest)?;
        if slug.as_str().ends_with(":batch") {
            return Err(CodexErr::InvalidRequest(
                "Select the ordinary model and enable /slow for batch inference.".into(),
            ));
        }
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
        let projection = tools::Projection::request(request, &prompt.tools)?;
        // Google and other routed hosts reject assistant-prefilled requests.
        // A failed stream can leave completed model items but no completed response.
        // Keep their original roles and tool receipts; the continuation notice belongs
        // only to this wire copy, never to durable history or a new human turn.
        if request.input.last().is_some_and(|item| match item {
            ResponseItem::Reasoning { .. } => true,
            ResponseItem::Message { role, .. } => role == "assistant",
            _ => false,
        }) {
            let mut text = concat!(
                "<harness_continuation>\n",
                "This is an automated harness continuation, not new user input. ",
                "The prior response did not finish the turn. Continue the existing task ",
                "from the assistant context above. Do not repeat completed tool actions ",
                "merely because the response was interrupted; their results are already ",
                "in the conversation.\n",
            )
            .to_owned();
            // Reattempting a large Google tool call can repeat the upstream idle
            // timeout. Ask for one small step on recovery, never on normal requests.
            if request.model.starts_with("google/") {
                text.push_str(concat!(
                    "For this recovery, take one small, useful step rather than generating a large tool call. ",
                    "Keep the next tool input under 2000 characters; split large edits into smaller complete ",
                    "steps and continue after receiving their results. This is recovery guidance, not a ",
                    "change to the task or its completion criteria.\n",
                ));
            }
            text.push_str("</harness_continuation>");
            request.input.push(ResponseItem::Message {
                id: None,
                role: "user".into(),
                content: vec![ContentItem::InputText { text }],
                encrypted_content: None,
                status: None,
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            });
        }
        Ok(Some(projection))
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
        if terminal_status(status) {
            return CodexErr::InvalidRequest(if message.is_empty() {
                format!("OpenRouter rejected the request ({status})")
            } else {
                message.chars().take(300).collect()
            });
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
        metadata: Option<Metadata>,
    }
    #[derive(Deserialize)]
    struct Metadata {
        #[serde(rename = "provider_name")]
        host_name: Option<String>,
        raw: Option<String>,
    }
    let error = serde_json::from_str::<Envelope>(body).ok()?.error;
    let host_message = error.metadata.and_then(|metadata| {
        let host = metadata.host_name?;
        let raw = serde_json::from_str::<Envelope>(&metadata.raw?).ok()?;
        Some(format!(
            "OpenRouter host {host} rejected the request: {}",
            raw.error.message
        ))
    });
    Some(
        host_message
            .unwrap_or(error.message)
            .chars()
            .take(300)
            .collect(),
    )
}

fn terminal_status(status: StatusCode) -> bool {
    status.is_client_error()
        && !matches!(
            status,
            StatusCode::PAYMENT_REQUIRED
                | StatusCode::REQUEST_TIMEOUT
                | StatusCode::TOO_MANY_REQUESTS
        )
}

pub(super) fn project_stream(
    mut stream: ApiResponseStream,
    projection: Option<tools::Projection>,
    settled: bool,
) -> ApiResponseStream {
    let Some(mut projection) = projection else {
        return stream;
    };
    let (tx, rx_event) = mpsc::channel(super::RESPONSE_STREAM_CHANNEL_CAPACITY);
    let routing_affinity = stream.routing_affinity.take();
    let upstream_request_id = stream.upstream_request_id.take();
    let interrupt = stream.interrupt.take();
    tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                () = tx.closed() => return,
                event = stream.rx_event.recv() => event,
            };
            let Some(event) = event else {
                return;
            };
            let events = match event {
                Ok(event) if settled => vec![Ok(projection.settled(event))],
                Ok(event) => projection.event(event).into_iter().map(Ok).collect(),
                Err(error) => {
                    let message = match &error {
                        ApiError::Transport(TransportError::Http { status, body, .. })
                            if terminal_status(*status) =>
                        {
                            Some(body.as_deref().and_then(remote_message).unwrap_or_else(|| {
                                format!("OpenRouter rejected the request ({status})")
                            }))
                        }
                        ApiError::Api { status, message } if terminal_status(*status) => {
                            Some(message.chars().take(300).collect())
                        }
                        _ => None,
                    };
                    vec![Err(match message {
                        Some(message) => ApiError::InvalidRequest { message },
                        None => error,
                    })]
                }
            };
            for event in events {
                if tx.send(event).await.is_err() {
                    return;
                }
            }
        }
    });
    ApiResponseStream {
        routing_affinity,
        rx_event,
        upstream_request_id,
        interrupt,
    }
}
