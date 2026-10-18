//! Completed response reasoning, disclosed from the response header without its own row.

use super::HistoryCell;
use super::HistoryTimestamp;
use super::markdown_render_cache::MarkdownRenderCache;
use super::output_provenance::OutputProvenance;
use super::output_provenance::mark_lines;
use crate::markdown_render::ListSpacing;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::visible_lines;
use codex_app_server_protocol::ThreadItem;
use ratatui::text::Line;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug)]
pub(crate) struct ThoughtCell {
    pub(crate) id: String,
    response_id: String,
    timestamp: Option<HistoryTimestamp>,
    text: String,
    cwd: PathBuf,
    rendered: MarkdownRenderCache,
}

impl ThoughtCell {
    pub(crate) fn response_id(&self) -> &str {
        &self.response_id
    }

    pub(crate) fn is_readable(&self) -> bool {
        !self.text.is_empty()
    }

    pub(crate) fn from_item(item: ThreadItem, cwd: &Path) -> Self {
        let ThreadItem::Thought {
            id,
            response_id,
            completed_at_ms,
            content,
            ..
        } = item
        else {
            unreachable!("ThoughtCell requires a Thought item")
        };
        Self {
            id,
            response_id,
            timestamp: HistoryTimestamp::from_millis(completed_at_ms),
            text: content
                .into_iter()
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n\n"),
            cwd: cwd.to_owned(),
            rendered: MarkdownRenderCache::default(),
        }
    }
}

impl HistoryCell for ThoughtCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        self.timestamp
    }

    fn activity_ids(&self) -> Vec<String> {
        vec![self.id.clone()]
    }

    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        Vec::new()
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.text
            .lines()
            .map(|line| Line::from(line.to_owned()))
            .collect()
    }

    fn expanded_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.rendered.render(width, ListSpacing::default(), || {
            let mut lines = crate::markdown_render::render_markdown_lines_with_width_and_cwd(
                &self.text,
                Some(usize::from(width)),
                Some(&self.cwd),
            );
            mark_lines(&mut lines, OutputProvenance::Reasoning);
            lines
        })
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.expanded_hyperlink_lines(width)
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.transcript_hyperlink_lines(width))
    }
}
