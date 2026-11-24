use super::Arc;
use super::AuthManager;
use super::Config;
use super::ConfigManager;
use super::managed_config_bundle_loader;

impl ConfigManager {
    /// Reload account policy without replacing another account's shared policy owner.
    pub(crate) async fn load_account_policy(
        &self,
        auth: Arc<AuthManager>,
        config: &Config,
    ) -> std::io::Result<Config> {
        let loader = managed_config_bundle_loader(
            auth,
            config.chatgpt_base_url.clone(),
            self.codex_home.clone(),
            config
                .http_client_factory()
                .with_network_policy(self.local_network_policy.policy()),
            config.config_layer_stack.requirements_toml(),
        )
        .await;
        let mut scoped = Self::new(
            self.codex_home.clone(),
            self.current_cli_overrides(),
            self.loader_overrides.clone(),
            self.strict_config,
            loader,
            self.arg0_paths.clone(),
            Arc::clone(&self.thread_config_loader),
        );
        scoped.runtime_feature_enablement = Arc::clone(&self.runtime_feature_enablement);
        scoped
            .load_latest_config(Some(config.cwd.to_path_buf()))
            .await
    }
}
