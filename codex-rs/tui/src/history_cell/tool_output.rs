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

pub(super) fn result_parts(
    owner: Option<&dyn HistoryCell>,
    outputs: &[&ToolOutputCell],
    width: u16,
    expanded: &std::collections::HashSet<String>,
    mode: OutputMode,
) -> super::activity_parts::ActivityParts {
    let first = match (owner, outputs.first()) {
        (Some(owner), _) => owner,
        (None, Some(output)) => *output,
        (None, None) => unreachable!("result disclosure requires an owner or captured output"),
    };
    let mut lines = first.compact_hyperlink_lines(width);
    mark_lines(&mut lines, OutputProvenance::Client);
    let mut parts = super::activity_parts::ActivityParts {
        lines,
        header: first.activity_header(),
        output: Some(super::ActivityDisclosure::Generic),
        controls: Vec::new(),
    };
    let open = owner
        .into_iter()
        .flat_map(HistoryCell::activity_ids)
        .chain(outputs.iter().flat_map(|output| output.activity_ids()))
        .any(|id| expanded.contains(&id));
    if open {
        if let Some(owner) = owner {
            parts.lines.extend(owner.activity_argument_lines(width));
        }
        for output in outputs {
            output.append_result_parts(&mut parts, width, expanded, mode);
        }
    }
    parts
}

#[derive(Debug)]
pub(crate) struct ToolOutputCell {
    id: String,
    call_id: Option<String>,
    name: codex_protocol::ToolName,
    output: FunctionCallOutputBody,
    readable: String,
    wait: Option<WaitOutcome>,
    code_mode: bool,
    formatted: OnceLock<Option<Vec<Option<String>>>>,
    escaped: OnceLock<Option<Vec<Option<String>>>>,
}

impl ToolOutputCell {
    /// Only a known zero measurement may hide this empty, successful presentation.
    /// Captured headers with status other than successful completion remain visible.
    pub(crate) fn is_empty_completed_return(&self) -> bool {
        if !self.readable.is_empty() || self.wait.is_some() || self.has_agent_wait_name() {
            return false;
        }
        match &self.output {
            FunctionCallOutputBody::Text(text) => !self.code_mode && text.is_empty(),
            FunctionCallOutputBody::ContentItems(items) => {
                items.iter().enumerate().all(|(index, item)| {
                    matches!(item, FunctionCallOutputContentItem::InputText { text }
                    if text.is_empty() || (self.code_mode && index == 0
                        && text.starts_with("Script completed\n")
                        && super::returned_output::is_execution_header(text)))
                })
            }
        }
    }

    fn heading(&self) -> std::borrow::Cow<'static, str> {
        if self.call_id.is_none() || self.has_agent_wait_name() {
            format!("{} returned", self.name).into()
        } else if self.wait.is_some() {
            "Waited".into()
        } else if self.code_mode {
            "Returned".into()
        } else {
            format!("{} returned", self.name).into()
        }
    }
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
    pub(crate) fn name(&self) -> &codex_protocol::ToolName {
        &self.name
    }

    pub(crate) fn captured_output(&self) -> &FunctionCallOutputBody {
        &self.output
    }
    pub(crate) fn append_result_parts(
        &self,
        parts: &mut super::activity_parts::ActivityParts,
        width: u16,
        expanded: &std::collections::HashSet<String>,
        mode: OutputMode,
    ) {
        if self.readable.is_empty() {
            return;
        }
        let id = format!("result:{}", self.id());
        let open = expanded.contains(&id);
        parts.controls.push(super::activity_parts::PartControl {
            id,
            line: parts.lines.len(),
            expanded: open,
        });
        parts.lines.push(client_notice("  Result"));
        if open {
            parts.lines.extend(self.output_lines(width, mode));
        }
    }

    pub(crate) fn is_agent_wait(&self) -> bool {
        self.name == codex_protocol::ToolName::new(Some("wrangler_agents".into()), "wait_agent")
    }

    pub(crate) fn has_agent_wait_name(&self) -> bool {
        self.name.name == "wait_agent"
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
        if call_id.is_none()
            && name != "wait_agent"
            && match output {
                FunctionCallOutputBody::Text(text) => text.is_empty(),
                FunctionCallOutputBody::ContentItems(items) => items.is_empty(),
            }
        {
            return None;
        }
        let tool_name = codex_protocol::ToolName::new(namespace.map(str::to_owned), name);
        let code_mode = tool_name.is_default_namespace() && matches!(name, "exec" | "wait");
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
            call_id: call_id.map(str::to_owned),
            name: tool_name,
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
            let provenance = if mode == OutputMode::Fmt || projection.is_some() {
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

    fn output_parts(
        &self,
        width: u16,
        _reserve: usize,
        expanded: &std::collections::HashSet<String>,
        mode: OutputMode,
    ) -> Option<super::activity_parts::ActivityParts> {
        (self.has_agent_wait_name() || self.activity_disclosure(width).is_some())
            .then(|| result_parts(None, &[self], width, expanded, mode))
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
        let mut header = super::activity_parts::ActivityHeader::at_end(&Line::from(vec![
            "• ".into(),
            self.heading().into(),
        ]));
        header.expand_on_truncation = false;
        Some(header)
    }
    fn outer_call_id(&self) -> Option<&str> {
        self.call_id.as_deref()
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

    fn expanded_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
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
