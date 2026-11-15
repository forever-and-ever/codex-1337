//! Build account API clients from the latest config and captured account authority.

use super::*;
use futures::future::BoxFuture;

impl AccountRequestProcessor {
    pub(super) fn captured_backend_credentials(
        credentials: &Arc<AuthManager>,
    ) -> Result<Arc<AuthManager>, JSONRPCErrorError> {
        credentials
            .capture_auxiliary_source()
            .map(|source| source.credentials())
            .map_err(|error| internal_error(error.to_string()))
    }

    // Keep config loading and runtime bootstrap out of request-handler future types.
    pub(super) fn backend_client<'a>(
        &'a self,
        credentials: &'a Arc<AuthManager>,
        auth: &'a CodexAuth,
        http_client_factory: codex_http_client::HttpClientFactory,
    ) -> BoxFuture<'a, Result<BackendClient, JSONRPCErrorError>> {
        Box::pin(async move {
            let config = self.load_latest_config().await;
            self.backend_client_for_config(&config, credentials, auth, http_client_factory)
                .await
        })
    }

    pub(super) fn backend_client_for_config<'a>(
        &'a self,
        config: &'a Config,
        credentials: &'a Arc<AuthManager>,
        auth: &'a CodexAuth,
        http_client_factory: codex_http_client::HttpClientFactory,
    ) -> BoxFuture<'a, Result<BackendClient, JSONRPCErrorError>> {
        Box::pin(async move {
            let runtime = self
                .thread_manager
                .runtime_auth_for_config(config)
                .map_err(|error| internal_error(error.to_string()))?
                .map(|(runtime, mode)| (runtime.for_credentials(Arc::clone(credentials)), mode));
            BackendClient::from_auth_with_runtime(
                config.chatgpt_base_url.clone(),
                auth,
                http_client_factory,
                runtime.as_ref(),
                /*headers*/ &Default::default(),
            )
            .await
            .map_err(|error| internal_error(error.to_string()))
        })
    }
}
