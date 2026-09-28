//! Admits presentation transitions without interrupting agent or server work.

use color_eyre::eyre::Result;

use super::App;
use crate::tui;

impl App {
    pub(super) fn apply_presentation_transition(
        &mut self,
        tui: &mut tui::Tui,
        previous: tui::PresentationMode,
    ) -> Result<()> {
        let current = tui.presentation_mode();
        if current == previous {
            return Ok(());
        }
        if current != tui::PresentationMode::Normal {
            self.commit_animation = None;
            self.chat_widget.terminal_title_next_refresh = None;
            self.chat_widget.drain_unpaced_commit_tick();
        }
        if current != tui::PresentationMode::Frozen {
            self.chat_widget.refresh_terminal_title();
        }
        if previous == tui::PresentationMode::Frozen && current != tui::PresentationMode::Frozen {
            let size = tui.terminal.size()?;
            tui.defer_screen_size(size);
        }
        Ok(())
    }
}
