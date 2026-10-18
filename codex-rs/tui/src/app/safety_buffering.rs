//! Safety-buffered turn retries that preserve the source thread.

use super::*;
use crate::app_server_session::turn_permissions_overrides;
use crate::chatwidget::ThreadInputState;
use crate::chatwidget::ThreadInputStateRestoreMode;
use crate::chatwidget::UserMessage;

pub(super) struct SafetyBufferedRetry {
    pub(super) thread_id: ThreadId,
    pub(super) turn_id: String,
    pub(super) model: String,
    pub(super) turn: AppCommand,
    pub(super) prompt: UserMessage,
}

impl App {
    pub(super) async fn retry_safety_buffered_turn(
        &mut self,
        _tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        retry: SafetyBufferedRetry,
    ) {
        let SafetyBufferedRetry {
            thread_id,
            turn_id,
            model,
            mut turn,
            prompt,
        } = retry;
        if self.active_thread_id != Some(thread_id)
            || self.chat_widget.thread_id() != Some(thread_id)
            || self.primary_thread_id != Some(thread_id)
        {
            return;
        }
        if !self.chat_widget.can_retry_safety_buffered_turn(&turn_id) {
            return;
        }

        let retry_config = self.chat_widget.config_ref().clone();
        let input_state = self.chat_widget.capture_thread_input_state();
        if self.pending_server_profiles.contains_key(&thread_id) {
            self.fail_safety_buffered_branch(
                input_state,
                prompt,
                color_eyre::eyre::eyre!("Wait for permissions to update before forking."),
            );
            return;
        }

        let AppCommand::UserTurn {
            cwd,
            active_permission_profile,
            model: turn_model,
            effort,
            collaboration_mode,
            ..
        } = &mut turn
        else {
            self.chat_widget.add_error_message(
                "Failed to retry with a faster model: original turn is unavailable.".to_string(),
            );
            return;
        };
        let permissions_override = Self::turn_permissions_override_from_config(
            &retry_config,
            active_permission_profile.as_ref(),
            self.runtime_permission_profile_override
                .as_ref()
                .and_then(RuntimePermissionProfileOverride::turn_permission_profile),
        );
        if let Err(err) = turn_permissions_overrides(permissions_override, cwd.as_path()) {
            self.chat_widget
                .add_error_message(format!("Failed to retry with a faster model: {err}"));
            return;
        }
        let daybreak_enabled = self.chat_widget.daybreak_enabled
            && !self.chat_widget.side_conversation_active()
            && !self.side_threads.contains_key(&thread_id);
        let eligible_account = self.chat_widget.daybreak_account_eligible();
        if let Err(message) = crate::daybreak::program_for_turn(
            &self.chat_widget.model_catalog().models,
            &model,
            eligible_account,
            daybreak_enabled,
        ) {
            self.chat_widget.add_error_message(message);
            return;
        }
        *turn_model = model.clone();
        *effort = Some(ReasoningEffortConfig::Low);
        *collaboration_mode = collaboration_mode.as_ref().map(|mode| {
            mode.with_updates(
                Some(model),
                Some(Some(ReasoningEffortConfig::Low)),
                /*developer_instructions*/ None,
            )
        });

        self.enqueue_safety_fork(app_server, thread_id, turn_id, turn, prompt)
            .await;
    }

    #[cfg(test)]
    pub(super) async fn retry_safety_buffered_turn_and_settle(
        &mut self,
        tui: &mut tui::Tui,
        server: &mut AppServerSession,
        retry: SafetyBufferedRetry,
    ) {
        self.retry_safety_buffered_turn(tui, server, retry).await;
        self.settle_navigation(tui, server)
            .await
            .expect("safety fork settles");
    }

    fn fail_safety_buffered_branch(
        &mut self,
        input_state: Option<ThreadInputState>,
        prompt: UserMessage,
        err: impl std::fmt::Display,
    ) {
        self.chat_widget.restore_thread_input_state(
            input_state,
            ThreadInputStateRestoreMode {
                preserve_in_flight_turn: false,
            },
        );
        self.chat_widget.cancel_safety_buffered_retry_submission();
        self.chat_widget.restore_user_message_to_composer(prompt);
        self.chat_widget
            .add_error_message(format!("Failed to retry with a faster model: {err}"));
    }
}

pub(super) fn safety_retry_fork_point(turns: &[Turn], turn_id: &str) -> Result<()> {
    let Some(turn_index) = turns.iter().position(|turn| turn.id == turn_id) else {
        return Err(color_eyre::eyre::eyre!(
            "interrupted turn {turn_id} is missing from the source thread"
        ));
    };
    if turn_index + 1 != turns.len() {
        return Err(color_eyre::eyre::eyre!(
            "interrupted turn {turn_id} is no longer the latest turn"
        ));
    }
    if turns[turn_index].status == TurnStatus::InProgress {
        return Err(color_eyre::eyre::eyre!(
            "interrupted turn {turn_id} is still in progress"
        ));
    }

    let Some(previous_turn) = turns[..turn_index].last() else {
        return Ok(());
    };
    if previous_turn.status == TurnStatus::InProgress {
        return Err(color_eyre::eyre::eyre!(
            "previous turn {} is still in progress",
            previous_turn.id
        ));
    }

    Ok(())
}

#[cfg(test)]
#[path = "safety_buffering_tests.rs"]
mod tests;
