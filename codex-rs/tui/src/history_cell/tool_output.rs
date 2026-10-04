//! Captured tool-output text; separate text blocks use LF display framing.

use super::HistoryCell;
use super::output_provenance::OutputMode;
use super::output_provenance::OutputProvenance;
use super::output_provenance::client_notice;
use super::output_provenance::format_json;
use super::output_provenance::mark_lines;
use super::output_provenance::unsafe_control;
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
use std::sync::OnceLock;

#[derive(Debug)]
pub(crate) struct ToolOutputCell {
    id: String,
    call_id: String,
    output: FunctionCallOutputBody,
    readable: String,
    wait: Option<WaitOutcome>,
    code_mode: bool,
    formatted: OnceLock<Option<Vec<Option<String>>>>,
    escaped: OnceLock<Option<Vec<Option<String>>>>,
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
            code_mode,
            formatted: OnceLock::new(),
            escaped: OnceLock::new(),
        })
    }

    fn blocks(&self) -> Vec<&str> {
        match &self.output {
            FunctionCallOutputBody::Text(text) => vec![text],
            FunctionCallOutputBody::ContentItems(items) => items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| match item {
                    FunctionCallOutputContentItem::InputText { text }
                        if !(self.code_mode
                            && index == 0
                            && super::returned_output::is_execution_header(text)) =>
                    {
                        Some(text.as_str())
                    }
                    _ => None,
                })
                .collect(),
        }
    }

    pub(crate) fn output_lines(&self, width: u16, mode: OutputMode) -> Vec<HyperlinkLine> {
        if self.readable.is_empty() {
            return Vec::new();
        }
        let formatted = self.projection();
        // Safety is mandatory even when optional JSON formatting exceeds its caps.
        // It is linear in retained input, expands by at most 6x, and never truncates.
        let escaped = self.escaped.get_or_init(|| {
            self.readable.chars().any(unsafe_control).then(|| {
                self.blocks()
                    .into_iter()
                    .map(|source| {
                        source.chars().any(unsafe_control).then(|| {
                            source.chars().fold(String::new(), |mut text, ch| {
                                if unsafe_control(ch) {
                                    text.extend(ch.escape_default());
                                } else {
                                    text.push(ch);
                                }
                                text
                            })
                        })
                    })
                    .collect::<Vec<_>>()
            })
        });
        let mut lines = Vec::new();
        for (index, source) in self.blocks().into_iter().enumerate() {
            let projection = (mode == OutputMode::Fmt)
                .then(|| formatted.and_then(|blocks| blocks[index].as_deref()))
                .flatten()
                .or_else(|| escaped.as_ref().and_then(|blocks| blocks[index].as_deref()));
            let text = projection.unwrap_or(source);
            let provenance = if projection.is_some() {
                OutputProvenance::Formatted
            } else {
                OutputProvenance::Captured
            };
            let mut block = plain_hyperlink_lines(
                text.split('\n')
                    .map(|line| Line::from(line.to_owned()))
                    .collect(),
            );
            mark_lines(&mut block, provenance);
            lines.extend(block);
        }
        let mut lines = adaptive_wrap_hyperlink_lines(
            &lines,
            RtOptions::new(width as usize)
                .initial_indent(crate::transcript_geometry::CHILD_PREFIX.into())
                .subsequent_indent(crate::transcript_geometry::CHILD_PREFIX.into()),
        );
        if escaped.is_some() {
            // Let TextLayout wrap this empty-source label; source wrapping would drop its tail.
            lines.push(client_notice(&format!(
                "{}Control characters escaped for display",
                crate::transcript_geometry::CHILD_PREFIX
            )));
        }
        lines
    }

    fn projection(&self) -> Option<&[Option<String>]> {
        self.formatted
            .get_or_init(|| {
                // LF framing bounds the work even when a capture contains many empty blocks.
                if self.readable.len() > super::output_provenance::INPUT_LIMIT {
                    return None;
                }
                let blocks = self.blocks();
                let mut size = blocks.len();
                let mut projection = Vec::with_capacity(blocks.len());
                for source in blocks {
                    let formatted = format_json(source).ok()?;
                    size =
                        size.saturating_add(formatted.as_ref().map_or(source.len(), String::len));
                    if size > super::output_provenance::OUTPUT_LIMIT {
                        return None;
                    }
                    projection.push(formatted);
                }
                projection.iter().any(Option::is_some).then_some(projection)
            })
            .as_deref()
    }
}

impl HistoryCell for ToolOutputCell {
    fn has_formatted_output(&self) -> bool {
        self.projection()
            .is_some_and(|blocks| blocks.iter().any(Option::is_some))
    }

    fn expanded_output_lines(&self, width: u16, mode: OutputMode) -> Vec<HyperlinkLine> {
        let mut lines = self.compact_hyperlink_lines(width);
        lines.extend(self.output_lines(width, mode));
        lines
    }
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
        self.output_lines(width, OutputMode::Raw)
    }

    fn is_stream_continuation(&self) -> bool {
        false
    }
}
