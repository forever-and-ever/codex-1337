//! Captured tool-output text; separate text blocks use LF display framing.

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
        matches!(
            item,
            codex_app_server_protocol::ThreadItem::FunctionCallOutput { .. }
        )
    }

    pub(crate) fn from_output(
        id: &str,
        call_id: Option<&str>,
        name: &str,
        namespace: Option<&str>,
        output: &FunctionCallOutputBody,
    ) -> Option<Self> {
        let call_id = call_id?;
        let code_mode = codex_protocol::ToolName::new(namespace.map(str::to_owned), name)
            .is_default_namespace()
            && matches!(name, "exec" | "wait");
        let readable = match output {
            FunctionCallOutputBody::Text(text) => text.clone(),
            FunctionCallOutputBody::ContentItems(items) => items
                .iter()
                .enumerate()
                // The constructor owns this separate slot; notify() strings have no slot.
                .filter(|(index, item)| {
                    !(code_mode
                        && *index == 0
                        && matches!(item, FunctionCallOutputContentItem::InputText { text }
                            if super::returned_output::is_execution_header(text)))
                })
                .filter_map(|(_, item)| match item {
                    FunctionCallOutputContentItem::InputText { text } => Some(text.as_str()),
                    FunctionCallOutputContentItem::InputImage { .. }
                    | FunctionCallOutputContentItem::InputAudio { .. }
                    | FunctionCallOutputContentItem::EncryptedContent { .. } => None,
                })
                // LF frames distinct text blocks in the display, not in the provider payload.
                .collect::<Vec<_>>()
                .join("\n"),
        };
        let wait = if code_mode && readable.is_empty() {
            super::returned_output::empty_wait_outcome(name, output)
        } else {
            (code_mode && name == "wait").then_some(WaitOutcome::Unknown)
        };
        Some(Self {
            id: id.to_owned(),
            call_id: call_id.to_owned(),
            output: output.clone(),
            readable,
            wait,
        })
    }
}

impl HistoryCell for ToolOutputCell {
    fn nested_detail_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.transcript_hyperlink_lines(width)
    }
    fn activity_header(&self) -> Option<super::activity_parts::ActivityHeader> {
        Some(super::activity_parts::ActivityHeader::at_end(&Line::from(
            vec!["• ".into(), self.heading().into()],
        )))
    }
    fn outer_call_id(&self) -> Option<&str> {
        Some(&self.call_id)
    }

    fn activity_disclosure(&self, _width: u16) -> Option<super::ActivityDisclosure> {
        (!self.readable.is_empty()).then_some(super::ActivityDisclosure::Generic)
    }

    fn activity_ids(&self) -> Vec<String> {
        vec![format!("output:tool-output-tokens:{}", self.id)]
    }

    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        // A code-only batch has no nested action header, but its retained result must be reachable.
        let heading = self.heading();
        let mut spans = vec!["• ".dim(), heading.bold()];
        let status = match &self.wait {
            Some(WaitOutcome::Running) => Some("still running"),
            Some(WaitOutcome::Completed) => Some("finished"),
            Some(WaitOutcome::Failed) => Some("failed"),
            Some(WaitOutcome::Terminated) => Some("terminated"),
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
        if self.readable.is_empty() {
            return Vec::new();
        }
        adaptive_wrap_hyperlink_lines(
            &plain_hyperlink_lines(
                self.readable
                    .split('\n')
                    .map(|line| Line::from(line.to_owned()))
                    .collect(),
            ),
            RtOptions::new(width as usize)
                .initial_indent(crate::transcript_geometry::CHILD_PREFIX.into())
                .subsequent_indent(crate::transcript_geometry::CHILD_PREFIX.into()),
        )
    }

    fn is_stream_continuation(&self) -> bool {
        false
    }
}
