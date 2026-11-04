//! Select discovery through the account retained by a conversation, without changing global login.

use super::CatalogRequestProcessor;
use super::config_load_error;
use super::invalid_request;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ModelListParams;
use codex_core::config::Config;
use codex_core::config::ConfigOverrides;
use codex_login::AuthManager;
use codex_model_provider_info::ModelProviderAdapter;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::ThreadId;
use std::sync::Arc;

pub(super) struct AccountCatalog {
    pub(super) config: Arc<Config>,
    pub(super) models_manager: SharedModelsManager,
    pub(super) auth_manager: Arc<AuthManager>,
}

impl CatalogRequestProcessor {
    pub(super) async fn account_catalog(
        &self,
        params: &ModelListParams,
    ) -> Result<AccountCatalog, JSONRPCErrorError> {
        // Policy edits take effect on every request, not just after an account transition.
        // Use the typed provider-validation loader before catalog access can refresh credentials.
        let config = Arc::new(
            self.config_manager
                .load_for_resume(
                    None,
                    None,
                    ConfigOverrides {
                        model_provider: params.model_provider.clone(),
                        ..Default::default()
                    },
                    None,
                )
                .await
                .map_err(|err| config_load_error(&err))?,
        );
        let thread = match params.thread_id.as_deref() {
            Some(id) => {
                let id = ThreadId::from_string(id)
                    .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
                self.thread_manager.get_thread(id).await.ok()
            }
            None => None,
        };
        let auth_manager = match &thread {
            Some(thread) => thread.auth_manager(),
            None => self.thread_manager.auth_manager(),
        };
        let models_manager = if let Some(manager) =
            super::super::turn_processor::openrouter::catalog_for_list(&config)?
        {
            manager
        } else if let Some(thread) = thread {
            thread
                .models_manager(&config)
                .await
                .map_err(invalid_request)?
        } else if config.model_provider.adapter == ModelProviderAdapter::OpenaiCompatible {
            let (cached_config, cached_manager) = self
                .model_catalog
                .current()
                .await
                .map_err(|err| config_load_error(&err))?;
            if config.model_provider_id == cached_config.model_provider_id
                && config.model_provider == cached_config.model_provider
                && config.model_catalog == cached_config.model_catalog
            {
                cached_manager
            } else {
                codex_core::build_models_manager(&config, Arc::clone(&auth_manager))
            }
        } else {
            codex_core::build_models_manager(&config, Arc::clone(&auth_manager))
        };
        Ok(AccountCatalog {
            config,
            models_manager,
            auth_manager,
        })
    }
}
