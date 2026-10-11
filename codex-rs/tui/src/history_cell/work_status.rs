use super::HistoryCell;
use super::HistoryTimestamp;
use super::PrefixedWrappedHistoryCell;
use super::agent_prefix;
use crate::terminal_hyperlinks::HyperlinkLine;
use ratatui::style::Stylize;
use ratatui::text::Line;

#[derive(Debug)]
pub(crate) struct WorkStatusCell(PrefixedWrappedHistoryCell);

impl WorkStatusCell {
    pub(crate) fn new(activity: String, timestamp: Option<HistoryTimestamp>) -> Self {
        let mut cell = PrefixedWrappedHistoryCell::new(
            Line::from(vec!["Activity ".bold(), activity.into()]),
            Line::from(agent_prefix(/*first*/ true)),
            Line::from(agent_prefix(/*first*/ false)),
        );
        cell.timestamp = timestamp;
        Self(cell)
    }
}

impl HistoryCell for WorkStatusCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        self.0.transcript_timestamp()
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.0.display_lines(width)
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.0.display_hyperlink_lines(width)
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.0.transcript_hyperlink_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.0.raw_lines()
    }
}
