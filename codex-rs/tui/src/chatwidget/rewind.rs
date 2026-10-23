//! Rewind's stand-in for the composer; `app/rewind.rs` owns the selection.

use super::ChatWidget;

impl ChatWidget {
    /// Replace the composer with the rewind view.
    pub(crate) fn show_rewind(&mut self) {
        self.bottom_pane.show_rewind();
    }

    pub(crate) fn dismiss_rewind(&mut self) {
        self.bottom_pane.dismiss_rewind();
    }

    pub(crate) fn shows_rewind(&self) -> bool {
        self.bottom_pane.shows_rewind()
    }

    /// Rewind waits for the same running task that upstream's Esc-Esc backtracking waited for.
    pub(crate) fn is_task_running(&self) -> bool {
        self.bottom_pane.is_task_running()
    }
}
