use super::ChatWidget;
use ratatui::text::Line;
use std::time::Duration;

impl ChatWidget {
    pub(crate) fn set_local_shell_notice(&mut self, notice: Option<String>) {
        self.bottom_pane.set_local_shell_notice(notice);
    }

    pub(crate) fn restore_local_shell_command(&mut self, command: &str) {
        if self.composer_text_with_pending().is_empty() {
            self.bottom_pane
                .set_composer_text(format!("!! {command}"), Vec::new(), Vec::new());
        } else {
            self.flash_local_shell_error(
                "Local command was not started; another draft is already being edited".to_owned(),
            );
        }
    }

    pub(crate) fn flash_local_shell_error(&mut self, error: String) {
        self.bottom_pane
            .show_footer_flash(Line::from(error), Duration::from_secs(5));
    }
}
