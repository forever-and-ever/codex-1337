use super::LiveModelCatalog;
use super::ModelsManager;
use super::ModelsManagerFuture;
use super::OpenAiModelsManager;
use super::RefreshStrategy;
use super::StaticModelsManager;
use codex_http_client::HttpClientFactory;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result;
use codex_protocol::openai_models::ModelsResponse;

/// TELOS(recoverable-sign-in): authoritative consumers retain recoverable auth failures.
pub trait AuthoritativeCatalog: Send + Sync {
    fn live_catalog(&self) -> ModelsManagerFuture<'_, Option<LiveModelCatalog>> {
        Box::pin(async { None })
    }

    fn adopt_live_catalog(&self, _publication: LiveModelCatalog) -> ModelsManagerFuture<'_, ()> {
        Box::pin(async {})
    }

    fn authoritative_catalog(
        &self,
        strategy: RefreshStrategy,
        factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, Result<ModelsResponse>>;
}

impl AuthoritativeCatalog for OpenAiModelsManager {
    fn live_catalog(&self) -> ModelsManagerFuture<'_, Option<LiveModelCatalog>> {
        self.snapshot_live_catalog()
    }

    fn adopt_live_catalog(&self, publication: LiveModelCatalog) -> ModelsManagerFuture<'_, ()> {
        self.apply_live_catalog(publication)
    }

    fn authoritative_catalog(
        &self,
        strategy: RefreshStrategy,
        factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, Result<ModelsResponse>> {
        Box::pin(async move {
            if let Err(error) = self.refresh_available_models(strategy, &factory).await {
                if matches!(error.details(), CodexErrorDetails::SignInRequired(_)) {
                    return Err(error);
                }
                tracing::error!("failed to refresh available models: {error}");
            }
            Ok(ModelsResponse {
                models: self.get_remote_models().await,
            })
        })
    }
}

impl AuthoritativeCatalog for StaticModelsManager {
    fn authoritative_catalog(
        &self,
        strategy: RefreshStrategy,
        factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, Result<ModelsResponse>> {
        Box::pin(async move { Ok(self.raw_model_catalog(strategy, factory).await) })
    }
}
