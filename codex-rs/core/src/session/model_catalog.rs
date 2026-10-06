use crate::agent::control::ProviderAccountSelection;
use crate::config::Config;
use crate::session::session::Session;
use codex_model_provider_info::ModelProviderAdapter;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;

impl Session {
    /// Discovery captures the committed conversation identity, not the UI default.
    /// Metadata-only consumers can keep the session's authoritative startup catalog.
    pub(crate) async fn request_models_manager(
        &self,
        config: &Config,
    ) -> Result<SharedModelsManager> {
        if config.model_provider.adapter != ModelProviderAdapter::ClaudeSubscription {
            return Ok(self.services.models_manager.clone());
        }
        let account = match self.services.model_client.provider_account_selection() {
            Some(ProviderAccountSelection::ClaudeSubscription { id, .. }) => id,
            Some(ProviderAccountSelection::Chatgpt { .. }) | None => {
                return Err(CodexErr::InvalidRequest(
                    "Select an Anthropic account before discovering models".into(),
                ));
            }
        };
        let provider = self
            .state
            .lock()
            .await
            .session_configuration
            .provider
            .clone();
        let provider = provider.for_anthropic_account(&account)?;
        let manager =
            provider.models_manager(config.models_cache_path(), config.model_catalog.clone());
        let catalog = manager
            .raw_model_catalog(
                RefreshStrategy::OnlineIfUncached,
                config.http_client_factory(),
            )
            .await;
        if !catalog.models.iter().any(|model| model.anthropic.is_some()) {
            return Err(CodexErr::InvalidRequest(
                "Cannot load authoritative Anthropic model metadata for this account".into(),
            ));
        }
        Ok(manager)
    }
}
