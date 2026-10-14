//! Retain reasoning and returned batches without breaking their enclosing activity group.
//!
//! Positions count preceding calls. Prepending history shifts those positions while sharing the
//! original immutable detail cells, so expanded output keeps its chronological source content.
//! Raw projection follows each cell's contract, retaining terminal input while hiding reasoning.

use super::HistoryCell;
use super::HistoryRenderMode;
use super::ToolOutputCell;
use super::output_provenance::OutputMode;
use crate::terminal_hyperlinks::HyperlinkLine;
use std::sync::Arc;

#[derive(Debug, Default, Clone)]
pub(crate) struct ActivityDetails {
    pub(super) entries: Vec<(usize, Arc<dyn HistoryCell>)>,
}

impl ActivityDetails {
    pub(crate) fn outer_call_id(&self) -> Option<&str> {
        self.entries
            .iter()
            .find_map(|(_, cell)| cell.outer_call_id())
    }

    pub(crate) fn members(&self) -> impl Iterator<Item = &dyn HistoryCell> {
        self.entries.iter().map(|(_, cell)| cell.as_ref())
    }

    pub(crate) fn push(&mut self, after_calls: usize, cell: Arc<dyn HistoryCell>) {
        if let Some(output) = cell.as_any().downcast_ref::<ToolOutputCell>()
            && self.entries.iter().any(|(_, retained)| {
                retained
                    .as_any()
                    .downcast_ref::<ToolOutputCell>()
                    .is_some_and(|old| old.id() == output.id())
            })
        {
            return;
        }
        self.entries.push((after_calls, cell));
    }

    pub(crate) fn output_ids(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|(_, cell)| cell.as_any().is::<ToolOutputCell>())
            .flat_map(|(_, cell)| cell.activity_ids())
            .collect()
    }

    /// None means there is no returned batch overriding the nested tool's result.
    pub(crate) fn returned_output_has_details(&self) -> Option<bool> {
        let mut result = None;
        for (_, cell) in &self.entries {
            if cell.as_any().is::<ToolOutputCell>() {
                *result.get_or_insert(false) |= cell.activity_disclosure(u16::MAX).is_some();
            }
        }
        result
    }

    /// The batch returned to the model replaces raw nested results in the disclosure, not in raw history.
    pub(crate) fn has_formatted_output(&self) -> bool {
        self.entries
            .iter()
            .any(|(_, cell)| cell.has_formatted_output())
    }

    pub(crate) fn output_lines(&self, width: u16, mode: OutputMode) -> Option<Vec<HyperlinkLine>> {
        let mut output = None;
        for (_, cell) in &self.entries {
            if let Some(cell) = cell.as_any().downcast_ref::<ToolOutputCell>() {
                let lines: &mut Vec<HyperlinkLine> = output.get_or_insert_default();
                if cell.activity_disclosure(width).is_none() {
                    continue;
                }
                if !lines.is_empty() {
                    lines.push("".into());
                }
                lines.extend(cell.output_lines(width, mode));
            }
        }
        output
    }

    pub(crate) fn append_result_parts(
        &self,
        parts: &mut super::activity_parts::ActivityParts,
        width: u16,
        expanded: &std::collections::HashSet<String>,
        mode: OutputMode,
    ) {
        for (_, cell) in &self.entries {
            if let Some(output) = cell.as_any().downcast_ref::<ToolOutputCell>()
                && output.activity_disclosure(width).is_some()
            {
                output.append_result_parts(parts, width, expanded, mode);
            }
        }
    }

    pub(crate) fn prepend(&mut self, mut older: Self, older_calls: usize) {
        for (position, _) in &mut self.entries {
            *position += older_calls;
        }
        older.entries.append(&mut self.entries);
        *self = older;
    }

    pub(crate) fn lines_after(
        &self,
        after_calls: usize,
        width: u16,
        mode: HistoryRenderMode,
    ) -> Vec<HyperlinkLine> {
        self.entries
            .iter()
            .filter(|(position, _)| *position == after_calls)
            .flat_map(|(_, cell)| {
                let mut lines = match mode {
                    HistoryRenderMode::Rich => cell.transcript_hyperlink_lines(width),
                    HistoryRenderMode::Raw => {
                        super::output_provenance::raw_cell_lines(cell.as_ref(), width)
                    }
                };
                if mode == HistoryRenderMode::Rich && !lines.is_empty() {
                    lines.insert(/*index*/ 0, HyperlinkLine::from(""));
                }
                lines
            })
            .collect()
    }
}
