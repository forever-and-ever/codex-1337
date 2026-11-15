//! Select discovery through the account retained by a conversation, without changing global login.

use super::CatalogRequestProcessor;
use super::config_load_error;
use super::invalid_request;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ModelListParams;
use codex_core::config::Config;
use codex_core::config::ConfigOverrides;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::ThreadId;
use std::sync::Arc;

pub(super) struct AccountCatalog {
    pub(super) config: Arc<Config>,
    pub(super) models_manager: SharedModelsManager,
    pub(super) provider: codex_model_provider::SharedModelProvider,
}

impl CatalogRequestProcessor {
    pub(super) async fn account_catalog(
        &self,
        params: &ModelListParams,
    ) -> Result<AccountCatalog, JSONRPCErrorError> {
        let thread = match params.thread_id.as_deref() {
            Some(id) => {
                let id = ThreadId::from_string(id)
                    .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
                Some(self.thread_manager.get_thread(id).await.map_err(|error| {
                    invalid_request(format!("Conversation catalog is unavailable: {error}"))
                })?)
            }
            None => None,
        };
        let config = match (params.model_provider.as_ref(), thread.as_ref()) {
            (None, Some(thread)) => thread.config().await,
            (None, None) => Box::pin(self.model_catalog.global_config())
                .await
                .map_err(|error| config_load_error(&error))?,
            (provider, _) => Arc::new(
                self.config_manager
                    .load_for_resume(
                        None,
                        None,
                        ConfigOverrides {
                            model_provider: provider.cloned(),
                            ..Default::default()
                        },
                        None,
                    )
                    .await
                    .map_err(|error| config_load_error(&error))?,
            ),
        };
        let (models_manager, provider) = if let Some(thread) = thread {
            let capture = thread
                .capture_provider()
                .await
                .map_err(|error| invalid_request(error.to_string()))?;
            let models_manager = capture
                .models_manager(&config)
                .map_err(|error| invalid_request(error.to_string()))?;
            (models_manager, Arc::clone(capture.provider()))
        } else if params.model_provider.is_none()
            && config.model_provider.adapter
                != codex_model_provider_info::ModelProviderAdapter::OpenRouter
        {
            let source = self
                .thread_manager
                .auth_manager()
                .capture_auxiliary_source()
                .map_err(|error| invalid_request(error.to_string()))?;
            self.model_catalog
                .admitted(Arc::clone(&config), Arc::new(source))
                .await
                .map_err(|error| invalid_request(error.to_string()))?
        } else {
            let source = self
                .thread_manager
                .auth_manager()
                .capture_auxiliary_source()
                .map_err(|error| invalid_request(error.to_string()))?;
            source
                .validate()
                .map_err(|error| invalid_request(error.to_string()))?;
            let credentials = source.credentials();
            let runtime = self
                .thread_manager
                .runtime_auth_for_config(&config)
                .map_err(|error| invalid_request(error.to_string()))?
                .map(|(runtime, mode)| (runtime.for_credentials(Arc::clone(&credentials)), mode));
            let provider = codex_model_provider::ProviderAuthContext {
                credentials: Arc::clone(&credentials),
                runtime_auth: runtime.clone(),
            }
            .model_provider(config.model_provider.clone());
            let models_manager =
                match super::super::turn_processor::openrouter::catalog_for_list(&config)? {
                    Some(manager) => manager,
                    None => codex_core::build_models_manager(
                        &config,
                        credentials,
                        runtime.map(|(manager, _)| manager),
                    ),
                };
            (models_manager, provider)
        };
        Ok(AccountCatalog {
            config,
            models_manager,
            provider,
        })
    }
}
