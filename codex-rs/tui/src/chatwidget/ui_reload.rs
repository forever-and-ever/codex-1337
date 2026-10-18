//! Reload safety concerns unfinished user actions, not whether the agent is working.

use super::ChatWidget;
use super::ExternalEditorState;
use super::SessionDraft;
use super::input_queue;

impl ChatWidget {
    #[cfg(target_os = "linux")]
    pub(crate) fn can_reload_ui(&self) -> bool {
        self.bottom_pane.reload_ready()
            && self.session_draft != Some(SessionDraft::Starting)
            && self.external_editor_state == ExternalEditorState::Closed
            && self.pending_image_submission.is_none()
            && self.input_queue.pending_steer.replacement().is_none()
            && !self.submission_settings_pending()
            && self.input_queue_frontend.startup_submission.is_none()
            && !self.input_queue.turn.is_pending()
            && self.input_queue.pending_steer.interrupt()
                != input_queue::SteerInterrupt::SubmitAsTurn
            && !self.may_receive_realtime_transcripts()
            && !self.realtime_retry_cleanup_pending()
    }
}
