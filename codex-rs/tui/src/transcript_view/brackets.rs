//! Where a bracket's stroke runs on between entries: a streamed reply arrives as one entry per
//! fragment, and its words hang from one bracket.

use std::sync::Arc;

use super::text::Joins;
use super::text::TextLayout;
use crate::history_cell::HistoryCell;
use crate::warpaint::Bracket;

impl super::TranscriptView {
    /// The brackets the stroke of the entry at `index`, laid out as `layout`, runs on into across
    /// its edges. A fragment takes the stroke of the stream above it on trust, so drawing it never
    /// prepares the entries above it.
    pub(super) fn joins(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
        layout: &TextLayout,
    ) -> Option<Joins> {
        let above = (index > 0 && self.continues_stream(cells, index))
            .then(|| layout.top_bracket())
            .flatten();
        let below = self.bracket_below(cells, index);
        (above.is_some() || below.is_some()).then_some(Joins { above, below })
    }

    /// The speaker of the first words the stream below the entry at `index` goes on with, through
    /// blank fragments of that speaker.
    fn bracket_below(&mut self, cells: &[Arc<dyn HistoryCell>], index: usize) -> Option<Bracket> {
        let mut upper = index;
        let mut through = None;
        loop {
            let lower = self.next_nonempty(cells, upper + 1)?;
            if !self.continues_stream(cells, lower) {
                return None;
            }
            let lower_layout = self.layout(cells, lower)?;
            let blank = match lower_layout.first_bracket() {
                Some(bracket) => {
                    return through
                        .is_none_or(|through| through == bracket)
                        .then_some(bracket);
                }
                None => lower_layout.blank_bracket()?,
            };
            if through.is_some_and(|through| through != blank) {
                return None;
            }
            through = Some(blank);
            upper = lower;
        }
    }

    /// Whether the entry at `index` continues the stream drawn above it; the live tail is `cells.len()`.
    fn continues_stream(&self, cells: &[Arc<dyn HistoryCell>], index: usize) -> bool {
        cells.get(index).map_or(
            index == cells.len() && self.live.is_some() && self.live_continuation,
            |cell| cell.is_stream_continuation(),
        )
    }
}
