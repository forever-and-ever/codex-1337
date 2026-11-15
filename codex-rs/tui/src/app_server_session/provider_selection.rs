//! Provider request overrides honor managed requirements over explicit invocation choices.
//! Omission lets the server resolve the provider for thread creation and forks.

use super::AppServerBootstrap;
use super::AppServerSession;
use super::BootstrapAccount;
use super::ThreadParamsMode;
use crate::legacy_core::config::Config;
use crate::resume_picker::SessionSelection;
use codex_config::ConfigLayerSource;
use codex_protocol::ThreadId;
use color_eyre::Result;
use std::time::Instant;

pub(crate) fn explicit_provider(config: &Config) -> Option<String> {
    config
        .config_layer_stack
        .layers_high_to_low()
        .find(|layer| layer.config.get("model_provider").is_some())
        .filter(|layer| {
            matches!(
                layer.name,
                ConfigLayerSource::SessionFlags
                    | ConfigLayerSource::User {
                        profile: Some(_),
                        ..
                    }
            )
        })
        .map(|_| config.model_provider_id.clone())
}

impl AppServerSession {
    /// A remote client forwards its local named profile. Embedded sessions reload that same
    /// profile from disk before /new, so only invocation flags can pin its provider.
    pub(crate) fn client_provider_override_for_new_chats(&self, config: &Config) -> Option<String> {
        match self.thread_params_mode() {
            ThreadParamsMode::Remote => self.explicit_model_provider(config),
            ThreadParamsMode::Embedded => {
                let launch_flag = self.model_provider_override.is_some()
                    || config.config_layer_stack.layers_high_to_low().any(|layer| {
                        layer.name == ConfigLayerSource::SessionFlags
                            && layer.config.get("model_provider").is_some()
                    });
                launch_flag
                    .then(|| self.explicit_model_provider(config))
                    .flatten()
            }
        }
    }

    pub(crate) fn explicit_model_provider(&self, config: &Config) -> Option<String> {
        self.model_provider_override
            .as_ref()
            .map(|provider| {
                config
                    .config_layer_stack
                    .required_model_provider()
                    .unwrap_or(provider)
                    .to_owned()
            })
            .or_else(|| explicit_provider(config))
    }

    pub(crate) async fn bootstrap_for_selection(
        &mut self,
        config: &Config,
        selection: &SessionSelection,
    ) -> Result<AppServerBootstrap> {
        match selection {
            SessionSelection::Resume(target) | SessionSelection::Fork(target) => {
                self.bootstrap_for_thread(config, Some(target.thread_id))
                    .await
            }
            SessionSelection::StartFresh
            | SessionSelection::AgentsOverview
            | SessionSelection::Exit => self.bootstrap(config).await,
        }
    }

    pub(crate) async fn bootstrap_for_thread(
        &mut self,
        config: &Config,
        thread_id: Option<ThreadId>,
    ) -> Result<AppServerBootstrap> {
        let started_at = Instant::now();
        let saved = match thread_id {
            Some(thread_id) => Some(self.thread_read(thread_id, /*include_turns*/ false).await?),
            None => None,
        };
        let model_provider = saved
            .as_ref()
            .map(|thread| thread.model_provider.clone())
            .or_else(|| self.explicit_model_provider(config));
        let mut bootstrap = match saved {
            Some(thread) => {
                let account = if thread.status == codex_app_server_protocol::ThreadStatus::NotLoaded
                {
                    BootstrapAccount::AwaitingResume
                } else {
                    BootstrapAccount::Loaded(
                        self.read_bootstrap_account(config, thread_id).await?.into(),
                    )
                };
                self.bootstrap_with_account_for_thread(config, account, thread)
                    .await?
            }
            None => {
                let account = self.read_bootstrap_account(config, None).await?;
                self.bootstrap_with_account(config, account, model_provider)
                    .await?
            }
        };
        bootstrap.duration = started_at.elapsed();
        Ok(bootstrap)
    }
}
