//! Persist the user's Daybreak selection for the active thread and new threads.

mod continuations;

use super::*;

impl App {
    pub(super) async fn persist_daybreak_selection(
        &mut self,
        server: &mut AppServerSession,
        thread_id: ThreadId,
        enabled: bool,
    ) {
        if !self
            .chat_widget
            .config_ref()
            .features
            .enabled(Feature::CliDaybreak)
            || self.active_thread_id != Some(thread_id)
        {
            return;
        }
        if enabled
            && (!self.chat_widget.daybreak_account_eligible()
                || !crate::daybreak::available(&self.chat_widget.model_catalog().models))
        {
            self.chat_widget.add_error_message(
                "Daybreak availability could not be confirmed for this account.".into(),
            );
            return;
        }
        self.persist_daybreak_selection_continuation(server, thread_id, enabled, |_| {});
    }
}
