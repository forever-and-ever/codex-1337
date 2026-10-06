//! Agent-wait presentation keeps the captured lifecycle separate from its transient indicator.

use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryTimestamp;
use crate::history_cell::PlainHistoryCell;
use crate::terminal_hyperlinks::HyperlinkLine;
use codex_app_server_protocol::CollabAgentToolCallStatus;
use ratatui::text::Line;

#[derive(Debug)]
pub(crate) struct AgentToolHistoryCell {
    pub(crate) timestamp: Option<HistoryTimestamp>,
    pub(crate) captured: PlainHistoryCell,
    pub(crate) wait: Option<WaitPresentation>,
}

#[derive(Debug)]
pub(crate) struct WaitPresentation {
    pub(crate) id: String,
    pub(crate) status: CollabAgentToolCallStatus,
    pub(crate) terminal_lines: Vec<Line<'static>>,
}

impl AgentToolHistoryCell {
    pub(crate) fn wait_for(cell: &dyn HistoryCell) -> Option<&WaitPresentation> {
        let cell = cell
            .as_any()
            .downcast_ref::<crate::history_cell::ToolItemCell>()
            .map_or(cell, |cell| cell.inner.as_ref());
        cell.as_any().downcast_ref::<Self>()?.wait.as_ref()
    }
}

impl HistoryCell for AgentToolHistoryCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        self.timestamp
    }

    fn tool_call_ids(&self) -> Vec<&str> {
        self.wait.iter().map(|wait| wait.id.as_str()).collect()
    }

    fn is_tool_entry(&self) -> bool {
        true
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        match &self.wait {
            Some(wait) if wait.status != CollabAgentToolCallStatus::InProgress => {
                wait.terminal_lines.clone()
            }
            _ => self.captured.display_lines(width),
        }
    }

    fn nested_summary_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        if self.wait.is_some() {
            crate::terminal_hyperlinks::plain_hyperlink_lines(self.display_lines(width))
        } else {
            self.captured.nested_summary_lines(width)
        }
    }

    fn nested_detail_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        crate::terminal_hyperlinks::plain_hyperlink_lines(self.display_lines(width))
    }

    fn has_nested_details(&self, width: u16) -> bool {
        self.wait.is_none() && self.captured.has_nested_details(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.captured.raw_lines()
    }
}
