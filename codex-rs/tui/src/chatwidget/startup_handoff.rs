//! Transfer transient startup confirmation between frontend instances, never durable input.

use super::ChatWidget;
use crate::bottom_pane::ComposerDraftSnapshot;

impl ChatWidget {
    pub(crate) fn take_startup_submission(&mut self) -> Option<ComposerDraftSnapshot> {
        self.input_queue_frontend.startup_submission.take()
    }

    pub(crate) fn restore_startup_submission(&mut self, submission: Option<ComposerDraftSnapshot>) {
        self.input_queue_frontend.startup_submission = submission;
        if self.input_queue_frontend.startup_submission.is_some() {
            self.bottom_pane.set_footer_hint_override(Some(vec![
                ("Waiting for startup".into(), String::new()),
                ("esc".into(), "cancel".into()),
            ]));
        }
    }
}
