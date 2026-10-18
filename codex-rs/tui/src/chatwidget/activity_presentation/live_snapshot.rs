//! Owned snapshots cross the UI preparation boundary.

use super::*;

// Release compatibility until a new upstream active-cell type gains a clone seam.
struct RenderedLiveSnapshot {
    compact: Vec<HyperlinkLine>,
    transcript: Vec<HyperlinkLine>,
    raw: Vec<Line<'static>>,
    timestamp: Option<history_cell::HistoryTimestamp>,
    tool: bool,
    continuation: bool,
}

impl std::fmt::Debug for RenderedLiveSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RenderedLiveSnapshot")
    }
}

impl HistoryCell for RenderedLiveSnapshot {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.compact.iter().map(|line| line.line.clone()).collect()
    }
    fn display_hyperlink_lines(&self, _width: u16) -> Vec<HyperlinkLine> {
        self.compact.clone()
    }
    fn transcript_hyperlink_lines(&self, _width: u16) -> Vec<HyperlinkLine> {
        self.transcript.clone()
    }
    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.raw.clone()
    }
    fn transcript_timestamp(&self) -> Option<history_cell::HistoryTimestamp> {
        self.timestamp
    }
    fn is_tool_entry(&self) -> bool {
        self.tool
    }
    fn is_stream_continuation(&self) -> bool {
        self.continuation
    }
}

pub(in crate::chatwidget) trait LiveSnapshot {
    fn snapshot(&self, width: u16) -> Box<dyn HistoryCell>;
}

impl LiveSnapshot for dyn HistoryCell {
    fn snapshot(&self, width: u16) -> Box<dyn HistoryCell> {
        macro_rules! snapshots {
            ($($cell:ty),+ $(,)?) => {
                $(if let Some(cell) = self.as_any().downcast_ref::<$cell>() {
                    return Box::new(cell.clone());
                })+
            };
        }
        snapshots!(
            history_cell::StreamingAgentTailCell,
            history_cell::StreamingPlanTailCell,
            history_cell::ThreadRecapLoadingCell,
            WebSearchCell,
            McpToolCallCell,
            history_cell::McpInventoryLoadingCell,
            history_cell::ComputerActivityCell,
            PlainHistoryCell,
            history_cell::UserHistoryCell,
            history_cell::AgentMarkdownCell,
            history_cell::AgentMessageCell,
            history_cell::CodeModeCell,
            history_cell::SessionHeaderHistoryCell,
            ExecCell,
        );
        if let Some(cell) = self
            .as_any()
            .downcast_ref::<realtime_split_flap::SplitFlapTranscriptCell>()
        {
            return Box::new(cell.snapshot(width));
        }
        if let Some(cell) = self.as_any().downcast_ref::<history_cell::ToolInputCell>() {
            return Box::new(history_cell::ToolInputCell(cell.0.clone()));
        }
        debug_assert!(
            false,
            "live transcript cell has no owned snapshot: {:?}",
            self.type_id()
        );
        static WARN: std::sync::Once = std::sync::Once::new();
        WARN.call_once(|| {
            tracing::warn!(
                "unknown live transcript cell: rendering an inline compatibility snapshot"
            );
        });
        Box::new(RenderedLiveSnapshot {
            compact: self.compact_hyperlink_lines(width),
            transcript: self.transcript_hyperlink_lines(width),
            raw: self.live_raw_lines(),
            timestamp: self.transcript_timestamp(),
            tool: self.is_tool_entry(),
            continuation: self.is_stream_continuation(),
        })
    }
}

impl ChatWidget {
    pub(crate) fn active_transcript_snapshot(
        &self,
        width: u16,
    ) -> crate::transcript_view::OwnedLiveTail {
        let width = crate::transcript_geometry::content_width(width, self.history_render_mode());
        crate::transcript_view::OwnedLiveTail {
            active: self
                .transcript
                .active_cell
                .as_deref()
                .map(|cell| Arc::from(cell.snapshot(width))),
            auxiliary: self
                .realtime_conversation
                .pending_history_cells
                .iter()
                .chain(self.realtime_conversation.live_transcript_cells())
                .map(|cell| Arc::from(cell.as_ref().snapshot(width)))
                .chain(
                    self.pending_rate_limit_reset_hint()
                        .map(|cell| Arc::new(cell.clone()) as Arc<dyn HistoryCell>),
                )
                .collect(),
        }
    }
}
