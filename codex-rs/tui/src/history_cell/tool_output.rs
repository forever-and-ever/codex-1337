//! Code Mode's returned batch, distinct from nested tool execution and raw results.

use super::HistoryCell;
use super::raw_lines_from_source;
use super::returned_output::WaitOutcome;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines;
use crate::terminal_hyperlinks::plain_hyperlink_lines;
use crate::terminal_hyperlinks::visible_lines;
use crate::wrapping::RtOptions;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use ratatui::style::Stylize;
use ratatui::text::Line;

#[derive(Debug)]
pub(crate) struct ToolOutputCell {
    id: String,
    call_id: String,
    output: FunctionCallOutputBody,
    readable: String,
    wait: Option<WaitOutcome>,
}

impl ToolOutputCell {
    fn heading(&self) -> &'static str {
        if self.wait.is_some() {
            "Waited"
        } else {
            "Returned"
        }
    }
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn is_item(item: &codex_app_server_protocol::ThreadItem) -> bool {
        matches!(item, codex_app_server_protocol::ThreadItem::FunctionCallOutput { name, namespace: None, .. } if matches!(name.as_str(), "exec" | "wait"))
    }

    pub(crate) fn from_output(
        id: &str,
        call_id: Option<&str>,
        name: &str,
        namespace: Option<&str>,
        output: &FunctionCallOutputBody,
    ) -> Option<Self> {
        if namespace.is_some() || !matches!(name, "exec" | "wait") {
            return None;
        }
        let readable = match output {
            FunctionCallOutputBody::Text(text) => {
                super::returned_output::readable_output(text).into_owned()
            }
            FunctionCallOutputBody::ContentItems(items) => items
                .iter()
                .map(|item| match item {
                    FunctionCallOutputContentItem::InputText { text } => {
                        super::returned_output::readable_output(text).into_owned()
                    }
                    FunctionCallOutputContentItem::InputImage { .. } => "<image>".into(),
                    FunctionCallOutputContentItem::InputAudio { .. } => "<audio>".into(),
                    FunctionCallOutputContentItem::EncryptedContent { .. } => String::new(),
                })
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
        };
        let wait = if readable.trim().is_empty() {
            super::returned_output::empty_wait_outcome(name, output)
        } else {
            (name == "wait").then_some(WaitOutcome::Unknown)
        };
        Some(Self {
            id: id.to_owned(),
            call_id: call_id.unwrap_or(id).to_owned(),
            output: output.clone(),
            readable,
            wait,
        })
    }
}

impl HistoryCell for ToolOutputCell {
    fn activity_header(&self) -> Option<super::activity_parts::ActivityHeader> {
        Some(super::activity_parts::ActivityHeader::at_end(&Line::from(
            vec!["• ".into(), self.heading().into()],
        )))
    }
    fn outer_call_id(&self) -> Option<&str> {
        Some(&self.call_id)
    }

    fn activity_disclosure(&self, _width: u16) -> Option<super::ActivityDisclosure> {
        (!self.readable.trim().is_empty()).then_some(super::ActivityDisclosure::Generic)
    }

    fn activity_ids(&self) -> Vec<String> {
        vec![format!("output:tool-output-tokens:{}", self.id)]
    }

    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        // A code-only batch has no nested action header, but its retained result must be reachable.
        let heading = self.heading();
        let mut spans = vec!["• ".dim(), heading.bold()];
        let status = match &self.wait {
            Some(WaitOutcome::Running) => Some("still running".into()),
            Some(WaitOutcome::Completed) => Some("finished".into()),
            Some(WaitOutcome::Exited(code)) => Some(format!("finished, exit {code}")),
            Some(WaitOutcome::Failed) => Some("failed".into()),
            Some(WaitOutcome::Terminated) => Some("terminated".into()),
            Some(WaitOutcome::Unknown) | None => None,
        };
        if let Some(status) = status {
            spans.push(format!(" · {status}").dim());
        }
        vec![spans.into()]
    }

    fn expanded_hyperlink_lines(
        &self,
        width: u16,
    ) -> Vec<crate::terminal_hyperlinks::HyperlinkLine> {
        let mut lines = self.compact_hyperlink_lines(width);
        lines.extend(self.transcript_hyperlink_lines(width));
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        match &self.output {
            FunctionCallOutputBody::Text(text) => raw_lines_from_source(text),
            FunctionCallOutputBody::ContentItems(items) => items
                .iter()
                .flat_map(|item| {
                    raw_lines_from_source(match item {
                        FunctionCallOutputContentItem::InputText { text } => text,
                        FunctionCallOutputContentItem::InputImage { .. } => "<image>",
                        FunctionCallOutputContentItem::InputAudio { .. } => "<audio>",
                        FunctionCallOutputContentItem::EncryptedContent { .. } => {
                            "<encrypted content>"
                        }
                    })
                })
                .collect(),
        }
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.transcript_hyperlink_lines(width))
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        adaptive_wrap_hyperlink_lines(
            &plain_hyperlink_lines(raw_lines_from_source(&self.readable)),
            RtOptions::new(width as usize)
                .initial_indent(crate::transcript_geometry::CHILD_PREFIX.into())
                .subsequent_indent(crate::transcript_geometry::CHILD_PREFIX.into()),
        )
    }

    fn is_stream_continuation(&self) -> bool {
        false
    }
}
