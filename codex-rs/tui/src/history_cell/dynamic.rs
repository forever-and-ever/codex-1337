//! Dynamic tool activity with retained arguments and output across live and loaded transcripts.

use super::HistoryCell;
use super::raw_lines_from_source;
use crate::style::accent_color;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines;
use crate::terminal_hyperlinks::plain_hyperlink_lines;
use crate::terminal_hyperlinks::visible_lines;
use crate::wrapping::RtOptions;
use codex_app_server_protocol::DynamicToolCallOutputContentItem;
use codex_app_server_protocol::DynamicToolCallStatus;
use codex_app_server_protocol::ThreadItem;
use ratatui::style::Stylize;
use ratatui::text::Line;
use serde_json::Value;
use std::sync::Arc;
use std::sync::RwLock;

#[derive(Clone, Debug)]
pub(crate) struct DynamicToolCallCell {
    call_id: String,
    data: Arc<RwLock<DynamicToolCallData>>,
}

#[derive(Debug)]
struct DynamicToolCallData {
    name: String,
    arguments: Value,
    status: DynamicToolCallStatus,
    interrupted: bool,
    output: Option<Vec<String>>,
    duration_ms: Option<i64>,
}

impl DynamicToolCallCell {
    pub(crate) fn from_item(item: ThreadItem) -> Option<Self> {
        let (call_id, data) = DynamicToolCallData::from_item(item)?;
        Some(Self {
            call_id,
            data: Arc::new(RwLock::new(data)),
        })
    }

    pub(crate) fn call_id(&self) -> &str {
        &self.call_id
    }

    pub(crate) fn is_active(&self) -> bool {
        self.data
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_active()
    }

    /// Updates the original retained row when concurrent calls complete in a different order.
    pub(crate) fn update_from_item(&self, item: ThreadItem) -> bool {
        let Some((call_id, data)) = DynamicToolCallData::from_item(item) else {
            return false;
        };
        if call_id != self.call_id {
            return false;
        }
        *self
            .data
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = data;
        true
    }

    pub(crate) fn mark_interrupted(&self) {
        let mut data = self
            .data
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !data.is_active() {
            return;
        }
        data.status = DynamicToolCallStatus::Failed;
        data.interrupted = true;
        data.output
            .get_or_insert_with(Vec::new)
            .push("Interrupted before this tool returned a result.".to_owned());
    }
}

impl DynamicToolCallData {
    fn from_item(item: ThreadItem) -> Option<(String, Self)> {
        let ThreadItem::DynamicToolCall {
            id,
            namespace,
            tool,
            arguments,
            status,
            content_items,
            success,
            duration_ms,
        } = item
        else {
            return None;
        };
        Some((
            id,
            Self {
                name: namespace
                    .map_or_else(|| tool.clone(), |namespace| format!("{namespace}.{tool}")),
                arguments,
                status: if success == Some(false) {
                    DynamicToolCallStatus::Failed
                } else {
                    status
                },
                interrupted: false,
                output: content_items.map(|items| {
                    items
                        .into_iter()
                        .map(|item| match item {
                            DynamicToolCallOutputContentItem::InputText { text } => text,
                            DynamicToolCallOutputContentItem::InputImage { .. } => {
                                "<image content>".to_owned()
                            }
                            DynamicToolCallOutputContentItem::InputAudio { .. } => {
                                "<audio content>".to_owned()
                            }
                        })
                        .collect()
                }),
                duration_ms,
            },
        ))
    }

    fn is_active(&self) -> bool {
        matches!(self.status, DynamicToolCallStatus::InProgress)
    }

    fn header(&self) -> (Line<'static>, super::activity_parts::ActivityHeader) {
        let (marker, verb) = if self.interrupted {
            ("•".red().bold(), "Interrupted")
        } else {
            match self.status {
                DynamicToolCallStatus::InProgress => ("•".dim(), "Calling"),
                DynamicToolCallStatus::Completed => ("•".green(), "Called"),
                DynamicToolCallStatus::Failed => ("•".red().bold(), "Failed"),
            }
        };
        let mut line = Line::from(vec![marker, " ".into(), verb.bold()]);
        let header = super::activity_parts::ActivityHeader::at_end(&line);
        line.extend([" ".into(), self.name.clone().fg(accent_color())]);
        if let Some(duration_ms) = self.duration_ms.filter(|duration| *duration >= 0) {
            line.push_span(format!(" · {duration_ms}ms").dim());
        }
        (line, header)
    }

    fn argument_lines(&self) -> Vec<Line<'static>> {
        let args = serde_json::to_string_pretty(&self.arguments)
            .unwrap_or_else(|_| self.arguments.to_string());
        raw_lines_from_source(&format!("Arguments: {args}"))
    }

    fn output_lines(&self) -> Vec<Line<'static>> {
        match &self.output {
            Some(output) if output.is_empty() => vec![Line::from("(no output)".dim())],
            Some(output) => output
                .iter()
                .flat_map(|text| raw_lines_from_source(text))
                .collect(),
            None if self.is_active() => Vec::new(),
            None => vec![Line::from("(output unavailable)".dim())],
        }
    }
}

impl HistoryCell for DynamicToolCallCell {
    fn tool_call_ids(&self) -> Vec<&str> {
        vec![self.call_id()]
    }
    fn activity_header(&self) -> Option<super::activity_parts::ActivityHeader> {
        Some(
            self.data
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .header()
                .1,
        )
    }
    fn activity_disclosure(&self, width: u16) -> Option<super::ActivityDisclosure> {
        let has_output = self
            .data
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .output
            .as_ref()
            .is_some_and(|output| output.iter().any(|text| !text.trim().is_empty()));
        (has_output || self.activity_arguments_truncated(width))
            .then_some(super::ActivityDisclosure::Generic)
    }

    fn activity_arguments_truncated(&self, width: u16) -> bool {
        self.compact_hyperlink_lines(u16::MAX)
            .iter()
            .any(|line| line.width() > usize::from(width))
    }

    fn has_stable_transcript_height(&self) -> bool {
        false
    }

    fn activity_ids(&self) -> Vec<String> {
        vec![format!("dynamic:{}", self.call_id)]
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.compact_hyperlink_lines(width))
    }

    fn compact_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let data = self
            .data
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut header = data.header().0;
        header.push_span("(");
        header.push_span(
            super::activity_preview::compact_arguments(
                &data.arguments,
                super::activity_preview::ArgumentSyntax::Json,
            )
            .dim(),
        );
        header.push_span(")");
        vec![super::activity_preview::compact_header(header, usize::from(width)).into()]
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let data = self
            .data
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut lines = vec![data.header().0.into()];
        let details = data
            .argument_lines()
            .into_iter()
            .chain(data.output_lines())
            .collect();
        let mut details = plain_hyperlink_lines(details);
        super::output_provenance::mark_lines(
            &mut details,
            super::output_provenance::OutputProvenance::Client,
        );
        if !data.is_active() {
            lines.push(super::output_provenance::client_notice(
                "Stored result · exact capture unavailable",
            ));
        }
        lines.extend(adaptive_wrap_hyperlink_lines(
            &details,
            RtOptions::new(usize::from(width).max(/*other*/ 1))
                .initial_indent(crate::transcript_geometry::CHILD_BRANCH.dim().into())
                .subsequent_indent(crate::transcript_geometry::CHILD_PREFIX.into()),
        ));
        lines
    }

    fn activity_argument_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let data = self
            .data
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut lines = vec![data.header().0.into()];
        let mut arguments = plain_hyperlink_lines(data.argument_lines());
        super::output_provenance::mark_lines(
            &mut arguments,
            super::output_provenance::OutputProvenance::Client,
        );
        lines.extend(adaptive_wrap_hyperlink_lines(
            &arguments,
            RtOptions::new(usize::from(width).max(/*other*/ 1))
                .initial_indent(crate::transcript_geometry::CHILD_BRANCH.dim().into())
                .subsequent_indent(crate::transcript_geometry::CHILD_PREFIX.into()),
        ));
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let data = self
            .data
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut lines = vec![data.header().0];
        lines.extend(raw_lines_from_source(&format!(
            "Arguments: {}",
            data.arguments
        )));
        lines.extend(data.output_lines());
        super::plain_lines(lines)
    }
}

#[cfg(test)]
#[path = "dynamic_tests.rs"]
mod tests;
