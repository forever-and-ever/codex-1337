use crate::agent::control::ProviderAccountSelection;
use crate::config::Config;
use codex_config::read_openrouter_config;
use codex_model_provider_info::ModelProviderAdapter;
use codex_models_manager::manager::RefreshStrategy;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use std::sync::Arc;

pub(crate) fn initial_account(
    adapter: ModelProviderAdapter,
    selected: Option<Arc<ProviderAccountSelection>>,
) -> Option<Arc<ProviderAccountSelection>> {
    match adapter {
        ModelProviderAdapter::ClaudeSubscription => selected.filter(|selection| {
            matches!(
                selection.as_ref(),
                ProviderAccountSelection::ClaudeSubscription { .. }
            )
        }),
        ModelProviderAdapter::OpenRouter => selected.filter(|selection| {
            matches!(
                selection.as_ref(),
                ProviderAccountSelection::OpenRouter { .. }
            )
        }),
        ModelProviderAdapter::OpenaiCompatible => None,
    }
}

pub(crate) async fn default_model(
    config: &Config,
    manager: &dyn codex_models_manager::manager::ModelsManager,
    allow_fallback: bool,
    refresh: RefreshStrategy,
) -> Result<String> {
    if config.model_provider.adapter == ModelProviderAdapter::OpenRouter {
        if let Some(model) = &config.model {
            return Ok(model.clone());
        }
        return read_openrouter_config(&config.codex_home)
            .await
            .map_err(|_| {
                CodexErr::InvalidRequest("Cannot read OpenRouter model preferences".into())
            })?
            .models
            .keys()
            .next()
            .map(ToString::to_string)
            .ok_or_else(|| {
                CodexErr::InvalidRequest(
                    "Add an OpenRouter model with /model, or name one with --model.".into(),
                )
            });
    }
    Ok(manager
        .get_default_model(
            &config.model,
            allow_fallback,
            refresh,
            config.http_client_factory(),
        )
        .await)
}

pub(crate) fn require_realtime(adapter: ModelProviderAdapter) -> Result<()> {
    match adapter {
        ModelProviderAdapter::OpenaiCompatible => Ok(()),
        ModelProviderAdapter::ClaudeSubscription => Err(CodexErr::InvalidRequest(
            "Realtime conversations are not supported by Claude subscriptions.".into(),
        )),
        // OpenRouter Responses does not expose OpenAI's realtime transport or model ids.
        ModelProviderAdapter::OpenRouter => Err(CodexErr::InvalidRequest(
            "Realtime conversations are not supported by OpenRouter.".into(),
        )),
    }
}
