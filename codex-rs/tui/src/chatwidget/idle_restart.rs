//! The client can disappear only when no local input or foreground operation is lost.

use super::*;

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
            && self.input_queue.pending_steer_replacement.is_none()
            && !self.input_queue.rate_limit_recovery_pending
            && !self.input_queue.submit_pending_steer_after_interrupt
    }
}

impl ThreadInputState {
    pub(crate) fn has_unsent_input(&self) -> bool {
        self.composer.is_some()
            || self.questions.as_ref().is_some_and(|q| q.unanswered_count() != 0)
            || self.pending_steer.is_some()
            || !self.rejected_steers_queue.is_empty()
            || !self.queued_user_messages.is_empty()
            // safety_buffering_prompt also retains already accepted input after
            // completion; only an unacknowledged submission is irreplaceable.
            || self.user_turn_pending_start
            || self.submit_pending_steer_after_interrupt
    }
}
