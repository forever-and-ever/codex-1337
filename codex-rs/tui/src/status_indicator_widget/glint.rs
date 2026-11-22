//! The status row asks for the glints that advance it.

use std::time::Duration;

use super::StatusIndicator;

impl StatusIndicator<'_> {
    /// Asks for the next glint: on the next beat while the row moves, a second on while only its
    /// clock does.
    pub(super) fn schedule_glint(&self) {
        let row = self.row;
        if !row.animations_enabled && self.timer.display_started_at.is_none() {
            return;
        }
        let moving = row.animations_enabled
            && (row.effects.progress
                || (row.effects.shimmer
                    && !crate::text_formatting::contains_rtl_or_arabic_number(&row.header)));
        let after = if moving {
            Duration::ZERO
        } else {
            Duration::from_secs(/*secs*/ 1)
        };
        row.frame_requester.schedule_glint_in(after);
    }
}
