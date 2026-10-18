//! Apply prepared stream replacements without rebuilding their layout on the UI task.

use super::App;
use super::Arc;
use super::ConsolidationScrollbackReflow;
use super::HistoryCell;
use super::Result;
use super::TranscriptSplice;
use super::history_cell;
use super::tui;

impl App {
    #[expect(
        clippy::expect_used,
        reason = "stream admission retains the boundary until its prepared replacement arrives"
    )]
    pub(in crate::app) fn handle_prepared_stream(
        &mut self,
        tui: &mut tui::Tui,
        prepared: crate::streaming::completion::PreparedStream,
    ) -> Result<()> {
        use crate::streaming::completion::RevisionKind;
        if self.active_thread_id != Some(prepared.thread_id) {
            tracing::trace!(thread_id = %prepared.thread_id, item_id = prepared.item_id, "dropping prepared stream for inactive origin; selection replays canonical thread events");
            return Ok(());
        }
        let matches = |cell: &Arc<dyn HistoryCell>| {
            history_cell::streaming_item_id(cell.as_ref()) == Some(prepared.item_id.as_str())
        };
        let Some(start) = self.transcript_cells.iter().position(matches) else {
            tracing::trace!(
                item_id = prepared.item_id,
                "dropping prepared stream without matching run"
            );
            if matches!(&prepared.kind, RevisionKind::Final(_)) {
                self.chat_widget.note_stream_consolidation_completed();
            }
            return Ok(());
        };
        let end = self
            .transcript_cells
            .iter()
            .rposition(matches)
            .expect("matching first cell")
            + 1;
        let mut retained: Vec<_> = self.transcript_cells[start..end]
            .iter()
            .filter(|cell| !matches(cell))
            .cloned()
            .collect();
        let mut cells: Vec<_> = if matches!(&prepared.kind, RevisionKind::Rewrap) {
            self.transcript_cells[start..end]
                .iter()
                .filter(|cell| {
                    matches(cell) && cell.as_any().is::<history_cell::StreamAnchorCell>()
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let latest_reply = !self.transcript_cells[start..].iter().any(|cell| {
            !matches(cell)
                && (history_cell::streaming_item_id(cell.as_ref()).is_some()
                    || cell.as_any().is::<history_cell::AgentMarkdownCell>()
                    || cell.as_any().is::<history_cell::ProposedPlanCell>())
        });
        let anchors = cells.clone();
        cells.push(prepared.cell);
        cells.extend(retained.iter().cloned());
        retained.extend(anchors);
        self.commit_transcript_splice(
            tui,
            TranscriptSplice::Members {
                range: start..end,
                cells,
                retained,
            },
        );
        self.finish_agent_message_consolidation(tui, ConsolidationScrollbackReflow::Required)?;
        if matches!(&prepared.kind, RevisionKind::Final(_)) {
            self.chat_widget.note_stream_consolidation_completed();
        }
        if latest_reply && let RevisionKind::Final(Some(markdown)) = prepared.kind {
            self.chat_widget.accept_prepared_markdown(
                prepared.thread_id,
                &prepared.item_id,
                markdown,
            );
        }
        self.insert_pending_usage_output_after_stream_shutdown(tui);
        Ok(())
    }
}
