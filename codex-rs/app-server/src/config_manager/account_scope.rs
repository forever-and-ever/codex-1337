use super::*;

impl ConfigManager {
    /// Reload account policy without replacing another account's shared policy owner.
    pub(crate) async fn load_account_policy(
        &self,
        auth: Arc<AuthManager>,
        config: &Config,
    ) -> std::io::Result<Config> {
        let endpoint = codex_backend_client::Client::new(
            config.chatgpt_base_url.clone(),
            config.http_client_factory(),
        )
        .config_bundle_url();
        let factory = config.http_client_factory().with_network_policy(
            self.local_network_policy
                .policy()
                .restrict_to_endpoints(endpoint.parse().into_iter().collect()),
        );
        let loader = cloud_config_bundle_loader(
            auth,
            config.chatgpt_base_url.clone(),
            self.codex_home.clone(),
            factory,
        );
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
