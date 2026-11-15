//! Apply complete session model choices after server acceptance, separately from saved defaults.
//!
//! A successful config write can still be overridden. Report that distinction without
//! replacing the active task's explicit selection with launch-time configuration.

mod continuations;

use super::App;
use crate::app_server_session::AppServerSession;
use crate::service_tier_resolution;
use codex_app_server_protocol::ConfigEdit;
use codex_app_server_protocol::WriteStatus;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::openai_models::ReasoningEffort;
use color_eyre::eyre::Result;

impl App {
    pub(super) async fn apply_default_model_selection(
        &mut self,
        app_server: &mut AppServerSession,
        model: String,
        effort: Option<ReasoningEffort>,
    ) {
        if self
            .active_thread_model_setting_update_params(model.clone())
            .is_some_and(|params| params.permissions.is_some())
            && self.reject_pending_permission_change()
        {
            return;
        }
        let model_changed = self.chat_widget.current_model() != model
            || self.chat_widget.current_collaboration_mode().model() != model;
        if model_changed {
            self.chat_widget.set_model(&model);
            self.sync_active_thread_model_setting(app_server, model.clone(), /*effort*/ None)
                .await;
            self.sync_active_thread_service_tier_to_cached_session()
                .await;
        }
        self.on_update_reasoning_effort(effort.clone());
        self.sync_active_thread_reasoning_setting(app_server, effort.clone())
            .await;
        self.persist_selected_model(app_server, model, effort);
    }

    pub(super) async fn select_session_model(
        &mut self,
        app_server: &mut AppServerSession,
        model: String,
        effort: Option<ReasoningEffort>,
        service_tier: Option<String>,
        sparkle_model: Option<String>,
    ) {
        let model_changed = self.model_selection_changes_current_model(&model);
        let effective_model_changed = self.chat_widget.current_model() != model;
        let params = if effective_model_changed {
            self.active_thread_model_setting_update_params(model.clone())
        } else {
            self.active_thread_reasoning_setting_update_params(effort.clone())
        };
        let Some(mut params) = params else {
            self.model_selection_generation.advance();
            self.apply_selected_session_model(
                model,
                effort,
                service_tier,
                model_changed,
                sparkle_model,
            );
            return;
        };
        if params.permissions.is_some() && self.reject_pending_permission_change() {
            return;
        }
        params.effort = effort.clone();
        params.collaboration_mode = Some(
            self.chat_widget
                .effective_collaboration_mode()
                .with_updates(Some(model.clone()), Some(effort.clone()), None),
        );
        params.service_tier = match service_tier.as_ref() {
            Some(tier) => Some(Some(tier.clone())),
            None => service_tier_resolution::service_tier_update_for_core(
                self.chat_widget.config_ref(),
                &self.local_settings.notices,
                &model,
                &self.model_catalog.models,
            ),
        };
        let unsupported_speed = service_tier
            .as_deref()
            .is_some_and(|tier| tier != SERVICE_TIER_DEFAULT_REQUEST_VALUE);
        self.model_selection_generation.advance();
        let generation = self.model_selection_generation;
        let origin = self.active_thread_id;
        if !app_server.operation_settings_supported() {
            if unsupported_speed {
                self.chat_widget.add_error_message(
                    "Model selection requires an app server with thread/settings/update support."
                        .into(),
                );
            } else {
                self.apply_selected_session_model(
                    model,
                    effort,
                    service_tier,
                    model_changed,
                    sparkle_model,
                );
            }
            return;
        }
        self.request_active_model_settings(
            app_server,
            params,
            move |app, result| {
                if app.model_selection_generation != generation
                    || app.active_thread_id != origin
                    || app.chat_widget.thread_id() != origin
                {
                    return;
                }
                match result {
                    Ok(true) => {}
                    Ok(false) if !unsupported_speed => {}
                    Ok(false) => {
                        app.chat_widget.add_error_message(
                            "Model selection requires an app server with thread/settings/update support.".into(),
                        );
                        return;
                    }
                    Err(_) => return,
                }
                app.apply_selected_session_model(model, effort, service_tier, model_changed, sparkle_model);
            },
        );
    }

    fn apply_selected_session_model(
        &mut self,
        model: String,
        effort: Option<ReasoningEffort>,
        service_tier: Option<String>,
        model_changed: bool,
        sparkle_model: Option<String>,
    ) {
        let in_plan_mode = self.chat_widget.effective_collaboration_mode().mode == ModeKind::Plan;
        let ultra = effort == Some(ReasoningEffort::Ultra);
        let clear_default_ultra = self
            .chat_widget
            .current_collaboration_mode()
            .reasoning_effort()
            == Some(ReasoningEffort::Ultra)
            && self.config.model_reasoning_effort != Some(ReasoningEffort::Ultra);
        let clear_plan_ultra = self.chat_widget.config_ref().plan_mode_reasoning_effort
            == Some(ReasoningEffort::Ultra)
            && self.config.plan_mode_reasoning_effort != Some(ReasoningEffort::Ultra);
        if model_changed {
            self.chat_widget.set_model(&model);
        }
        if !in_plan_mode
            || (self.chat_widget.current_collaboration_mode().model() == model
                && (ultra || clear_default_ultra))
        {
            self.chat_widget.set_reasoning_effort(effort.clone());
        }
        if in_plan_mode || ultra || clear_plan_ultra {
            self.chat_widget
                .set_plan_mode_reasoning_effort(effort.clone());
        }
        if let Some(service_tier) = service_tier.as_ref() {
            self.chat_widget
                .set_service_tier(Some(service_tier.clone()));
        }
        if let Some(model) = sparkle_model {
            self.chat_widget
                .on_sparkle_model_selected_from_picker(&model);
        }
        // Accepted settings notifications update the canonical cached sessions.
        let mut message = format!("Model changed to {model}");
        if let Some(label) = Self::reasoning_label_for(&model, effort.as_ref()) {
            message.push(' ');
            message.push_str(&label);
        }
        if let Some(service_tier) = service_tier {
            message.push(' ');
            message.push_str(if service_tier == SERVICE_TIER_DEFAULT_REQUEST_VALUE {
                "standard"
            } else if service_tier == ServiceTier::Fast.request_value() {
                "fast"
            } else {
                &service_tier
            });
        }
        message.push_str(" for this session only");
        self.chat_widget.add_info_message(message, /*hint*/ None);
    }
}
