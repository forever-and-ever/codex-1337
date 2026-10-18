//! Preserve failed-turn cells while replacing a paginated history window.

use super::App;
use super::Arc;
use super::HistoryCell;
use super::TranscriptSplice;
use super::history_cell;
use super::tui;

impl App {
    /// Move metadata witnesses with their source turn, never with a later page's tools.
    #[expect(
        clippy::expect_used,
        reason = "The search predicate already proved that the cell is a FailedTurnCell."
    )]
    pub(super) fn reseat_history_failures(
        &mut self,
        tui: &mut tui::Tui,
        cells: &mut [Arc<dyn HistoryCell>],
    ) {
        let mut removed = Vec::new();
        for cell in cells {
            let Some(failure) = cell.as_any().downcast_ref::<history_cell::FailedTurnCell>() else {
                continue;
            };
            if let Some((index, existing)) =
                self.transcript_cells
                    .iter()
                    .enumerate()
                    .find(|(_, existing)| {
                        existing
                            .as_any()
                            .downcast_ref::<history_cell::FailedTurnCell>()
                            .is_some_and(|existing| existing.turn_id == failure.turn_id)
                    })
            {
                let existing_failure = existing
                    .as_any()
                    .downcast_ref::<history_cell::FailedTurnCell>()
                    .expect("matched failure cell");
                if existing_failure.completed_at == failure.completed_at
                    && existing_failure.error == failure.error
                {
                    *cell = Arc::clone(existing);
                }
                removed.push(index);
            }
        }
        removed.sort_unstable();
        removed.dedup();
        for index in removed.into_iter().rev() {
            self.commit_transcript_splice(
                tui,
                TranscriptSplice::Members {
                    range: index..index + 1,
                    cells: Vec::new(),
                    retained: Vec::new(),
                },
            );
        }
    }
}
