//! UI event continuations for persistence, export, and prompt-revert receipts.

use super::App;
use super::AppEvent;
use super::AppServerSession;
use super::Arc;
use super::HistoryCell;
use super::ThreadId;
use super::tui;
use super::turn_submissions;
use crate::app::transcript_splice::TranscriptSplice;

impl App {
    pub(super) async fn dispatch_submission_event(
        &mut self,
        app_server: &mut AppServerSession,
        event: turn_submissions::SubmissionEvent,
    ) {
        let retired = matches!(&event, turn_submissions::SubmissionEvent::Retire);
        self.handle_submission_event(app_server, event).await;
        if retired {
            self.sync_frontend_requests();
        }
    }

    pub(super) fn open_transcript_export_menu(&mut self) {
        if let Some(active) = &self.active_export {
            self.chat_widget.show_transcript_export_cancel(active.pages);
        } else {
            self.chat_widget.show_transcript_export_options();
        }
    }

    pub(super) fn finish_prompt_revert_projection(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &AppServerSession,
        thread_id: ThreadId,
        selected_cell: &Arc<dyn HistoryCell>,
    ) {
        self.pending_thread_switch_resets -= 1;
        if self.chat_widget.thread_id() == Some(thread_id) {
            if let Some(index) = self
                .transcript_cells
                .iter()
                .position(|cell| Arc::ptr_eq(cell, selected_cell))
            {
                self.commit_transcript_splice(
                    tui,
                    TranscriptSplice::Members {
                        range: index..self.transcript_cells.len(),
                        cells: Vec::new(),
                        retained: Vec::new(),
                    },
                );
                self.code_mode_transcript.retain(&self.transcript_cells);
                self.native_history.retain(&self.transcript_cells);
            }
            self.transcript_view.jump_to_latest();
            self.scrollback_has_older_history = app_server.has_older_history(thread_id);
            self.deferred_history_lines.clear();
            self.last_rendered_history_tail = None;
            self.last_thread_usage_status_cell = None;
            self.pending_thread_usage_history_refresh = false;
            self.backtrack_render_pending = !tui.is_owned_screen();
            self.chat_widget
                .set_queue_autosend_suppressed(/*suppressed*/ false);
            self.chat_widget.emit_prompt_edit_thread_event();
            tui.frame_requester().schedule_frame();
        }
    }

    pub(in crate::app) fn persist_selected_model(
        &mut self,
        app_server: &AppServerSession,
        model: String,
        effort: Option<codex_protocol::openai_models::ReasoningEffort>,
    ) {
        self.app_event_tx.send(AppEvent::FollowTranscript);
        let edits = crate::config_update::build_model_selection_edits(
            &self.chat_widget.config_ref().model_provider_id,
            model.as_str(),
            effort.as_ref(),
        );
        self.queue_model_defaults(
            app_server,
            edits,
            "default model and reasoning effort",
            move |app, _, result| {
                match result {
                    Ok(()) => {
                        let mut message = format!("Model changed to {model}");
                        if let Some(label) = Self::reasoning_label_for(&model, effort.as_ref()) {
                            message.push(' ');
                            message.push_str(&label);
                        }
                        app.chat_widget.add_info_message(message, None);
                    }
                    Err(error) => app.chat_widget.add_error_message(format!(
                        "Failed to save default model: config/batchWrite failed in TUI: {error}"
                    )),
                }
                Ok(())
            },
        );
    }

    pub(super) async fn select_service_tier(&mut self, service_tier: Option<String>) {
        self.config.service_tier.clone_from(&service_tier);
        self.refresh_status_line();
        self.sync_active_thread_service_tier_to_cached_session()
            .await;
        self.chat_widget.add_info_message(
            service_tier.map_or_else(
                || "Service tier cleared for this conversation".to_string(),
                |tier| format!("Service tier set to {tier} for this conversation"),
            ),
            None,
        );
    }

    pub(super) fn persist_plan_reasoning_effort(
        &mut self,
        app_server: &AppServerSession,
        effort: Option<codex_protocol::openai_models::ReasoningEffort>,
    ) {
        self.app_event_tx.send(AppEvent::FollowTranscript);
        let edit = effort.map_or_else(
            || crate::config_update::clear_config_value("plan_mode_reasoning_effort"),
            |effort| {
                crate::config_update::replace_config_value(
                    "plan_mode_reasoning_effort",
                    serde_json::json!(effort.to_string()),
                )
            },
        );
        self.queue_model_defaults(
            app_server,
            vec![edit],
            "Plan mode reasoning effort",
            |app, _, result| {
                if let Err(error) = result {
                    app.chat_widget.add_error_message(format!(
                        "Failed to save Plan mode reasoning effort: {error}"
                    ));
                }
                Ok(())
            },
        );
    }
    pub(super) fn persist_conversation_model_selection(
        &mut self,
        app_server: &AppServerSession,
        model: &str,
        effort: &codex_protocol::openai_models::ReasoningEffort,
        default_effort: Option<&codex_protocol::openai_models::ReasoningEffort>,
    ) {
        let message = format!("Model changed to {model} {effort} for this conversation");
        if let Some(default_effort) = default_effort {
            let edits = crate::config_update::build_model_selection_edits(
                &self.chat_widget.config_ref().model_provider_id,
                model,
                Some(default_effort),
            );
            self.queue_model_defaults(
                app_server,
                edits,
                "default model and reasoning effort",
                move |app, _, result| {
                    match result {
                        Ok(()) => app.chat_widget.add_info_message(message, None),
                        Err(error) => app
                            .chat_widget
                            .add_error_message(format!("Failed to save default model: {error}")),
                    }
                    Ok(())
                },
            );
        } else {
            self.chat_widget.add_info_message(message, None);
        }
    }
}
