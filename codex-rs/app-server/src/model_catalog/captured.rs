//! Reuse global discovery only while admitted authority and committed config survive.

use super::Arc;
use super::Config;
use super::Feature;
use super::ModelCatalog;
use super::SharedModelsManager;

pub(super) struct AdmittedCatalog {
    config: Arc<Config>,
    source: Arc<codex_login::CapturedAuthSource>,
    models_manager: SharedModelsManager,
    provider: codex_model_provider::SharedModelProvider,
}

impl ModelCatalog {
    /// Reload effective discovery routing without replacing unchanged captured catalog state.
    pub(crate) async fn global_config(&self) -> std::io::Result<Arc<Config>> {
        let latest = self
            .config_manager
            .load_latest_config(self.startup_cwd.clone())
            .await?;
        let (config, _) = self.current().await?;
        let previous = config.config_layer_stack.requirements_toml();
        let current = latest.config_layer_stack.requirements_toml();
        if latest.model_provider_id != config.model_provider_id
            || latest.model_providers != config.model_providers
            || latest.model_catalog != config.model_catalog
            || latest.runtime_auth_mode != config.runtime_auth_mode
            || latest.features.enabled(Feature::RuntimeAgentInstances)
                != config.features.enabled(Feature::RuntimeAgentInstances)
            || latest.features.enabled(Feature::ApiKeyModelDiscovery)
                != config.features.enabled(Feature::ApiKeyModelDiscovery)
            || previous.model_provider != current.model_provider
            || previous.model_providers != current.model_providers
        {
            self.invalidate().await;
            return self.current().await.map(|(config, _)| config);
        }
        Ok(config)
    }

    /// Reuses discovery only while both committed configuration and admitted authority survive.
    pub(crate) async fn admitted(
        &self,
        config: Arc<Config>,
        source: Arc<codex_login::CapturedAuthSource>,
    ) -> std::io::Result<(
        SharedModelsManager,
        codex_model_provider::SharedModelProvider,
    )> {
        source.validate()?;
        let mut admitted = self.admitted.lock().await;
        if let Some(cached) = admitted.as_ref()
            && Arc::ptr_eq(&cached.config, &config)
            && cached.source.same_authority(&source)
        {
            return Ok((
                Arc::clone(&cached.models_manager),
                Arc::clone(&cached.provider),
            ));
        }
        let credentials = source.credentials();
        let runtime = self
            .thread_manager
            .runtime_auth_for_config(&config)?
            .map(|(runtime, mode)| (runtime.for_credentials(Arc::clone(&credentials)), mode));
        let provider = codex_model_provider::ProviderAuthContext {
            credentials: Arc::clone(&credentials),
            runtime_auth: runtime.clone(),
        }
        .model_provider(config.model_provider.clone());
        let models_manager = codex_core::build_models_manager(
            &config,
            credentials,
            runtime.map(|(manager, _)| manager),
        );
        source.validate()?;
        *admitted = Some(AdmittedCatalog {
            config,
            source,
            models_manager: Arc::clone(&models_manager),
            provider: Arc::clone(&provider),
        });
        Ok((models_manager, provider))
    }
}
