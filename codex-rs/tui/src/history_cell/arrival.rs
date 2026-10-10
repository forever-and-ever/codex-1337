//! Context a step admitted after its results: an injection, or a message from another agent.
//! A Feed Item of its own, whose figure arrives with the step's seal; see
//! `transcript_view::step_bills`.

use super::ActivityDisclosure;
use super::HistoryCell;
use super::HistoryTimestamp;
use super::activity_parts::ActivityHeader;
use super::sanitize_user_text;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::visible_lines;
use crate::transcript_view::FeedEntry;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_lines;
use codex_app_server_protocol::ArrivalItem;
use codex_protocol::step_admission::ArrivalKind;
use ratatui::style::Stylize;
use ratatui::text::Line;

#[derive(Debug)]
pub(crate) struct ArrivalCell(ArrivalItem);

impl ArrivalCell {
    pub(crate) fn new(item: ArrivalItem) -> Self {
        Self(item)
    }

    pub(crate) fn id(&self) -> &str {
        &self.0.id
    }

    /// The response whose step admitted this arrival and bills it.
    pub(crate) fn response_id(&self) -> &str {
        &self.0.response_id
    }

    fn heading(&self) -> Line<'static> {
        let label = sanitize_user_text(self.0.label.as_str().into()).replace(['\n', '\t'], " ");
        let (kind, joint) = match self.0.kind {
            ArrivalKind::InterAgent => ("Message", " from "),
            ArrivalKind::Injection => ("Injection", " · "),
            ArrivalKind::User => ("User input", " · "),
        };
        Line::from(vec![kind.bold(), joint.dim(), label.into()])
    }

    fn body(&self, width: u16) -> Vec<Line<'static>> {
        let Some(text) = &self.0.text else {
            return vec!["encrypted".dim().italic().into()];
        };
        let text = sanitize_user_text(text.as_str().into());
        adaptive_wrap_lines(
            text.lines().map(|line| Line::from(line.to_owned())),
            RtOptions::new(width.saturating_sub(2).max(1) as usize),
        )
    }
}

impl FeedEntry for ArrivalCell {
    fn feed_title(&self) -> Line<'static> {
        self.heading()
    }

    fn feed_body(&self, width: u16) -> Vec<HyperlinkLine> {
        crate::terminal_hyperlinks::prefix_hyperlink_lines(
            self.body(width).into_iter().map(Into::into).collect(),
            "  ".into(),
            "  ".into(),
        )
    }
}

impl HistoryCell for ArrivalCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        HistoryTimestamp::from_millis(self.0.announced_at_ms)
    }

    fn activity_ids(&self) -> Vec<String> {
        vec![self.0.id.clone()]
    }

    fn activity_disclosure(&self, _width: u16) -> Option<ActivityDisclosure> {
        Some(ActivityDisclosure::Generic)
    }

    fn activity_header(&self) -> Option<ActivityHeader> {
        let mut header = ActivityHeader::at_end(&self.heading());
        header.expand_on_truncation = false;
        Some(header)
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.display_hyperlink_lines(width))
    }

    fn display_hyperlink_lines(&self, _width: u16) -> Vec<HyperlinkLine> {
        crate::terminal_hyperlinks::prefix_hyperlink_lines(
            vec![self.heading().into()],
            super::agent_prefix(/*first*/ true),
            "  ".into(),
        )
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.display_lines(u16::MAX)
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.expanded_hyperlink_lines(width)
    }

    fn expanded_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = self.display_hyperlink_lines(width);
        lines.extend(crate::terminal_hyperlinks::prefix_hyperlink_lines(
            self.body(width).into_iter().map(Into::into).collect(),
            "  ".into(),
            "  ".into(),
        ));
        lines
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.transcript_hyperlink_lines(width))
    }
}
