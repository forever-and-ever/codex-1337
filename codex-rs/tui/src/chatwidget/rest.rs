//! How each turn ends, for the rest row.

use super::ChatWidget;
use crate::bottom_pane::Rest;

impl ChatWidget {
    /// Shows how the turn that just ended ended; a replayed turn leaves the thread ready, as a
    /// resumed thread is.
    pub(super) fn rest_after_turn(&mut self, rest: Rest) {
        let rest = if self.thread_usage.replaying_turn_completion {
            Rest::Ready
        } else {
            rest
        };
        self.bottom_pane.set_rest(rest);
    }
}
