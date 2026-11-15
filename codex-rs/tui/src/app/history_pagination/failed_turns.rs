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
        cells: &mut Vec<Arc<dyn HistoryCell>>,
    ) {
        // A newer page can supply a failure's source before older failures'
        // sources arrive. Carry those retained metadata witnesses with the prefix
        // rather than leapfrogging them; their own source page will reseat them.
        let projected = cells
            .iter()
            .enumerate()
            .filter_map(|(index, cell)| {
                cell.as_any()
                    .downcast_ref::<history_cell::FailedTurnCell>()
                    .map(|failure| (failure.turn_id.clone(), index))
            })
            .collect::<std::collections::HashMap<_, _>>();
        let mut insertions = Vec::new();
        let mut removed = Vec::new();
        if let Some(boundary) = self.transcript_cells.iter().rposition(|cell| {
            cell.as_any()
                .downcast_ref::<history_cell::FailedTurnCell>()
                .is_some_and(|failure| projected.contains_key(&failure.turn_id))
        }) {
            let mut insertion = 0;
            for (index, cell) in self.transcript_cells[..boundary].iter().enumerate() {
                if let Some(failure) = cell.as_any().downcast_ref::<history_cell::FailedTurnCell>()
                {
                    if let Some(position) = projected.get(&failure.turn_id) {
                        insertion = position + 1;
                    } else {
                        insertions.push((insertion, Arc::clone(cell)));
                        removed.push(index);
                    }
                }
            }
        }
        for cell in cells.iter_mut() {
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
        insertions.sort_by_key(|(index, _)| *index);
        for (index, cell) in insertions.into_iter().rev() {
            cells.insert(index, cell);
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
