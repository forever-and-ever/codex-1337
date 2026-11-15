//! A recourse rewrite is prepared here, then admitted only by the app's verified fork continuation.

use super::super::AppCommand;
use super::super::ChatWidget;
use super::super::ShellEscapePolicy;
use super::super::UserMessageHistoryRecord;
use super::super::UserMessageSource;
use super::super::input_queue;
use super::super::submission_dispatch::UserTurnDispatch;

impl ChatWidget {
    pub(crate) fn prepare_safety_recourse_rewrite(&mut self, prompt: &str) -> Option<AppCommand> {
        if !self.is_session_configured()
            || self.is_user_turn_pending_or_running()
            || self.pending_image_submission.is_some()
            || self.input_queue.rate_limit_recovery_pending
            || self.safety_recourse.is_some()
            || self.submission_settings_pending()
            || crate::reload::worker::is_activating()
        {
            return None;
        }
        let thread_id = self.thread_id?;
        let op = self
            .submit_user_message_with_dispatch(
                prompt.into(),
                UserMessageHistoryRecord::UserMessageText,
                ShellEscapePolicy::Disallow,
                UserMessageSource::SafetyRecourse,
                None,
                UserTurnDispatch::Prepared,
            )
            .1?;
        // A recovered draft is ordinary input, not another request to reconstruct a recourse fork.
        match &mut self.input_queue.turn {
            input_queue::UserTurnInput::AwaitingStart(
                input_queue::PendingTurnStart::Submitted(input),
            )
            | input_queue::UserTurnInput::Started(input) => {
                input.source = UserMessageSource::Prompt;
                input.dispatch = UserTurnDispatch::Thread(thread_id);
            }
            _ => {}
        }
        Some(op)
    }
}
