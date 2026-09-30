//! Code Mode's returned batch, distinct from nested tool execution and raw results.

use super::HistoryCell;
use super::raw_lines_from_source;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_lines;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use ratatui::style::Stylize;
use ratatui::text::Line;

#[derive(Debug)]
pub(crate) struct ToolOutputCell {
    id: String,
    output: FunctionCallOutputBody,
}

impl ToolOutputCell {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn is_item(item: &codex_app_server_protocol::ThreadItem) -> bool {
        matches!(item, codex_app_server_protocol::ThreadItem::FunctionCallOutput { name, namespace: None, .. } if matches!(name.as_str(), "exec" | "wait"))
    }

    pub(crate) fn from_output(
        id: &str,
        name: &str,
        namespace: Option<&str>,
        output: &FunctionCallOutputBody,
    ) -> Option<Self> {
        (namespace.is_none() && matches!(name, "exec" | "wait")).then(|| Self {
            id: id.to_owned(),
            output: output.clone(),
        })
    }
}

impl HistoryCell for ToolOutputCell {
    fn activity_ids(&self) -> Vec<String> {
        vec![format!("output:tool-output-tokens:{}", self.id)]
    }

    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        // A code-only batch has no nested action header, but its retained result must be reachable.
        vec![vec!["• ".dim(), "Returned".bold()].into()]
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
        adaptive_wrap_lines(
            self.raw_lines(),
            RtOptions::new(width as usize)
                .initial_indent("  ".into())
                .subsequent_indent("  ".into()),
        )
    }

    fn is_stream_continuation(&self) -> bool {
        false
    }
}
