use super::App;
use super::Arc;
use super::HistoryCell;
use super::Overlay;
use super::tui;
use std::ops::Range;

pub(super) enum TranscriptSplice {
    Companion {
        thread_id: super::ThreadId,
        range: Range<usize>,
        cells: Vec<Arc<dyn HistoryCell>>,
    },
    Revision {
        range: Range<usize>,
        cell: Arc<dyn HistoryCell>,
    },
    Members {
        range: Range<usize>,
        cells: Vec<Arc<dyn HistoryCell>>,
        retained: Vec<Arc<dyn HistoryCell>>,
    },
    AbsorbTail {
        previous_revision: u64,
        hydrated_revision: u64,
    },
}

impl App {
    pub(super) fn commit_transcript_splice(
        &mut self,
        tui: &mut tui::Tui,
        splice: TranscriptSplice,
    ) {
        match splice {
            TranscriptSplice::Companion {
                thread_id,
                range,
                cells,
            } => {
                if let Some(companion) = self.side_companions.get_mut(&thread_id) {
                    companion
                        .view
                        .replace_group_members(&companion.cells, range.clone(), &cells);
                    companion.cells.splice(range, cells);
                }
            }
            TranscriptSplice::Revision { range, cell } => {
                self.native_history
                    .consolidate(&self.transcript_cells[range.clone()], &cell);
                self.transcript_view
                    .replace_range(&self.transcript_cells, range.clone(), &cell);
                if let Some(Overlay::Transcript(overlay)) = self.overlay.as_mut() {
                    overlay.regroup_cell_members(range.clone(), std::slice::from_ref(&cell));
                }
                self.transcript_cells.splice(range, [cell]);
            }
            TranscriptSplice::Members {
                range,
                cells,
                retained,
            } => {
                let previous = self.transcript_cells[range.clone()]
                    .iter()
                    .filter(|old| !retained.iter().any(|cell| Arc::ptr_eq(old, cell)))
                    .cloned()
                    .collect::<Vec<_>>();
                if let Some(replacement) = cells
                    .iter()
                    .find(|new| !retained.iter().any(|cell| Arc::ptr_eq(new, cell)))
                {
                    self.native_history.consolidate(&previous, replacement);
                } else {
                    self.native_history.discard(&previous);
                }
                self.transcript_view.replace_group_members(
                    &self.transcript_cells,
                    range.clone(),
                    &cells,
                );
                if let Some(Overlay::Transcript(overlay)) = self.overlay.as_mut() {
                    overlay.regroup_cell_members(range.clone(), &cells);
                }
                self.transcript_cells.splice(range, cells);
            }
            TranscriptSplice::AbsorbTail {
                previous_revision,
                hydrated_revision,
            } => {
                self.transcript_view.absorb_tail_into_live(
                    &self.transcript_cells,
                    previous_revision,
                    hydrated_revision,
                );
                if let Some(Overlay::Transcript(overlay)) = self.overlay.as_mut() {
                    overlay.absorb_tail_into_live(previous_revision, hydrated_revision);
                }
                self.transcript_cells.pop();
                self.native_history.retain(&self.transcript_cells);
            }
        }
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
            TranscriptSplice::Members {
                range,
                retained: cells.clone(),
                cells,
            },
        );
    }
}
