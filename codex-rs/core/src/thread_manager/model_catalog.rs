//! Keep each catalog bound to its provider and auth owner while preserving host cache policy.

use crate::config::Config;
use crate::runtime_auth::RuntimeAuthSlot;
use crate::runtime_auth::runtime_for_config;
use codex_features::Feature;
use codex_features::RuntimeAuthMode;
use codex_login::AuthManager;
use codex_model_provider::ModelCatalogFactory;
use codex_model_provider::ProviderAuthContext;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::manager::ModelCatalogSettings;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::protocol::SessionSource;
use codex_runtime_agent_instance::RuntimeAuthManager;
use std::sync::Arc;

/// The startup catalog and the credential/runtime owner used to construct it.
pub struct StartupModels {
    pub(super) credentials: Arc<AuthManager>,
    pub(super) runtime_auth: RuntimeAuthSlot,
    pub(super) installation_id: String,
    pub(super) catalog: ThreadModelCatalog,
}

impl StartupModels {
    /// Share the startup catalog with host discovery and prefetch callers.
    pub fn models_manager(&self) -> SharedModelsManager {
        Arc::clone(&self.catalog.manager)
    }
}

/// Construct the catalog and runtime owner together using the host's catalog factory.
/// Use `ModelCatalogSettings` for standard provider catalogs with a disk, custom, or no cache.
/// Runtime registration remains lazy; this does not register an instance or generate a key.
pub fn build_startup_models(
    config: &Config,
    credentials: Arc<AuthManager>,
    installation_id: String,
    factory: Arc<dyn ModelCatalogFactory>,
) -> StartupModels {
    let runtime_auth = RuntimeAuthSlot::default();
    let runtime = runtime_for_config(
        &runtime_auth,
        config,
        Arc::clone(&credentials),
        &installation_id,
    );
    let catalog = ThreadModelCatalog::new(
        config.model_provider_id.clone(),
        config.model_provider.clone(),
        config.wrangler.default_claude_account.clone(),
        config.model_catalog.clone(),
        factory,
        discovery_enabled(config),
        ProviderAuthContext {
            credentials: Arc::clone(&credentials),
            runtime_auth: runtime.map(|runtime| (runtime, config.runtime_auth_mode)),
        },
    );
    StartupModels {
        credentials,
        runtime_auth,
        installation_id,
        catalog,
    }
}

struct CapturedCatalog {
    authority: crate::client::CapturedProvider,
    provider_id: String,
    catalog: Option<ModelsResponse>,
    cache_path: std::path::PathBuf,
    discovery_enabled: bool,
    originator: String,
    manager: SharedModelsManager,
}

pub(crate) struct ThreadModelCatalog {
    pub(super) manager: SharedModelsManager,
    provider_id: String,
    provider: ModelProviderInfo,
    anthropic_account: Option<String>,
    catalog: Option<ModelsResponse>,
    factory: Arc<dyn ModelCatalogFactory>,
    api_key_model_discovery_default: bool,
    runtime_mode: Option<RuntimeAuthMode>,
    captured: std::sync::Mutex<Option<CapturedCatalog>>,
}

pub fn build_models_manager(
    config: &Config,
    credentials: Arc<AuthManager>,
    runtime: Option<Arc<RuntimeAuthManager>>,
) -> SharedModelsManager {
    build_models_manager_with_anthropic_account(
        config,
        credentials,
        config.wrangler.default_claude_account.clone(),
        runtime,
    )
}

pub fn build_models_manager_with_anthropic_account(
    config: &Config,
    credentials: Arc<AuthManager>,
    discovery_account: Option<String>,
    runtime: Option<Arc<RuntimeAuthManager>>,
) -> SharedModelsManager {
    let auth = ProviderAuthContext {
        credentials,
        runtime_auth: runtime
            .filter(|_| config.features.enabled(Feature::RuntimeAgentInstances))
            .map(|runtime| (runtime, config.runtime_auth_mode)),
    };
    ModelCatalogSettings::disk_file(config.models_cache_path()).build(
        catalog_provider(config.model_provider.clone(), discovery_account, auth),
        &config.model_provider_id,
        config.model_catalog.clone(),
        discovery_enabled(config),
    )
}

fn discovery_enabled(config: &Config) -> bool {
    config.features.enabled(Feature::ApiKeyModelDiscovery)
        || config.model_provider.adapter.speaks_anthropic_messages()
}

pub(super) fn catalog_provider(
    info: ModelProviderInfo,
    account: Option<String>,
    auth: ProviderAuthContext,
) -> codex_model_provider::SharedModelProvider {
    if info.adapter.speaks_anthropic_messages() {
        // Native credentials are account-owned and never use OpenAI runtime auth.
        codex_model_provider::create_model_provider_with_anthropic_account(
            info,
            Some(auth.credentials),
            account,
        )
    } else {
        auth.model_provider(info)
    }
}

impl ThreadModelCatalog {
    pub(super) fn new(
        provider_id: String,
        provider: ModelProviderInfo,
        anthropic_account: Option<String>,
        catalog: Option<ModelsResponse>,
        factory: Arc<dyn ModelCatalogFactory>,
        api_key_model_discovery_default: bool,
        auth: ProviderAuthContext,
    ) -> Self {
        let runtime_mode = auth.runtime_auth.as_ref().map(|(_, mode)| *mode);
        let manager = factory.build(
            catalog_provider(provider.clone(), anthropic_account.clone(), auth),
            &provider_id,
            catalog.clone(),
            api_key_model_discovery_default,
        );
        Self {
            manager,
            provider_id,
            provider,
            anthropic_account,
            catalog,
            factory,
            api_key_model_discovery_default,
            runtime_mode,
            captured: std::sync::Mutex::new(None),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn for_thread(
        &self,
        config: &Config,
        credentials: Arc<AuthManager>,
        runtime: Option<Arc<RuntimeAuthManager>>,
        same_owner: bool,
        originator: &str,
        session_source: &SessionSource,
    ) -> SharedModelsManager {
        let models = config.model_catalog.as_ref().or_else(|| {
            self.catalog
                .as_ref()
                .filter(|_| self.matches_catalog_provider(config))
        });
        let auth = ProviderAuthContext {
            credentials,
            runtime_auth: runtime.map(|runtime| (runtime, config.runtime_auth_mode)),
        };
        let manager = self.for_config(config, models, auth, same_owner);
        let manager = manager.for_originator(originator).unwrap_or(manager);
        if session_source.is_non_root_agent() {
            // The final provider/auth/originator catalog may be new even when the base
            // manager was reused. Preserve loaded snapshots, including unsaved updates.
            manager
                .ensure_initialized(config.http_client_factory())
                .await;
        }
        manager
    }

    pub(crate) fn for_captured(
        &self,
        config: &Config,
        captured: &crate::client::CapturedProvider,
        originator: &str,
    ) -> Result<SharedModelsManager, crate::client::ProviderCaptureError> {
        captured.validate()?;
        let provider = captured.catalog_provider(config)?;
        let models = config.model_catalog.as_ref().or_else(|| {
            self.catalog
                .as_ref()
                .filter(|_| self.matches_catalog_provider(config))
        });
        let discovery = discovery_enabled(config);
        let cache_path = config.models_cache_path();
        let mut cache = self
            .captured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = cache.as_ref()
            && entry.provider_id == config.model_provider_id
            && entry.catalog.as_ref() == models
            && entry.cache_path == cache_path
            && entry.discovery_enabled == discovery
            && entry.originator == originator
            && entry.authority.same_authority(captured)
        {
            return Ok(Arc::clone(&entry.manager));
        }
        // Never borrow the startup catalog's mutable selector for an admitted issuer.
        let manager = self.factory.build(
            provider,
            &config.model_provider_id,
            models.cloned(),
            discovery,
        );
        let manager = manager.for_originator(originator).unwrap_or(manager);
        *cache = Some(CapturedCatalog {
            authority: captured.clone(),
            provider_id: config.model_provider_id.clone(),
            catalog: models.cloned(),
            cache_path,
            discovery_enabled: discovery,
            originator: originator.to_owned(),
            manager: Arc::clone(&manager),
        });
        Ok(manager)
    }

    pub(super) fn for_config(
        &self,
        config: &Config,
        models: Option<&ModelsResponse>,
        auth: ProviderAuthContext,
        same_owner: bool,
    ) -> SharedModelsManager {
        let api_key_model_discovery_default = discovery_enabled(config);
        if same_owner
            && self.runtime_mode == auth.runtime_auth.as_ref().map(|(_, mode)| *mode)
            && self.matches_catalog_provider(config)
            && self.catalog.as_ref() == models
            && self.api_key_model_discovery_default == api_key_model_discovery_default
        {
            return Arc::clone(&self.manager);
        }
        self.factory.build(
            catalog_provider(
                config.model_provider.clone(),
                config.wrangler.default_claude_account.clone(),
                auth,
            ),
            &config.model_provider_id,
            models.cloned(),
            api_key_model_discovery_default,
        )
    }

    fn matches_catalog_provider(&self, config: &Config) -> bool {
        let provider = &config.model_provider;
        // Guardian overrides inference transport and retries without changing the catalog.
        // Keep all other provider fields, including routing and headers, in the comparison.
        self.provider_id == config.model_provider_id
            && (!provider.adapter.speaks_anthropic_messages()
                || self.anthropic_account == config.wrangler.default_claude_account)
            && self.provider
                == ModelProviderInfo {
                    request_max_retries: self.provider.request_max_retries,
                    stream_max_retries: self.provider.stream_max_retries,
                    supports_websockets: self.provider.supports_websockets,
                    ..provider.clone()
                }
    }

    pub(super) fn set_api_key_model_discovery_enabled(&self, enabled: bool) {
        self.factory.set_api_key_model_discovery_enabled(enabled);
    }
}
