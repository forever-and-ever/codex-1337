use super::transcript_splice::TranscriptSplice;
use super::*;

fn source_item_id(cell: &dyn HistoryCell) -> Option<&str> {
    let cell = cell.as_any();
    if let Some(cell) = cell.downcast_ref::<history_cell::AgentMessageCell>() {
        cell.item_id.as_deref()
    } else if let Some(cell) = cell.downcast_ref::<history_cell::ProposedPlanStreamCell>() {
        cell.item_id.as_deref()
    } else {
        cell.downcast_ref::<history_cell::AgentMarkdownCell>()
            .and_then(|cell| cell.item_id.as_deref())
    }
}

impl App {
    pub(super) fn discard_stream_items(&mut self, tui: &mut tui::Tui, item_ids: &[String]) {
        let discarded = |cell: &Arc<dyn HistoryCell>| {
            source_item_id(cell.as_ref()).is_some_and(|id| item_ids.iter().any(|item| item == id))
        };
        let Some(start) = self.transcript_cells.iter().position(&discarded) else {
            return;
        };
        let end = self
            .transcript_cells
            .iter()
            .rposition(&discarded)
            .expect("located stream item")
            + 1;
        let retained: Vec<_> = self.transcript_cells[start..end]
            .iter()
            .filter(|cell| !discarded(cell))
            .cloned()
            .collect();
        self.commit_transcript_splice(
            tui,
            TranscriptSplice::Members {
                range: start..end,
                cells: retained.clone(),
                retained,
            },
        );
    }
}
