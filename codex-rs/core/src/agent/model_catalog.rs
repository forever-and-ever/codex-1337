//! Resolves fresh-child models against configured providers without replaying parent history.

use crate::agent::child_config::MAX_SPAWN_AGENT_MODEL_OVERRIDES;
use crate::agent::child_config::model_supports_multi_agent_backend;
use crate::agent::control::ProviderAccountSelection;
use crate::codex_thread::CodexThread;
use crate::config::Config;
use crate::session::session::Session;
use crate::thread_manager::accounts::AccountInheritance;
use crate::thread_manager::accounts::account_auth_for_provider;
use crate::thread_manager::build_models_manager;
use crate::thread_manager::build_models_manager_with_anthropic_account;
use codex_model_provider_info::ModelProviderAdapter;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::protocol::MultiAgentVersion;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::Mutex;

const CATALOG_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 5);

#[derive(Default)]
pub(crate) struct AgentModelsManagers(Mutex<HashMap<String, CachedAgentModelsManager>>);

#[derive(PartialEq)]
struct AgentModelsKey {
    provider: ModelProviderInfo,
    catalog: Option<ModelsResponse>,
    api_key_discovery: bool,
    account: Option<ProviderAccountSelection>,
    auth_account: Option<String>,
}

struct CachedAgentModelsManager {
    key: AgentModelsKey,
    manager: SharedModelsManager,
}

pub(crate) struct ProviderModels {
    pub(crate) provider_id: String,
    pub(crate) manager: SharedModelsManager,
    pub(crate) models: Vec<ModelPreset>,
}

impl Session {
    pub(crate) async fn agent_models_manager(
        &self,
        config: &Config,
    ) -> Result<SharedModelsManager, String> {
        if config.model_provider_id == self.provider_id {
            return self
                .request_models_manager(config)
                .await
                .map_err(|error| error.to_string());
        }
        let auth = account_auth_for_provider(
            config,
            &self.services.local_agent_runtime.account_selection,
            &self.services.auth_manager,
            /*persisted*/ None,
            // This branch is exclusively cross-provider: there is no parent binding to inherit.
            &AccountInheritance::OtherProvider,
        )
        .await
        .map_err(|error| error.to_string())?
        .unwrap_or_else(|| self.services.auth_manager.clone());
        let mut account = self
            .services
            .local_agent_runtime
            .account_selection
            .get(&config.model_provider_id)
            .map(|selection| selection.as_ref().clone());
        if account.is_none()
            && config.model_provider.adapter == ModelProviderAdapter::ClaudeSubscription
        {
            account = codex_config::read_account_preferences(config.codex_home.as_path())
                .await
                .map_err(|error| error.to_string())?
                .unwrap_or_else(|| config.wrangler.clone())
                .default_claude_account
                .map(|id| ProviderAccountSelection::ClaudeSubscription { id });
        }
        let key = AgentModelsKey {
            provider: config.model_provider.clone(),
            catalog: config.model_catalog.clone(),
            api_key_discovery: config
                .features
                .enabled(codex_features::Feature::ApiKeyModelDiscovery),
            account: (!config.model_provider.requires_openai_auth)
                .then_some(account)
                .flatten(),
            auth_account: config
                .model_provider
                .requires_openai_auth
                .then(|| auth.selected_account_id())
                .flatten(),
        };
        let mut managers = self.services.agent_models_managers.0.lock().await;
        if let Some(cached) = managers.get(&config.model_provider_id)
            && cached.key == key
        {
            return Ok(cached.manager.clone());
        }
        let manager = if config.model_provider.adapter == ModelProviderAdapter::ClaudeSubscription {
            let account = match key.account.as_ref() {
                Some(ProviderAccountSelection::ClaudeSubscription { id, .. }) => Some(id.clone()),
                Some(ProviderAccountSelection::Chatgpt { .. }) => {
                    return Err(
                        "Configured Anthropic provider has a foreign account binding".into(),
                    );
                }
                None => None,
            };
            build_models_manager_with_anthropic_account(config, auth, account)
        } else {
            build_models_manager(config, auth)
        };
        managers.insert(
            config.model_provider_id.clone(),
            CachedAgentModelsManager {
                key,
                manager: manager.clone(),
            },
        );
        Ok(manager)
    }

    pub(crate) async fn agent_model_catalogs(
        &self,
        config: &Config,
        refresh: RefreshStrategy,
    ) -> Vec<ProviderModels> {
        let required_provider = config.config_layer_stack.required_model_provider();
        let mut providers = std::iter::once((&config.model_provider_id, &config.model_provider))
            .chain(
                config
                    .model_providers
                    .iter()
                    .filter(|(id, _)| *id != &config.model_provider_id),
            )
            .filter(|(id, provider)| {
                required_provider.is_none_or(|required| required == id.as_str())
                    && (id.as_str() == config.model_provider_id
                        || provider.requires_openai_auth
                        || provider.adapter == ModelProviderAdapter::ClaudeSubscription
                        || provider.model_catalog_url.is_some())
            })
            .collect::<Vec<_>>();
        providers.sort_by_key(|(id, _)| (id.as_str() != config.model_provider_id, id.as_str()));
        let catalogs = futures::future::join_all(providers.into_iter().map(|(id, provider)| async move {
            let mut provider_config = config.clone();
            provider_config.model_provider_id = id.clone();
            provider_config.model_provider = provider.clone();
            // Explicit catalog overrides belong to the parent's provider, not every backend.
            if id != &config.model_provider_id {
                provider_config.model_catalog = None;
            }
            let manager = match self.agent_models_manager(&provider_config).await {
                Ok(manager) => manager,
                Err(error) => {
                    tracing::warn!(provider_id = id, %error, "cannot load subagent model catalog");
                    return None;
                }
            };
            // Ordinary turns stay offline. Explicit selection also refreshes an uncached
            // primary provider: a resumed Claude thread may have no warm catalog yet.
            let models = match tokio::time::timeout(CATALOG_TIMEOUT, manager.list_models(
                refresh, provider_config.http_client_factory(),
            )).await {
                Ok(models) => models,
                Err(_) => manager.try_list_models().unwrap_or_default(),
            };
            Some(ProviderModels {
                provider_id: id.clone(),
                manager,
                models,
            })
        })).await;
        catalogs.into_iter().flatten().collect()
    }
}

impl CodexThread {
    /// The catalog this conversation reads for `config`'s provider: the one its turns use for
    /// its own provider, and the one its agents would use for another.
    pub async fn models_manager(&self, config: &Config) -> Result<SharedModelsManager, String> {
        self.session.agent_models_manager(config).await
    }
}

pub(crate) fn advertised_agent_models(
    catalogs: &[ProviderModels],
    version: MultiAgentVersion,
) -> Vec<ModelPreset> {
    // Count the full selectable catalog, not just the abbreviated advertised list:
    // a hidden or omitted alternative still makes a bare selector ambiguous.
    let mut occurrences = HashMap::new();
    for model in catalogs.iter().flat_map(|catalog| &catalog.models) {
        if model_supports_multi_agent_backend(model, version) {
            *occurrences.entry(model.model.as_str()).or_insert(0) += 1;
        }
    }
    let occurrences = &occurrences;
    let mut providers = catalogs
        .iter()
        .map(|catalog| {
            catalog
                .models
                .iter()
                .filter(move |model| {
                    model.show_in_picker && model_supports_multi_agent_backend(model, version)
                })
                .map(move |model| {
                    let mut model = model.clone();
                    if occurrences[model.model.as_str()] > 1 {
                        model.model = format!("{}/{}", catalog.provider_id, model.model);
                    }
                    model
                })
        })
        .collect::<Vec<_>>();
    // Round-robin keeps a provider's catalog from hiding every other backend.
    let mut models = Vec::new();
    while models.len() < MAX_SPAWN_AGENT_MODEL_OVERRIDES {
        let before = models.len();
        for provider in &mut providers {
            if let Some(model) = provider.next() {
                models.push(model);
                if models.len() == MAX_SPAWN_AGENT_MODEL_OVERRIDES {
                    break;
                }
            }
        }
        if models.len() == before {
            break;
        }
    }
    models
}

#[cfg(test)]
#[path = "model_catalog_tests.rs"]
mod tests;
