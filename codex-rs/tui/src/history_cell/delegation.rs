//! Attributed cross-thread messages are tool output, never user prompts.

use super::HistoryCell;
use super::HistoryTimestamp;
use super::Line;
use super::RtOptions;
use super::Stylize;
use super::activity_parts::ActivityParts;
use super::activity_parts::FoldState;
use super::output_provenance::OutputMode;
use super::sanitize_user_text;
use crate::dynamic_tools::DelegatedMessage;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines;
use crate::terminal_hyperlinks::plain_hyperlink_lines;
use crate::terminal_hyperlinks::prefix_hyperlink_lines;
use crate::terminal_hyperlinks::visible_lines;
use crate::transcript_view::EntryKind;
use crate::transcript_view::FeedEntry;
use crate::warpaint::Figure;
use codex_protocol::step_admission::ArrivalKind;

#[derive(Debug)]
pub(crate) struct DelegatedMessageCell(pub(crate) DelegatedMessage);

impl FeedEntry for DelegatedMessageCell {
    fn feed_kind(&self) -> EntryKind {
        EntryKind::Arrival(ArrivalKind::InterAgent)
    }

    fn feed_id(&self) -> String {
        // Coordination envelopes have no delivery ID. Each retained immutable cell is distinct,
        // even when the same sender sends identical text twice.
        format!("message:{self:p}")
    }

    fn feed_title(&self) -> Line<'static> {
        let sender = self
            .0
            .source_thread_name
            .as_deref()
            .unwrap_or(&self.0.source_thread_id);
        let sender = sanitize_user_text(sender.into()).replace(['\n', '\t'], " ");
        Line::from(vec![
            "Message".bold(),
            format!("  Sent by {sender}").fg(Figure::InkMuted.color()),
        ])
    }

    fn feed_body(&self, width: u16, _expanded: &FoldState, _mode: OutputMode) -> ActivityParts {
        ActivityParts {
            lines: self.body(width),
            ..ActivityParts::default()
        }
    }
}

impl HistoryCell for DelegatedMessageCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        self.0.sent_at.and_then(HistoryTimestamp::from_millis)
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.display_hyperlink_lines(width))
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = vec![self.feed_title().into()];
        lines.extend(prefix_hyperlink_lines(
            self.body(width.saturating_sub(2)),
            "  ".into(),
            "  ".into(),
        ));
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
    fn body(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = plain_hyperlink_lines(self.raw_lines());
        super::output_provenance::hang(&mut lines, crate::warpaint::Bracket::Message);
        adaptive_wrap_hyperlink_lines(&lines, RtOptions::new(width.max(1) as usize))
    }
}
