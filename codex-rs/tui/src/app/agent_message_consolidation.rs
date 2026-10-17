//! Transcript consolidation for finalized streaming agent messages.
//!
//! During streaming, the chat widget emits transient `AgentMessageCell`s so it
//! can animate stable lines into scrollback while keeping the active mutable
//! tail in the bottom pane. Once the answer finishes, the app replaces that
//! trailing run with a single source-backed `AgentMarkdownCell`. This makes the
//! transcript the canonical owner of the raw markdown source used for future
//! resize re-renders. The retained view preserves its displayed revision before that
//! replacement, then repaints without replaying terminal scrollback.

use std::sync::Arc;

use color_eyre::eyre::Result;

use super::App;
use super::resize_reflow::trailing_run_start;
use super::transcript_splice::TranscriptSplice;
use crate::app_event::ConsolidationScrollbackReflow;
use crate::history_cell;
use crate::history_cell::HistoryCell;
use crate::pager_overlay::Overlay;
use crate::tui;

impl App {
    pub(super) fn handle_consolidate_agent_message(
        &mut self,
        tui: &mut tui::Tui,
        source: history_cell::AgentMarkdownCell,
        scrollback_reflow: ConsolidationScrollbackReflow,
        deferred_history_cell: Option<Box<dyn HistoryCell>>,
    ) -> Result<()> {
        // Some finalize paths must preserve a last provisional stream cell long
        // enough for queue ordering, then fold it into the canonical
        // source-backed cell during consolidation.
        if let Some(cell) = deferred_history_cell {
            let cell: Arc<dyn HistoryCell> = cell.into();
            if let Some(Overlay::Transcript(t)) = &mut self.overlay {
                t.insert_cell(cell.clone());
            }
            if !tui.is_owned_screen() {
                self.native_history.defer(&cell);
            }
            self.transcript_cells.push(cell);
        }

        // Walk backward to find the contiguous run of streaming AgentMessageCells that
        // belong to the just-finalized stream.
        let end = self.transcript_cells.len();
        tracing::debug!(
            "ConsolidateAgentMessage: transcript_cells.len()={end}, source_len={}",
            source.copy_source().map_or(/*default*/ 0, str::len)
        );
        let start = trailing_run_start::<history_cell::AgentMessageCell>(&self.transcript_cells);
        if start < end {
            tracing::debug!(
                "ConsolidateAgentMessage: replacing cells [{start}..{end}] with AgentMarkdownCell"
            );
            let mut consolidated = source;
            consolidated.timestamp = self.transcript_cells[start]
                .as_any()
                .downcast_ref::<history_cell::AgentMessageCell>()
                .and_then(|cell| cell.timestamp);
            let consolidated: Arc<dyn HistoryCell> = Arc::new(consolidated);
            let splice = if tui.is_owned_screen() {
                TranscriptSplice::Members {
                    range: start..end,
                    cells: vec![consolidated],
                    retained: Vec::new(),
                }
            } else {
                TranscriptSplice::Revision {
                    range: start..end,
                    cell: consolidated,
                }
            };
            self.commit_transcript_splice(tui, splice);

            self.finish_agent_message_consolidation(tui, scrollback_reflow)?;
        } else {
            tracing::debug!(
                "ConsolidateAgentMessage: no cells to consolidate(start={start}, end={end})",
            );
            self.maybe_finish_stream_reflow(tui)?;
        }

        Ok(())
    }

    pub(super) fn handle_prepared_stream(
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

    fn finish_agent_message_consolidation(
        &mut self,
        tui: &mut tui::Tui,
        scrollback_reflow: ConsolidationScrollbackReflow,
    ) -> Result<()> {
        if tui.is_owned_screen() {
            self.transcript_reflow.clear_pending_reflow();
            self.transcript_reflow.clear_stream_flags();
            tui.frame_requester().schedule_frame();
            return Ok(());
        }
        match scrollback_reflow {
            ConsolidationScrollbackReflow::IfResizeReflowRan => {
                self.maybe_finish_stream_reflow(tui)?;
            }
            ConsolidationScrollbackReflow::Required => {
                self.finish_required_stream_reflow(tui)?;
            }
        }

        Ok(())
    }
}
