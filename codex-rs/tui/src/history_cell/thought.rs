//! A completed response's reasoning attribution with optional readable disclosure.

use super::ActivityDisclosure;
use super::HistoryCell;
use super::HistoryTimestamp;
use crate::legacy_core::config::Config;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::visible_lines;
use codex_app_server_protocol::ThreadItem;
use ratatui::style::Stylize;
use ratatui::text::Line;
use std::path::Path;
use std::path::PathBuf;

#[derive(Clone, Copy)]
pub(crate) enum ThoughtTextVisibility {
    Hidden,
    Summary,
    SummaryAndContent,
}

impl From<&Config> for ThoughtTextVisibility {
    fn from(config: &Config) -> Self {
        match (config.hide_agent_reasoning, config.show_raw_agent_reasoning) {
            (true, _) => Self::Hidden,
            (false, false) => Self::Summary,
            (false, true) => Self::SummaryAndContent,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ThoughtCell {
    pub(crate) id: String,
    timestamp: Option<HistoryTimestamp>,
    tokens: Option<u64>,
    text: String,
    cwd: PathBuf,
}

impl ThoughtCell {
    fn heading(&self) -> Line<'static> {
        let mut heading = Line::default();
        heading.push_span("Thought".bold());
        heading
    }

    pub(crate) fn from_item(
        item: ThreadItem,
        cwd: &Path,
        visibility: ThoughtTextVisibility,
    ) -> Self {
        let ThreadItem::Thought {
            id,
            completed_at_ms,
            summary,
            content,
            reported_reasoning_tokens,
            ..
        } = item
        else {
            unreachable!("ThoughtCell requires a Thought item")
        };
        let text = if matches!(visibility, ThoughtTextVisibility::Hidden) {
            String::new()
        } else {
            summary
                .into_iter()
                .chain(
                    matches!(visibility, ThoughtTextVisibility::SummaryAndContent)
                        .then_some(content)
                        .into_iter()
                        .flatten(),
                )
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        };
        Self {
            id,
            timestamp: HistoryTimestamp::from_millis(completed_at_ms),
            tokens: reported_reasoning_tokens,
            text,
            cwd: cwd.to_owned(),
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

    fn activity_disclosure(&self, _width: u16) -> Option<ActivityDisclosure> {
        (!self.text.trim().is_empty()).then_some(ActivityDisclosure::Generic)
    }

    fn activity_header_tokens(&self) -> Option<u64> {
        self.tokens
    }

    fn activity_header(&self) -> Option<super::activity_parts::ActivityHeader> {
        let mut header = super::activity_parts::ActivityHeader::at_end(&self.heading());
        header.columns += usize::from(crate::transcript_geometry::MARKER_COLUMNS);
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
        let details = crate::markdown_render::render_markdown_lines_with_width_and_cwd(
            &self.text,
            Some(usize::from(width.saturating_sub(2))),
            Some(&self.cwd),
        );
        lines.extend(crate::terminal_hyperlinks::prefix_hyperlink_lines(
            details,
            "  ".into(),
            "  ".into(),
        ));
        lines
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.transcript_hyperlink_lines(width))
    }
}
