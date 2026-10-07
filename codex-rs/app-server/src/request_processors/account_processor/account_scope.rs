//! Account reads select a conversation or fresh-root scope without retargeting the global login.

use super::*;
use codex_core::config::ConfigOverrides;
use codex_model_provider_info::ModelProviderAdapter;
use codex_utils_path_uri::PathConvention;

impl AccountRequestProcessor {
    pub(crate) async fn for_account_read(
        &self,
        params: &GetAccountParams,
    ) -> Result<Self, JSONRPCErrorError> {
        let Some(new_thread) = &params.new_thread else {
            return self.for_thread(params.thread_id.as_deref()).await;
        };
        if params.thread_id.is_some() {
            return Err(invalid_request(
                "newThread cannot be combined with threadId",
            ));
        }
        let cwd = new_thread
            .cwd
            .as_ref()
            .map(|cwd| {
                cwd.to_path_uri(PathConvention::native())
                    .map(|cwd| cwd.to_path_buf())
            })
            .transpose()
            .map_err(|error| invalid_request(error.to_string()))?;
        let config = self
            .config_manager
            .load_with_overrides(
                new_thread.config.clone(),
                ConfigOverrides {
                    model_provider: new_thread.model_provider.clone(),
                    cwd,
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| invalid_request(error.to_string()))?;
        let mut scoped = self.clone();
        scoped.auth_manager = self
            .thread_manager
            .auth_for_new_thread(&config)
            .await
            .map_err(|error| invalid_request(error.to_string()))?;
        scoped.scoped_provider_account_id = (config.model_provider.adapter
            == ModelProviderAdapter::ClaudeSubscription)
            .then(|| config.wrangler.default_claude_account.clone())
            .flatten();
        scoped.scoped_config = Some(Arc::new(config));
        Ok(scoped)
    }
}
