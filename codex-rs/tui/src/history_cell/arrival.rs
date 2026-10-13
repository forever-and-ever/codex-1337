//! Context a step admitted after its results: an injection, or a message from another agent.
//! A Feed Item of its own, whose figure arrives with the step's seal; see
//! `transcript_view::step_bills`.

use super::ActivityDisclosure;
use super::HistoryCell;
use super::HistoryTimestamp;
use super::activity_parts::ActivityHeader;
use super::activity_parts::ActivityParts;
use super::sanitize_user_text;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::visible_lines;
use crate::transcript_view::FeedEntry;
use crate::wrapping::RtOptions;
use codex_app_server_protocol::ArrivalItem;
use codex_protocol::step_admission::ArrivalKind;
use ratatui::style::Stylize;
use ratatui::text::Line;
use std::collections::HashSet;

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

    pub(crate) fn source_lines(
        &self,
        mode: super::output_provenance::OutputMode,
    ) -> Vec<HyperlinkLine> {
        use super::output_provenance::{
            ContextAuthor, captured_blocks, client_lines, client_notice, context_sources,
        };
        use codex_protocol::step_admission::ArrivalSource;
        let author = match self.0.kind {
            ArrivalKind::InterAgent => ContextAuthor::Subagent,
            ArrivalKind::User => ContextAuthor::User,
            ArrivalKind::Injection => ContextAuthor::Environment,
        };
        match &self.0.source {
            ArrivalSource::Captured { blocks } => captured_blocks(blocks, author, mode),
            ArrivalSource::Unavailable => {
                vec![client_notice("Readable context capture unavailable")]
            }
            ArrivalSource::ToolDeclarations { text } => context_sources(
                &[codex_protocol::ContextSource::ToolDeclarations { text: text.clone() }],
                mode,
            ),
            ArrivalSource::Legacy => {
                let mut lines = vec![client_notice(
                    "Legacy arrival — exact context provenance was not recorded",
                )];
                if let Some(text) = &self.0.text {
                    lines.extend(client_lines(text, mode));
                }
                lines
            }
        }
    }

    fn raw_fold(&self) -> String {
        format!("arrival-raw:{}", self.0.id)
    }

    pub(super) fn context_lines(
        &self,
        width: u16,
        mode: super::output_provenance::OutputMode,
    ) -> Vec<HyperlinkLine> {
        crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines(
            &self.source_lines(mode),
            RtOptions::new(width.saturating_sub(2).max(1) as usize),
        )
    }
}

impl FeedEntry for ArrivalCell {
    fn feed_title(&self) -> Line<'static> {
        self.heading()
    }

    fn feed_body(
        &self,
        width: u16,
        expanded: &HashSet<String>,
        mode: super::output_provenance::OutputMode,
    ) -> ActivityParts {
        use super::activity_parts::PartControl;
        use super::output_provenance::{OutputProvenance, mark_lines};
        let declarations = matches!(
            self.0.source,
            codex_protocol::step_admission::ArrivalSource::ToolDeclarations { .. }
        );
        let heading = if declarations {
            "Raw Context"
        } else {
            "Context"
        };
        let mut lines = vec![HyperlinkLine::from(heading)];
        mark_lines(&mut lines, OutputProvenance::Client);
        let mut controls = Vec::new();
        if declarations {
            let id = self.raw_fold();
            let expanded = expanded.contains(&id);
            controls.push(PartControl {
                id,
                line: 0,
                expanded,
            });
            if expanded {
                lines.extend(self.context_lines(width, mode));
            }
        } else {
            lines.extend(self.context_lines(width, mode));
        }
        ActivityParts {
            lines: crate::terminal_hyperlinks::prefix_hyperlink_lines(
                lines,
                "  ".into(),
                "  ".into(),
            ),
            header: Some(ActivityHeader {
                source_offset: heading.len(),
                expand_on_truncation: false,
            }),
            output: None,
            controls,
        }
    }
}

impl HistoryCell for ArrivalCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        HistoryTimestamp::from_millis(self.0.announced_at_ms)
    }

    fn has_formatted_output(&self) -> bool {
        match &self.0.source {
            codex_protocol::step_admission::ArrivalSource::Captured { blocks } => {
                !blocks.is_empty()
            }
            codex_protocol::step_admission::ArrivalSource::ToolDeclarations { .. } => true,
            codex_protocol::step_admission::ArrivalSource::Legacy => self.0.text.is_some(),
            codex_protocol::step_admission::ArrivalSource::Unavailable => false,
        }
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
        self.source_lines(super::output_provenance::OutputMode::Raw)
            .into_iter()
            .filter(|line| {
                line.source
                    .as_ref()
                    .is_some_and(|source| source.prefix_bytes == 0)
            })
            .map(|line| line.line)
            .collect()
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.expanded_hyperlink_lines(width)
    }

    fn expanded_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = self.display_hyperlink_lines(width);
        lines.extend(crate::terminal_hyperlinks::prefix_hyperlink_lines(
            self.context_lines(width, super::output_provenance::OutputMode::Raw),
            "  ".into(),
            "  ".into(),
        ));
        lines
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.transcript_hyperlink_lines(width))
    }
}
