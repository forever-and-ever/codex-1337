//! The client can disappear only when no local input or foreground operation is lost.

use super::ChatWidget;
use super::ExternalEditorState;
use super::ThreadInputState;
use super::input_queue;

impl ChatWidget {
    pub(crate) fn ready_for_idle_restart(&self) -> bool {
        self.turn_lifecycle.last_turn_id.is_some()
            && !self.is_user_turn_pending_or_running()
            && !self.bottom_pane.is_task_running()
            && !self.has_active_agent_stream()
            && !self.has_active_plan_stream()
            && self.composer_is_empty()
            && self.composer_text_with_pending().is_empty()
            && self.no_modal_or_popup_active()
            && !self.has_pending_protected_request()
            && self.external_editor_state == ExternalEditorState::Closed
            && self.pending_image_submission.is_none()
            && self
                .bottom_pane
                .questions
                .as_ref()
                .is_none_or(|q| q.unanswered_count() == 0)
            && !self.input_queue.has_queued_follow_up_messages()
            && self.input_queue.pending_steer.is_none()
            && self.input_queue.pending_steer.replacement().is_none()
            && !self.submission_settings_pending()
            && self.input_queue.pending_steer.interrupt()
                != input_queue::SteerInterrupt::SubmitAsTurn
    }
}

impl ThreadInputState {
    pub(crate) fn has_unsent_input(&self) -> bool {
        self.composer
            .as_ref()
            .is_some_and(super::user_messages::ThreadComposerState::has_content)
            || self.initial_user_message.is_some()
            || self
                .questions
                .as_ref()
                .is_some_and(|q| q.unanswered_count() != 0)
            || self.input_queue.pending_steer.is_some()
            || self.input_queue.has_queued_follow_up_messages()
            || self.input_queue.turn.is_pending()
    }
}
