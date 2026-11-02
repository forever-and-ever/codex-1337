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
    fn authoritative_catalog(
        &self,
        strategy: RefreshStrategy,
        factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, Result<ModelsResponse>>;
}

impl AuthoritativeCatalog for OpenAiModelsManager {
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
