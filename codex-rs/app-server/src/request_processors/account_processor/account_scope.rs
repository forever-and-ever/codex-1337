//! Account requests select a conversation or fresh-root scope without retargeting the global login.

use super::AccountRequestProcessor;
use super::Arc;
use super::JSONRPCErrorError;
use super::Result;
use super::invalid_request;
use codex_app_server_protocol::NewThreadAccountParams;
use codex_core::config::ConfigOverrides;
use codex_login::anthropic::AccountId;
use codex_model_provider_info::ModelProviderAdapter;
use codex_protocol::KeyName;
use codex_utils_path_uri::PathConvention;

#[derive(Clone)]
pub(super) enum ScopedProviderAccount {
    Claude(AccountId),
    OpenRouter(KeyName),
}

impl ScopedProviderAccount {
    pub(super) fn from_committed(
        adapter: ModelProviderAdapter,
        id: Option<String>,
    ) -> Result<Option<Self>, JSONRPCErrorError> {
        match (adapter, id) {
            (ModelProviderAdapter::OpenaiCompatible, _)
            | (ModelProviderAdapter::ClaudeSubscription | ModelProviderAdapter::OpenRouter, None) => {
                Ok(None)
            }
            (ModelProviderAdapter::ClaudeSubscription, Some(id)) => id
                .parse::<AccountId>()
                .map(Self::Claude)
                .map(Some)
                .map_err(|error| invalid_request(error.to_string())),
            (ModelProviderAdapter::OpenRouter, Some(id)) => id
                .parse::<KeyName>()
                .map(Self::OpenRouter)
                .map(Some)
                .map_err(invalid_request),
        }
    }

    pub(super) fn id(&self) -> String {
        match self {
            Self::Claude(id) => id.to_string(),
            Self::OpenRouter(key) => key.to_string(),
        }
    }
}

impl AccountRequestProcessor {
    pub(crate) async fn for_account_scope(
        &self,
        thread_id: Option<&str>,
        new_thread: Option<&NewThreadAccountParams>,
    ) -> Result<Self, JSONRPCErrorError> {
        let Some(new_thread) = new_thread else {
            return self.for_thread(thread_id).await;
        };
        if thread_id.is_some() {
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
        scoped.scoped_provider_account = match config.model_provider.adapter {
            ModelProviderAdapter::OpenaiCompatible => None,
            ModelProviderAdapter::ClaudeSubscription => ScopedProviderAccount::from_committed(
                ModelProviderAdapter::ClaudeSubscription,
                config.wrangler.default_claude_account.clone(),
            )?,
            ModelProviderAdapter::OpenRouter => config
                .wrangler
                .openrouter
                .default_key()
                .cloned()
                .map(ScopedProviderAccount::OpenRouter),
        };
        scoped.scoped_config = Some(Arc::new(config));
        Ok(scoped)
    }
}
