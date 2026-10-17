use super::ChatComposer;
use super::InputResult;
use crate::key_hint::KeyBindingListExt;
use crate::local_shell::LocalShellAdmission;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::style::Stylize;
use ratatui::text::Line;
use std::time::Duration;

impl ChatComposer {
    pub(super) fn is_local_shell_input(&self) -> bool {
        self.config.shell_commands_enabled && self.current_text().trim_start().starts_with("!!")
    }

    pub(super) fn try_local_shell_submission(&mut self) -> Option<InputResult> {
        if !self.is_local_shell_input() {
            return None;
        }
        if !self.attachments.is_empty() {
            self.show_footer_flash(
                Line::from("Local shell does not accept attachments").red(),
                Duration::from_secs(5),
            );
            return Some(InputResult::None);
        }
        // Bare !! never needs admission. Every run claims the capability before the
        // destructive prepare, including while its App event is still in flight.
        let lease = if self.current_text().trim() == "!!" {
            None
        } else {
            let Some(lease) = self
                .local_shell_admission
                .as_ref()
                .and_then(crate::local_shell::LocalShellAdmission::try_acquire)
            else {
                self.show_footer_flash(
                    Line::from("Local shell occupied; draft unchanged · !! view/restore"),
                    Duration::from_secs(5),
                );
                return Some(InputResult::None);
            };
            Some(lease)
        };
        Some(
            match self.prepare_submission_text(/*record_history*/ false) {
                Some((text, _)) => crate::local_shell::LocalShellRequest::parse(&text, lease)
                    .map(InputResult::LocalShell)
                    .unwrap_or(InputResult::None),
                None => InputResult::None,
            },
        )
    }

    pub(crate) fn set_local_shell_context(
        &mut self,
        admission: LocalShellAdmission,
        notice: Option<String>,
    ) {
        self.local_shell_admission = Some(admission);
        self.footer.local_shell_notice = notice.map(Line::from);
    }

    pub(super) fn local_shell_footer_line(&self) -> Option<Line<'static>> {
        self.is_local_shell_input()
            .then(|| Line::from("Local shell · TUI host · not sent to agent"))
    }

    pub(super) fn try_restricted_local_shell_submission(
        &mut self,
        key: KeyEvent,
    ) -> Option<InputResult> {
        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && (self.submit_keys.is_pressed(key) || self.queue_keys.is_pressed(key))
        {
            self.try_local_shell_submission()
        } else {
            None
        }
    }
}
