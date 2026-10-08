//! Session facts that start a warming hold or break the cached prompt prefix.
use super::Session;
use super::session::SessionConfiguration;
use crate::PromptCacheFact;
use crate::WarmingLore;

impl Session {
    pub(crate) async fn prompt_cache_lore(&self) -> Option<WarmingLore> {
        let config = self.get_config().await;
        let snapshot = self.thread_config_snapshot().await;
        let model = self
            .services
            .models_manager
            .get_model_info(&snapshot.model, &config.to_models_manager_config())
            .await;
        self.services
            .model_client
            .prompt_cache_lore(&model, snapshot.reasoning_effort.as_ref())
    }

    pub(super) fn observe_human_turn(&self, human: bool) {
        if human {
            self.services
                .model_client
                .observe_prompt_cache(PromptCacheFact::HumanTurn);
        }
    }

    /// Only switches that change the prefix's identity invalidate it; other settings append
    /// context to the next request and keep the cached prefix.
    pub(super) fn observe_prompt_cache_settings(
        &self,
        previous: &SessionConfiguration,
        next: &SessionConfiguration,
    ) {
        let (previous_mode, next_mode) = (
            &previous.step_settings.collaboration_mode,
            &next.step_settings.collaboration_mode,
        );
        if previous_mode.model() != next_mode.model()
            || previous_mode.reasoning_effort() != next_mode.reasoning_effort()
            || previous.provider.info() != next.provider.info()
        {
            self.services
                .model_client
                .observe_prompt_cache(PromptCacheFact::Invalidated);
        }
    }
}
