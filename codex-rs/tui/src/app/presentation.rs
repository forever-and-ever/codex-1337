//! Admits presentation transitions without interrupting agent or server work.

use color_eyre::eyre::Result;
use ratatui::layout::Size;

use super::App;
use crate::tui;
use crate::tui::TuiEvent;

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

    /// Pays what a draw owes short of a full frame where it can: skipping what the mode withholds,
    /// deferring while output is busy, or glinting the status row, whose refusal owes content
    /// instead. A spared draw hands the geometry it sampled to the frame that pays. True when
    /// spared.
    pub(super) fn spare_full_frame(
        &self,
        tui: &mut tui::Tui,
        event: &TuiEvent,
        screen_size: Size,
    ) -> Result<bool> {
        let paint = if matches!(event, TuiEvent::Draw) {
            tui.scheduled_paint()
        } else {
            tui::ScheduledPaint::Full
        };
        if paint != tui::ScheduledPaint::Skip && Self::defer_input_frame(tui, event, screen_size) {
            return Ok(true);
        }
        match paint {
            tui::ScheduledPaint::Full => return Ok(false),
            tui::ScheduledPaint::Skip => {}
            tui::ScheduledPaint::Glint => {
                let chat_widget = &self.chat_widget;
                let glinted = tui.draw_glint(screen_size, |area, height, buf| {
                    chat_widget.paint_status_glint(area, height, buf)
                })?;
                if !glinted {
                    // The screen or the row's layout moved, which is content: it waits with
                    // any content already awaiting its deadline instead of painting it early.
                    tui.frame_requester().schedule_frame();
                }
            }
        }
        tui.defer_screen_size(screen_size);
        tui.clipboard.advance(tui.frame_requester());
        Ok(true)
    }
}
