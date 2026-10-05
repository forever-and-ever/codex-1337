//! Attributed cross-thread messages are tool output, never user prompts.

use super::*;
use crate::dynamic_tools::DelegatedMessage;
use crate::style::history_prompt_style;

#[derive(Debug)]
pub(crate) struct DelegatedMessageCell(pub(crate) DelegatedMessage);

impl HistoryCell for DelegatedMessageCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        self.0.sent_at.and_then(HistoryTimestamp::from_millis)
    }
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let style = history_prompt_style();
        let sender = self
            .0
            .source_thread_name
            .as_deref()
            .unwrap_or(&self.0.source_thread_id);
        let sender = sanitize_user_text(sender.into()).replace(['\n', '\t'], " ");
        let mut header = Vec::new();
        header.push(format!("Sent by {sender}").bold());
        let body = sanitize_user_text(self.0.input.as_str().into());
        let content = std::iter::once(Line::from(header))
            .chain(body.lines().map(|line| Line::from(line.to_owned())));
        let wrapped = adaptive_wrap_lines(
            content,
            RtOptions::new(width.saturating_sub(3).max(1) as usize),
        );
        let mut lines = vec![Line::from("").style(style)];
        lines.extend(
            prefix_lines(wrapped, "  ".into(), "  ".into())
                .into_iter()
                .map(|line| line.style(style)),
        );
        lines.push(Line::from("").style(style));
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        sanitize_user_text(self.0.input.as_str().into())
            .lines()
            .map(|line| Line::from(line.to_owned()))
            .collect()
    }
}
