//! Expose live activity identity and local detail rendering to the owned transcript.
//! Canonical cells retain all available data; presentation does not modify tool execution.

use super::*;
use crate::history_cell::activity_parts::FoldState;
use crate::transcript_view::ActivityTranscriptLines;

mod live_snapshot;
pub(super) use live_snapshot::LiveSnapshot;

impl ChatWidget {
    pub(crate) fn active_activity_cell(&self) -> Option<&dyn HistoryCell> {
        self.transcript.active_cell.as_deref()
    }

    pub(crate) fn active_activity_ids(&self) -> Vec<String> {
        self.active_activity_cell()
            .map_or_else(Vec::new, HistoryCell::activity_ids)
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
        expanded_parts: &FoldState,
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
