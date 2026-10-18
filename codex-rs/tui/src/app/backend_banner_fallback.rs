//! Apply backend-authorized fallback and task-local Reserve settings without saving defaults.

mod continuations;

use super::App;
use crate::app_event::AppEvent;
use crate::app_server_session::AppServerSession;
use crate::chatwidget::AutomaticModelSwitchReason;
use crate::model_catalog::LUNA_RESERVE_MODEL;
use crate::model_catalog::model_display_name;
use crate::service_tier_resolution;
use codex_app_server_protocol::ThreadSettingsUpdateParams;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ModeKind;
use codex_protocol::openai_models::ReasoningEffort;

impl App {
    pub(super) async fn update_luna_reserve_reasoning(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        effort: Option<ReasoningEffort>,
    ) {
        // A picker left open during recovery or a task switch must not affect the new model.
        if self.active_thread_id != Some(thread_id)
            || self.chat_widget.thread_id() != Some(thread_id)
            || self.chat_widget.current_model() != LUNA_RESERVE_MODEL
        {
            return;
        }
        self.app_event_tx.send(AppEvent::FollowTranscript);
        let mut mode = self.chat_widget.effective_collaboration_mode();
        mode.settings.reasoning_effort = effort.clone();
        let params = ThreadSettingsUpdateParams {
            thread_id: thread_id.to_string(),
            effort: effort.clone(),
            collaboration_mode: Some(mode.clone()),
            ..ThreadSettingsUpdateParams::default()
        };
        self.request_reserve_reasoning(app_server, thread_id, effort, mode.mode, params);
    }

    pub(super) async fn apply_backend_banner_fallback(
        &mut self,
        app_server: &mut AppServerSession,
    ) {
        // Reattached tasks must wait for the read that supersedes the latest hard stop.
        if self.rate_limit_refresh_state.has_pending_recovery() {
            return;
        }
        let Some(thread_id) = self.active_thread_id else {
            return;
        };
        if Some(thread_id) != self.chat_widget.thread_id() {
            return;
        }
        let Some(switch) = self.chat_widget.backend_banner_fallback() else {
            return;
        };
        if self.chat_widget.hold_automatic_model_switch() {
            return;
        }
        let generation = self.model_selection_generation;
        let original_model = self.chat_widget.current_model().to_owned();
        let original_effort = self.chat_widget.current_reasoning_effort();
        let target = switch.model;
        let entering_reserve = target.model == crate::model_catalog::LUNA_RESERVE_MODEL;
        if entering_reserve && !self.chat_widget.prepare_luna_reserve_return() {
            self.chat_widget.show_unavailable_reserve_recovery();
            return;
        }
        let effort = switch
            .effort
            .filter(|effort| {
                target
                    .supported_reasoning_efforts
                    .iter()
                    .any(|option| option.effort == *effort)
            })
            .unwrap_or(target.default_reasoning_effort);
        let mut mode = self.chat_widget.effective_collaboration_mode();
        mode.settings.model = target.model.clone();
        mode.settings.reasoning_effort = Some(effort.clone());
        // Automatic recovery must not apply the permission defaults used by manual model selection.
        let params = ThreadSettingsUpdateParams {
            thread_id: thread_id.to_string(),
            model: Some(target.model.clone()),
            effort: Some(effort.clone()),
            collaboration_mode: Some(mode.clone()),
            service_tier: service_tier_resolution::service_tier_update_for_core(
                self.chat_widget.config_ref(),
                &self.local_settings.notices,
                &target.model,
                &self.model_catalog.try_list_models().unwrap_or_default(),
            ),
            ..ThreadSettingsUpdateParams::default()
        };
        // Older remote servers can decline this method. Keep the existing recovery UI in that
        // case or on failure, rather than changing local selection before the task accepts it.
        // The operation lane orders the write before later manual selections.
        self.request_backend_banner_fallback(
            app_server,
            thread_id,
            params,
            mode,
            generation,
            original_model,
            original_effort,
            target.model,
            switch.reason,
            effort,
            entering_reserve,
        );
    }
}
