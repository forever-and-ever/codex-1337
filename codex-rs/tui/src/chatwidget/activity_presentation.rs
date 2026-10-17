//! Expose live activity identity and local detail rendering to the owned transcript.
//! Canonical cells retain all available data; presentation does not modify tool execution.

use super::*;
use crate::transcript_view::ActivityTranscriptLines;

// Release compatibility until a new upstream active-cell type gains a clone seam.
struct RenderedLiveSnapshot {
    compact: Vec<HyperlinkLine>,
    transcript: Vec<HyperlinkLine>,
    raw: Vec<Line<'static>>,
    timestamp: Option<crate::history_cell::HistoryTimestamp>,
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
    fn transcript_timestamp(&self) -> Option<crate::history_cell::HistoryTimestamp> {
        self.timestamp
    }
    fn is_tool_entry(&self) -> bool {
        self.tool
    }
    fn is_stream_continuation(&self) -> bool {
        self.continuation
    }
}

pub(super) trait LiveSnapshot {
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
            history_cell::WebSearchCell,
            history_cell::McpToolCallCell,
            history_cell::McpInventoryLoadingCell,
            history_cell::ComputerActivityCell,
            history_cell::PlainHistoryCell,
            history_cell::UserHistoryCell,
            history_cell::AgentMarkdownCell,
            history_cell::AgentMessageCell,
            history_cell::CodeModeCell,
            history_cell::SessionHeaderHistoryCell,
            crate::exec_cell::ExecCell,
        );
        if let Some(cell) = self
            .as_any()
            .downcast_ref::<super::realtime_split_flap::SplitFlapTranscriptCell>()
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
            )
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

    pub(crate) fn active_activity_ids(&self) -> Vec<String> {
        self.transcript
            .active_cell
            .as_ref()
            .map_or_else(Vec::new, |cell| cell.activity_ids())
    }

    pub(crate) fn active_invocation_cell(&self) -> Option<&dyn HistoryCell> {
        self.transcript.active_cell.as_deref()
    }

    pub(crate) fn active_auxiliary_transcript_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let active = self.transcript.active_cell.as_deref();
        let mode = self.history_render_mode();
        self.active_cell_hyperlink_lines_with(width, |cell, width| {
            if active.is_some_and(|active| std::ptr::eq(active, cell)) {
                Vec::new()
            } else {
                let lines = if cell.as_any().is::<history_cell::SessionInfoCell>() {
                    history_cell::fullscreen_session_lines(cell, width, false, mode)
                } else if mode == HistoryRenderMode::Raw {
                    cell.display_hyperlink_lines_for_mode(width, mode)
                } else {
                    cell.compact_hyperlink_lines(width)
                };
                crate::transcript_geometry::clock_lines(lines, cell.transcript_timestamp())
            }
        })
        .unwrap_or_default()
    }

    /// Render only the chosen activity presentation, before any auxiliary live text.
    pub(crate) fn active_cell_owned_transcript_lines(
        &self,
        width: u16,
        expanded: bool,
        expanded_parts: &std::collections::HashSet<String>,
        tool_output_tokens: Option<u64>,
        output_mode: history_cell::output_provenance::OutputMode,
    ) -> Option<ActivityTranscriptLines> {
        let active = self.transcript.active_cell.as_deref();
        let mode = self.history_render_mode();
        let render_compact = |cell: &dyn HistoryCell, width| {
            let lines = if cell.as_any().is::<history_cell::SessionInfoCell>() {
                history_cell::fullscreen_session_lines(cell, width, /*detailed*/ false, mode)
            } else if mode == HistoryRenderMode::Raw {
                cell.display_hyperlink_lines_for_mode(width, mode)
            } else {
                cell.compact_hyperlink_lines(width)
            };
            crate::transcript_geometry::clock_lines(lines, cell.transcript_timestamp())
        };
        let has_activity = mode == HistoryRenderMode::Rich
            && active.is_some_and(|cell| !cell.activity_ids().is_empty());
        let parts = active.filter(|_| has_activity).and_then(|cell| {
            cell.output_parts(width, /*reserve*/ 0, expanded_parts, output_mode)
        });
        let (activity, header, disclosure, controls) = if let Some(parts) = parts {
            (
                crate::transcript_geometry::clock_lines(
                    parts.lines,
                    active.and_then(HistoryCell::transcript_timestamp),
                ),
                parts.header,
                parts.output,
                parts.controls,
            )
        } else {
            let activity = active.map_or_else(Vec::new, |cell| {
                if expanded && has_activity {
                    crate::transcript_geometry::clock_lines(
                        cell.expanded_output_lines(width, output_mode),
                        cell.transcript_timestamp(),
                    )
                } else {
                    render_compact(cell, width)
                }
            });
            let disclosure = active
                .filter(|_| has_activity)
                .and_then(|cell| cell.activity_disclosure(width));
            (
                activity,
                active.and_then(|cell| {
                    if expanded && has_activity {
                        cell.expanded_activity_header()
                    } else {
                        cell.activity_header()
                    }
                }),
                disclosure,
                Vec::new(),
            )
        };
        let mut auxiliary = self.active_auxiliary_transcript_lines(width);
        if activity.is_empty() && auxiliary.is_empty() {
            return None;
        }
        if !activity.is_empty() && !auxiliary.is_empty() {
            auxiliary.insert(/*index*/ 0, HyperlinkLine::from(""));
        }
        Some(ActivityTranscriptLines {
            output_mode: (expanded && active.is_some_and(HistoryCell::has_formatted_output))
                .then_some(output_mode),
            is_tool_entry: active.is_some_and(HistoryCell::is_tool_entry),

            activity,
            header,
            auxiliary,
            header_tokens: active
                .and_then(HistoryCell::activity_header_tokens)
                .or(tool_output_tokens),
            disclosure,
            controls,
        })
    }
}
