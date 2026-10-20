//! Attributed cross-thread messages are tool output, never user prompts.

use super::HistoryCell;
use super::HistoryTimestamp;
use super::Line;
use super::RtOptions;
use super::Stylize;
use super::adaptive_wrap_lines;
use super::prefix_lines;
use super::sanitize_user_text;
use crate::dynamic_tools::DelegatedMessage;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::plain_hyperlink_lines;

#[derive(Debug)]
pub(crate) struct DelegatedMessageCell(pub(crate) DelegatedMessage);

impl HistoryCell for DelegatedMessageCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        self.0.sent_at.and_then(HistoryTimestamp::from_millis)
    }
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.rows(width).1
    }

    /// The message's words hang from the message bracket; its heading is the client's.
    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let (heading, rows) = self.rows(width);
        let mut lines = plain_hyperlink_lines(rows);
        super::output_provenance::hang(&mut lines[heading..], crate::warpaint::Bracket::Message);
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        sanitize_user_text(self.0.input.as_str().into())
            .lines()
            .map(|line| Line::from(line.to_owned()))
            .collect()
    }
}

impl DelegatedMessageCell {
    /// The message's rows, and how many of them its "Sent by" heading wraps to.
    fn rows(&self, width: u16) -> (usize, Vec<Line<'static>>) {
        let sender = self
            .0
            .source_thread_name
            .as_deref()
            .unwrap_or(&self.0.source_thread_id);
        let sender = sanitize_user_text(sender.into()).replace(['\n', '\t'], " ");
        let options = RtOptions::new(width.saturating_sub(3).max(1) as usize);
        let heading = adaptive_wrap_lines(
            std::iter::once(Line::from(format!("Sent by {sender}").bold())),
            options.clone(),
        );
        let body = sanitize_user_text(self.0.input.as_str().into());
        let words = adaptive_wrap_lines(
            body.lines().map(|line| Line::from(line.to_owned())),
            options,
        );
        (
            heading.len(),
            prefix_lines(
                heading.into_iter().chain(words).collect(),
                "  ".into(),
                "  ".into(),
            ),
        )
    }
}
