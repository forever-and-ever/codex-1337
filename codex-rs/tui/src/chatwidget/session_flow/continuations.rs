//! Prompt-revert notices projected after deferred history preparation.

use super::ChatWidget;

impl ChatWidget {
    pub(crate) fn project_prompt_revert_notice(&mut self, notice: &str) {
        if self.bottom_pane.composer_input_enabled() {
            self.set_prompt_revert_notice(Some(notice));
        }
    }

    pub(crate) fn set_prompt_revert_notice(&mut self, notice: Option<&str>) {
        self.set_queue_autosend_suppressed(notice.is_some());
        let enabled = notice.is_none()
            && !self.blocks_direct_input
            && !self.has_misalignment_policy_violation();
        self.bottom_pane
            .set_composer_input_enabled(enabled, notice.map(str::to_string));
        self.request_redraw();
    }
}
