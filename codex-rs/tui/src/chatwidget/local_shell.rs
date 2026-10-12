use super::ChatWidget;
use crate::local_shell::LocalShellAdmission;
use ratatui::text::Line;
use std::time::Duration;

impl ChatWidget {
    pub(crate) fn set_local_shell_context(
        &mut self,
        admission: LocalShellAdmission,
        notice: Option<String>,
    ) {
        self.bottom_pane.set_local_shell_context(admission, notice);
    }

    pub(crate) fn restore_local_shell_command(&mut self, command: &str) -> bool {
        if !self.composer_is_empty() {
            return false;
        }
        self.bottom_pane
            .set_composer_text(format!("!! {command}"), Vec::new(), Vec::new());
        true
    }

    pub(crate) fn flash_local_shell_error(&mut self, error: String) {
        self.bottom_pane
            .show_footer_flash(Line::from(error), Duration::from_secs(5));
    }
}
