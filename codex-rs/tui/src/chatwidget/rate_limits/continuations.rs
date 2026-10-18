//! Submission gates while deferred model settings are pending.

use super::ChatWidget;

impl ChatWidget {
    pub(in crate::chatwidget) fn submission_settings_pending(&self) -> bool {
        self.input_queue.rate_limit_recovery_pending
            || self.input_queue_frontend.automatic_model_switch_pending
    }

    pub(crate) fn automatic_model_switch_pending(&self) -> bool {
        self.input_queue_frontend.automatic_model_switch_pending
    }

    pub(crate) fn hold_automatic_model_switch(&mut self) -> bool {
        std::mem::replace(
            &mut self.input_queue_frontend.automatic_model_switch_pending,
            true,
        )
    }

    pub(crate) fn finish_automatic_model_switch(&mut self) {
        if std::mem::take(&mut self.input_queue_frontend.automatic_model_switch_pending) {
            self.submit_initial_user_message_if_pending();
            let _ = self.maybe_send_next_queued_input();
        }
    }
}
