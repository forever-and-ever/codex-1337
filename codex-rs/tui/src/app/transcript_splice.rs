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
}
