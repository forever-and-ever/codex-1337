use std::sync::Arc;

use super::TranscriptView;
use crate::history_cell::HistoryCell;

impl TranscriptView {
    pub(super) fn near_prepared_start(&mut self, cells: &[Arc<dyn HistoryCell>]) -> bool {
        if self.cache.waiting || (self.area.is_empty() && self.is_following()) {
            return false;
        }
        let (index, row) = self.start(cells);
        // Finding the viewport start can itself encounter an unfinished layout.
        if self.cache.waiting {
            return false;
        }
        let threshold = usize::from(self.area.height);
        let mut distance = row;
        if distance > threshold {
            return false;
        }
        for previous in (0..index).rev() {
            // Unknown height is not zero: wait for preparation instead of fetching
            // older pages and preparing the entire retained transcript on every frame.
            let Some(layout) = self.layout(cells, previous) else {
                return false;
            };
            distance = distance.saturating_add(layout.row_count());
            if distance > threshold {
                return false;
            }
        }
        true
    }
}
