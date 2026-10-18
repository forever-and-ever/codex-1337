//! Request continuations for model fallback and Reserve reasoning changes.

use super::App;
use super::AppServerSession;
use super::AutomaticModelSwitchReason;
use super::LUNA_RESERVE_MODEL;
use super::ModeKind;
use super::ReasoningEffort;
use super::ThreadId;
use super::ThreadSettingsUpdateParams;
use super::model_display_name;

impl App {
    pub(super) fn request_reserve_reasoning(
        &mut self,
        app_server: &AppServerSession,
        thread_id: ThreadId,
        effort: Option<ReasoningEffort>,
        mode: ModeKind,
        params: ThreadSettingsUpdateParams,
    ) {
        self.model_selection_generation.advance();
        let generation = self.model_selection_generation;
        self.request_thread_settings_update(app_server, params, move |app, updated| {
            if updated
                && app.model_selection_generation == generation
                && app.active_thread_id == Some(thread_id)
                && app.chat_widget.thread_id() == Some(thread_id)
                && app.chat_widget.current_model() == LUNA_RESERVE_MODEL
            {
                app.chat_widget.set_reasoning_effort(effort.clone());
                if mode == ModeKind::Plan {
                    app.chat_widget.set_plan_mode_reasoning_effort(effort);
                }
            }
        });
    }

    pub(super) fn request_backend_banner_fallback(
        &mut self,
        app_server: &AppServerSession,
        thread_id: ThreadId,
        params: ThreadSettingsUpdateParams,
        mode: codex_protocol::config_types::CollaborationMode,
        generation: super::super::thread_settings::ModelSelectionGeneration,
        original_model: String,
        original_effort: Option<ReasoningEffort>,
        model: String,
        reason: AutomaticModelSwitchReason,
        effort: ReasoningEffort,
        entering_reserve: bool,
    ) {
        let accepted =
            self.request_thread_settings_update(app_server, params, move |app, updated| {
                if app.active_thread_id != Some(thread_id)
                    || app.chat_widget.thread_id() != Some(thread_id)
                    || app.model_selection_generation != generation
                {
                    return;
                }
                if updated {
                    app.chat_widget.finish_backend_banner_fallback(
                        mode,
                        original_model,
                        original_effort,
                    );
                    let (prefix, suffix) = match reason {
                        AutomaticModelSwitchReason::UsageLimit => {
                            ("Automatically switched to", " due to usage limits.")
                        }
                        AutomaticModelSwitchReason::UsageRecovered => (
                            "Automatically switched back to",
                            " because ordinary usage is available again.",
                        ),
                    };
                    let mut message = format!("{prefix} {}", model_display_name(&model));
                    if let Some(label) = Self::reasoning_label_for(&model, Some(&effort)) {
                        message.push(' ');
                        message.push_str(&label);
                    }
                    message.push_str(suffix);
                    app.chat_widget.add_info_message(message, /*hint*/ None);
                } else if entering_reserve {
                    app.chat_widget.show_unavailable_reserve_recovery();
                }
            });
        if !accepted && entering_reserve {
            self.chat_widget.show_unavailable_reserve_recovery();
        }
    }
}
