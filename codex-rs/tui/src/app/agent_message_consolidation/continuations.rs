//! Replace the response marker only after its complete block has been prepared.

use super::App;
use super::ConsolidationScrollbackReflow;
use super::Result;
use super::TranscriptSplice;
use super::history_cell;
use super::tui;

impl App {
    pub(in crate::app) fn handle_prepared_stream(
        &mut self,
        tui: &mut tui::Tui,
        prepared: &crate::streaming::completion::PreparedStream,
    ) -> Result<()> {
        if self.active_thread_id != Some(prepared.thread_id) {
            if self
                .side_companions
                .get(&prepared.thread_id)
                .is_some_and(|companion| companion.owns_preparation(prepared))
            {
                self.publish_side_cells(tui, prepared.thread_id);
            }
            return Ok(());
        }
        let Some(response) = self.chat_widget.take_prepared_stream(prepared) else {
            return Ok(());
        };
        let Some(start) = self.transcript_cells.iter().position(|cell| {
            cell.as_any()
                .downcast_ref::<history_cell::RespondingCell>()
                .is_some_and(|response| response.item_id == prepared.item_id)
        }) else {
            tracing::trace!(
                item_id = prepared.item_id,
                "dropping prepared response without its marker"
            );
            self.chat_widget.note_stream_consolidation_completed();
            return Ok(());
        };
        let latest_reply = !self.transcript_cells[start + 1..].iter().any(|cell| {
            history_cell::streaming_item_id(cell.as_ref()).is_some()
                || cell.as_any().is::<history_cell::AgentMarkdownCell>()
                || cell.as_any().is::<history_cell::ProposedPlanCell>()
        });
        self.commit_transcript_splice(
            tui,
            TranscriptSplice::Members {
                range: start..start + 1,
                cells: vec![response.cell],
                retained: Vec::new(),
            },
        );
        self.finish_agent_message_consolidation(tui, ConsolidationScrollbackReflow::Required)?;
        self.chat_widget.note_stream_consolidation_completed();
        if latest_reply && let Some(markdown) = response.assistant {
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
