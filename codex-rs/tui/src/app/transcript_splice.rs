use super::*;
use std::ops::Range;

pub(super) enum SpliceShape {
    Revision,
    Members(Vec<Arc<dyn HistoryCell>>),
}

pub(super) struct TranscriptSplice {
    pub(super) range: Range<usize>,
    pub(super) cells: Vec<Arc<dyn HistoryCell>>,
    pub(super) shape: SpliceShape,
}

impl App {
    pub(super) fn commit_transcript_splice(
        &mut self,
        tui: &mut tui::Tui,
        splice: TranscriptSplice,
    ) {
        let retained = match &splice.shape {
            SpliceShape::Revision => &[][..],
            SpliceShape::Members(retained) => retained.as_slice(),
        };
        let previous = self.transcript_cells[splice.range.clone()]
            .iter()
            .filter(|old| !retained.iter().any(|cell| Arc::ptr_eq(old, cell)))
            .cloned()
            .collect::<Vec<_>>();
        if let Some(replacement) = splice
            .cells
            .iter()
            .find(|new| !retained.iter().any(|cell| Arc::ptr_eq(new, cell)))
        {
            self.native_history.consolidate(&previous, replacement);
        } else {
            self.native_history.discard(&previous);
        }
        match splice.shape {
            SpliceShape::Revision => {
                let replacement = &splice.cells[0];
                self.transcript_view.replace_range(
                    &self.transcript_cells,
                    splice.range.clone(),
                    replacement,
                );
                if let Some(Overlay::Transcript(overlay)) = self.overlay.as_mut() {
                    overlay.regroup_cell_members(splice.range.clone(), &splice.cells);
                }
            }
            SpliceShape::Members(_) => {
                self.transcript_view.replace_group_members(
                    &self.transcript_cells,
                    splice.range.clone(),
                    &splice.cells,
                );
                if let Some(Overlay::Transcript(overlay)) = self.overlay.as_mut() {
                    overlay.regroup_cell_members(splice.range.clone(), &splice.cells);
                }
            }
        }
        self.transcript_cells.splice(splice.range, splice.cells);
        tui.frame_requester().schedule_frame();
        self.chat_widget.request_pending_usage_output_insertion();
    }

    /// Seats a step's Thought ahead of the step's members once the step's bill names them.
    pub(super) fn seat_thought(&mut self, tui: &mut tui::Tui, response_id: &str) {
        let Some(range) = self
            .transcript_view
            .misplaced_thought(&self.transcript_cells, response_id)
        else {
            return;
        };
        let mut cells = self.transcript_cells[range.clone()].to_vec();
        cells.rotate_right(/*k*/ 1);
        self.commit_transcript_splice(
            tui,
            TranscriptSplice {
                range,
                shape: SpliceShape::Members(cells.clone()),
                cells,
            },
        );
    }
}
