use super::*;
use crate::agent::control::AgentTierStatus;
use crate::agent::control::normalize_agent_service_tier;

impl Session {
    pub(crate) async fn select_agent_service_tier(
        &self,
        requested: &str,
    ) -> CodexResult<AgentTierStatus> {
        let _guard = thread_settings::acquire_persistence_lock(self).await;
        let config = self.get_config().await;
        let model = config
            .model
            .as_deref()
            .ok_or_else(|| CodexErr::InvalidRequest("Agent model is absent.".to_owned()))?;
        let model_info = self
            .services
            .models_manager
            .get_model_info(model, &config.to_models_manager_config())
            .await;
        let selected = normalize_agent_service_tier(
            requested,
            &model_info,
            self.features.enabled(Feature::FastMode),
        )
        .map_err(CodexErr::InvalidRequest)?;
        if let Some(model) = self
            .active_turn
            .lock()
            .await
            .as_ref()
            .and_then(|turn| turn.task.as_ref())
            .map(|task| task.turn_context.capture_current_model_info())
        {
            normalize_agent_service_tier(
                requested,
                &model,
                self.features.enabled(Feature::FastMode),
            )
            .map_err(CodexErr::InvalidRequest)?;
        }
        let snapshot = {
            let mut state = self.state.lock().await;
            let configuration = &mut state.session_configuration;
            let parent = configuration.parent_thread_id.ok_or_else(|| {
                CodexErr::InvalidRequest(
                    "Only spawned agents have an inherited service tier.".to_owned(),
                )
            })?;
            // Ordinary thread-settings clearing means explicit default; this operation restores
            // inheritance. Keep that distinction in the existing optional persisted selection.
            Arc::make_mut(&mut configuration.step_settings).service_tier = selected.clone();
            self.services
                .agent_control
                .set_agent_tier_binding(self.thread_id, parent, selected);
            configuration.thread_settings_snapshot(&self.services.turn_environments.selections())
        };
        // Unlike an empty user composer, an explicitly spawned/retiered agent must retain
        // this selection even before its first turn materializes the rollout.
        self.send_event_raw(Event {
            id: self.next_internal_sub_id(),
            msg: EventMsg::ThreadSettingsApplied(
                codex_protocol::protocol::ThreadSettingsAppliedEvent {
                    thread_id: Some(self.thread_id),
                    thread_settings: snapshot,
                },
            ),
        })
        .await;
        self.flush_rollout()
            .await
            .map_err(|error| CodexErr::InvalidRequest(error.to_string()))?;
        Ok(self.agent_tier_status().await)
    }

    pub(crate) async fn agent_tier_status(&self) -> AgentTierStatus {
        let config = self.get_config().await;
        let snapshot = self.thread_config_snapshot().await;
        let requested = if snapshot.parent_thread_id.is_some() {
            self.services
                .agent_control
                .agent_service_tier(self.thread_id)
        } else {
            self.services.agent_control.root_service_tier()
        };
        let selected = if snapshot.parent_thread_id.is_some() {
            snapshot.service_tier
        } else {
            requested.clone()
        };
        let active_model = self
            .active_turn
            .lock()
            .await
            .as_ref()
            .and_then(|turn| turn.task.as_ref())
            .map(|task| task.turn_context.capture_current_model_info());
        let model = match active_model {
            Some(model) => model,
            None => Arc::new(
                self.services
                    .models_manager
                    .get_model_info(&snapshot.model, &config.to_models_manager_config())
                    .await,
            ),
        };
        AgentTierStatus {
            service_tier: selected.unwrap_or_else(|| {
                if snapshot.parent_thread_id.is_some() {
                    "inherit"
                } else {
                    "model_default"
                }
                .to_owned()
            }),
            effective_service_tier: get_service_tier(
                requested,
                self.features.enabled(Feature::FastMode),
                &model,
            ),
        }
    }
}
