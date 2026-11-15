use super::PluginRequestProcessor;
use super::internal_error;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_core::config::Config;
use codex_core_plugins::PluginsManager;
use codex_login::CodexAuth;
use std::sync::Arc;
use std::sync::OnceLock;

impl PluginRequestProcessor {
    pub(super) async fn captured_remote_plugin_context(
        &self,
    ) -> Result<(Config, Option<CodexAuth>, Arc<PluginsManager>), JSONRPCErrorError> {
        let source = self
            .auth_manager
            .capture_job_source()
            .map_err(|error| internal_error(error.to_string()))?;
        let credentials = source.credentials();
        let auth = credentials.auth().await;
        if source.requires_auth() && auth.is_none() {
            return Err(internal_error("Plugin account credentials are unavailable"));
        }
        source
            .validate()
            .map_err(|error| internal_error(error.to_string()))?;
        let mut config = self.load_latest_config(None).await?;
        config.application_network_policy =
            credentials.http_client_factory().network_policy().clone();
        let runtime = self
            .thread_manager
            .runtime_auth_for_config(&config)
            .map_err(|error| internal_error(error.to_string()))?;
        let slot = Arc::new(match runtime {
            Some((runtime, _)) => OnceLock::from(runtime.for_credentials(Arc::clone(&credentials))),
            None => OnceLock::new(),
        });
        let manager = Arc::new(
            self.thread_manager
                .plugins_manager()
                .for_auth_context(credentials, slot),
        );
        Ok((config, auth, manager))
    }
}
