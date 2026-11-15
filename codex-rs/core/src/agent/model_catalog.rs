//! Resolves fresh-child models against configured providers without replaying parent history.

use crate::agent::child_config::model_supports_multi_agent_backend;
use crate::agent::control::ProviderAccountSelection;
use crate::codex_thread::CodexThread;
use crate::config::Config;
use crate::session::session::Session;
use crate::thread_manager::accounts::AccountInheritance;
use crate::thread_manager::accounts::account_auth_for_provider;
use crate::thread_manager::build_models_manager;
use crate::thread_manager::build_models_manager_with_anthropic_account;
use codex_http_client::HttpClientFactory;
use codex_model_provider_info::ModelProviderAdapter;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::protocol::MultiAgentVersion;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::task::AbortOnDropHandle;

const CATALOG_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 5);

#[derive(Default)]
pub(crate) struct AgentModelsManagers {
    cached: Mutex<HashMap<String, CachedAgentModelsManager>>,
    warming: Mutex<HashMap<String, AgentCatalogWarm>>,
}

struct AgentCatalogWarm {
    manager: SharedModelsManager,
    task: AbortOnDropHandle<()>,
}

impl AgentModelsManagers {
    async fn warm(
        &self,
        provider_id: &str,
        manager: &SharedModelsManager,
        factory: HttpClientFactory,
    ) {
        let mut warming = self.warming.lock().await;
        if let Some(current) = warming.get(provider_id)
            && Arc::ptr_eq(&current.manager, manager)
            && !current.task.is_finished()
        {
            return;
        }
        let provider_id = provider_id.to_owned();
        let task_manager = Arc::clone(manager);
        let task_provider = provider_id.clone();
        let task = AbortOnDropHandle::new(tokio::spawn(async move {
            if tokio::time::timeout(
                CATALOG_TIMEOUT,
                task_manager.list_models(RefreshStrategy::OnlineIfUncached, factory),
            )
            .await
            .is_err()
            {
                tracing::warn!(
                    provider_id = task_provider,
                    "subagent model catalog warm timed out"
                );
            }
        }));
        warming.insert(
            provider_id,
            AgentCatalogWarm {
                manager: Arc::clone(manager),
                task,
            },
        );
    }
}

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
        if account.is_none() && config.model_provider.adapter.speaks_anthropic_messages() {
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
        let mut managers = self.services.agent_models_managers.cached.lock().await;
        if let Some(cached) = managers.get(&config.model_provider_id)
            && cached.key == key
        {
            return Ok(cached.manager.clone());
        }
        let runtime = crate::runtime_auth::runtime_for_config(
            &self.services.runtime_auth,
            config,
            Arc::clone(&auth),
            &self.installation_id,
        );
        let manager = if config.model_provider.adapter.speaks_anthropic_messages() {
            let account = match key.account.as_ref() {
                Some(ProviderAccountSelection::ClaudeSubscription { id, .. }) => Some(id.clone()),
                Some(
                    ProviderAccountSelection::Chatgpt { .. }
                    | ProviderAccountSelection::OpenRouter { .. },
                ) => {
                    return Err(
                        "Configured Anthropic provider has a foreign account binding".into(),
                    );
                }
                None => None,
            };
            build_models_manager_with_anthropic_account(config, auth, account, runtime)
        } else {
            build_models_manager(config, auth, runtime)
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
        let openrouter_available =
            match codex_config::read_openrouter_config(&config.codex_home).await {
                Ok(preferences) => !preferences.key_files.is_empty(),
                Err(error) => {
                    tracing::warn!(%error, "cannot read OpenRouter subagent preferences");
                    false
                }
            };
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
                        || match provider.adapter {
                            ModelProviderAdapter::ClaudeSubscription => true,
                            ModelProviderAdapter::OpenRouter => openrouter_available,
                            ModelProviderAdapter::OpenaiCompatible => false,
                        }
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
            // Even an Offline list_models can wait behind a network refresh's semaphore.
            // Turns read only the resident snapshot; background warming loads disk or network.
            let models = if refresh == RefreshStrategy::Offline {
                manager.try_list_models().unwrap_or_default()
            } else {
                match tokio::time::timeout(CATALOG_TIMEOUT, manager.list_models(
                    refresh, provider_config.http_client_factory(),
                )).await {
                    Ok(models) => models,
                    Err(_) => manager.try_list_models().unwrap_or_default(),
                }
            };
            if models.is_empty() && refresh == RefreshStrategy::Offline {
                self.services.agent_models_managers.warm(
                    id, &manager, provider_config.http_client_factory(),
                ).await;
            }
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
    // Hidden alternatives remain routable and still make bare selectors ambiguous.
    let mut occurrences = HashMap::new();
    for model in catalogs.iter().flat_map(|catalog| &catalog.models) {
        if model_supports_multi_agent_backend(model, version) {
            *occurrences.entry(model.model.as_str()).or_insert(0) += 1;
        }
    }
    let occurrences = &occurrences;
    catalogs
        .iter()
        .flat_map(|catalog| {
            catalog.models.iter().filter_map(move |model| {
                if !model.show_in_picker || !model_supports_multi_agent_backend(model, version) {
                    return None;
                }
                let mut model = model.clone();
                if occurrences[model.model.as_str()] > 1
                    || catalog.provider_id == codex_model_provider_info::OPENROUTER_PROVIDER_ID
                {
                    model.model = format!("{}/{}", catalog.provider_id, model.model);
                }
                Some(model)
            })
        })
        .collect()
}

pub(crate) fn describe_agent_model(model: &ModelPreset, description: &str) -> String {
    let mut line = format!("- `{}`", model.model);
    if !description.is_empty() {
        line.push_str(": ");
        line.push_str(description);
    }
    if !model.supported_reasoning_efforts.is_empty() {
        line.push_str(" Reasoning efforts: ");
        for (index, preset) in model.supported_reasoning_efforts.iter().enumerate() {
            if index > 0 {
                line.push_str(", ");
            }
            line.push_str(preset.effort.as_str());
            if preset.effort == model.default_reasoning_effort {
                line.push_str(" (default)");
            }
        }
        line.push('.');
    }
    if !model.service_tiers.is_empty() {
        line.push_str(" Service tiers: ");
        for (index, tier) in model.service_tiers.iter().enumerate() {
            if index > 0 {
                line.push_str(", ");
            }
            line.push_str(tier.id.as_str());
        }
        line.push('.');
    }
    line
}

#[cfg(test)]
#[path = "model_catalog_tests.rs"]
mod tests;
