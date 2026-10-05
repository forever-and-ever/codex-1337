//! Reload safety concerns unfinished user actions, not whether the agent is working.

use super::*;

impl ChatWidget {
    #[cfg(target_os = "linux")]
    pub(crate) fn can_reload_ui(&self) -> bool {
        self.bottom_pane.reload_ready()
            && self.session_draft != Some(SessionDraft::Starting)
            && self.external_editor_state == ExternalEditorState::Closed
            && self.pending_image_submission.is_none()
            && self.input_queue.pending_steer_replacement.is_none()
            && !self.input_queue.rate_limit_recovery_pending
            && self.input_queue.startup_submission.is_none()
            && !self.input_queue.user_turn_pending_start
            && !self.input_queue.submit_pending_steer_after_interrupt
            && !self.may_receive_realtime_transcripts()
            && !self.realtime_retry_cleanup_pending()
    }
}
