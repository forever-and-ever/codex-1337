//! Context a step admitted after its results: an injection, or a message from another agent.
//! Prepared with a local token estimate before publication. A later step seal may
//! supersede the estimate; see `transcript_view::step_bills`.

use super::ActivityDisclosure;
use super::HistoryCell;
use super::HistoryTimestamp;
use super::activity_parts::ActivityHeader;
use super::activity_parts::ActivityParts;
use super::markdown_render_cache::MarkdownRenderCache;
use super::sanitize_user_text;
use crate::history_cell::activity_parts::FoldState;
use crate::markdown_render::ListSpacing;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::visible_lines;
use crate::transcript_view::FeedEntry;
use crate::wrapping::RtOptions;
use codex_app_server_protocol::ArrivalItem;
use codex_protocol::step_admission::ArrivalKind;
use codex_utils_output_truncation::count_o200k_arrival;
use ratatui::style::Stylize;
use ratatui::text::Line;

mod message;

#[derive(Debug)]
pub(crate) struct ArrivalCell {
    item: ArrivalItem,
    estimated_tokens: Option<u64>,
    rendered_message: MarkdownRenderCache,
}

impl ArrivalCell {
    pub(crate) fn new(item: ArrivalItem) -> Self {
        // Preparation is synchronous: neither first paint nor replay waits on a later
        // provider seal, which may be permanently severed by a server restart.
        let estimated_tokens = count_o200k_arrival(&item.source);
        Self {
            item,
            estimated_tokens,
            rendered_message: MarkdownRenderCache::default(),
        }
    }

    pub(crate) fn id(&self) -> &str {
        &self.item.id
    }

    pub(crate) fn estimated_tokens(&self) -> Option<u64> {
        self.estimated_tokens
    }

    /// The response whose step admitted this arrival and bills it.
    pub(crate) fn response_id(&self) -> &str {
        &self.item.response_id
    }

    pub(crate) fn placement(&self) -> Option<&codex_protocol::step_admission::ArrivalPlacement> {
        self.item.placement.as_ref()
    }

    fn heading(&self) -> Line<'static> {
        let label = sanitize_user_text(self.item.label.as_str().into()).replace(['\n', '\t'], " ");
        let (kind, joint) = match self.item.kind {
            ArrivalKind::InterAgent => ("Message", " from "),
            ArrivalKind::Injection => ("Injection", " · "),
            ArrivalKind::User => ("User input", " · "),
        };
        let mut heading = Line::from(vec![kind.bold(), joint.dim(), label.into()]);
        if let Some(envelope) = message::envelope(&self.item)
            && envelope.message_type != "MESSAGE"
        {
            heading.push_span(format!(" ({})", envelope.message_type));
        }
        heading
    }

    pub(crate) fn source_lines(
        &self,
        mode: super::output_provenance::OutputMode,
    ) -> Vec<HyperlinkLine> {
        use super::output_provenance::ContextAuthor;
        use super::output_provenance::captured_blocks;
        use super::output_provenance::client_lines;
        use super::output_provenance::client_notice;
        use super::output_provenance::context_sources;
        use codex_protocol::step_admission::ArrivalSource;
        let author = match self.item.kind {
            ArrivalKind::InterAgent => ContextAuthor::Subagent,
            ArrivalKind::User => ContextAuthor::User,
            ArrivalKind::Injection => ContextAuthor::Environment,
        };
        match &self.item.source {
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
                if let Some(text) = &self.item.text {
                    lines.extend(client_lines(text, mode));
                }
                lines
            }
        }
    }

    fn raw_fold(&self) -> String {
        format!("arrival-raw:{}", self.item.id)
    }

    pub(super) fn context_lines(
        &self,
        width: u16,
        mode: super::output_provenance::OutputMode,
    ) -> Vec<HyperlinkLine> {
        use super::output_provenance::ContextAuthor;
        use super::output_provenance::captured_blocks;
        use super::output_provenance::client_lines;
        use super::output_provenance::client_notice;
        use codex_protocol::step_admission::ArrivalSource;
        let lines = match (&self.item.source, message::envelope(&self.item)) {
            (ArrivalSource::Captured { blocks }, Some(envelope)) => {
                let blocks: Vec<&str> = std::iter::once(envelope.payload)
                    .chain(blocks.iter().skip(1).map(String::as_str))
                    .collect();
                captured_blocks(&blocks, ContextAuthor::Subagent, mode)
            }
            (ArrivalSource::Legacy, Some(envelope)) => {
                let mut lines = vec![client_notice(
                    "Legacy arrival — exact context provenance was not recorded",
                )];
                lines.extend(client_lines(envelope.payload, mode));
                lines
            }
            _ => self.source_lines(mode),
        };
        crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines(
            &lines,
            RtOptions::new(width.saturating_sub(2).max(1) as usize),
        )
    }
}

impl FeedEntry for ArrivalCell {
    fn feed_kind(&self) -> crate::transcript_view::EntryKind {
        use crate::transcript_view::EntryKind;
        EntryKind::Arrival(self.item.kind)
    }
    fn feed_id(&self) -> String {
        self.item.id.clone()
    }
    fn feed_disclosure(&self) -> crate::transcript_view::HeaderDisclosure {
        if self.item.kind == ArrivalKind::Injection {
            crate::transcript_view::HeaderDisclosure::Collapsed
        } else {
            crate::transcript_view::HeaderDisclosure::Expanded
        }
    }

    fn feed_title(&self) -> Line<'static> {
        self.heading()
    }

    fn feed_body(
        &self,
        width: u16,
        expanded: &FoldState,
        mode: super::output_provenance::OutputMode,
    ) -> ActivityParts {
        use super::activity_parts::PartControl;
        use super::output_provenance::OutputProvenance;
        use super::output_provenance::mark_lines;
        if self.item.kind == ArrivalKind::InterAgent {
            return ActivityParts {
                lines: self
                    .rendered_message
                    .render(width, ListSpacing::Uniform, || {
                        message::body(&self.item, width)
                    }),
                ..ActivityParts::default()
            };
        }
        let declarations = matches!(
            self.item.source,
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
            let expanded =
                expanded.is_open(super::activity_parts::FoldGroup::RawContext, &id, false);
            controls.push(PartControl {
                group: super::activity_parts::FoldGroup::RawContext,
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
            lines,
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
        HistoryTimestamp::from_millis(self.item.announced_at_ms)
    }

    fn has_formatted_output(&self) -> bool {
        if self.item.kind == ArrivalKind::InterAgent {
            return false;
        }
        match &self.item.source {
            codex_protocol::step_admission::ArrivalSource::Captured { blocks } => {
                !blocks.is_empty()
            }
            codex_protocol::step_admission::ArrivalSource::ToolDeclarations { .. } => true,
            codex_protocol::step_admission::ArrivalSource::Legacy => self.item.text.is_some(),
            codex_protocol::step_admission::ArrivalSource::Unavailable => false,
        }
    }

    fn activity_ids(&self) -> Vec<String> {
        vec![self.item.id.clone()]
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
