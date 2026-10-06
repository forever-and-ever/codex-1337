//! Preserve invocation identity when an item's factory returns a plain notice.

use super::ActivityDisclosure;
use super::HistoryCell;
use super::activity_parts::ActivityHeader;
use super::activity_parts::ActivityParts;
use crate::terminal_hyperlinks::HyperlinkLine;
use ratatui::text::Line;
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct ToolItemCell {
    pub(crate) call_id: String,
    pub(crate) inner: Arc<dyn HistoryCell>,
}

impl ToolItemCell {
    pub(crate) fn new(call_id: String, inner: impl HistoryCell + 'static) -> Self {
        Self {
            call_id,
            inner: Arc::new(inner),
        }
    }
}

impl HistoryCell for ToolItemCell {
    fn transcript_timestamp(&self) -> Option<super::HistoryTimestamp> {
        self.inner.transcript_timestamp()
    }
    fn has_stable_transcript_height(&self) -> bool {
        self.inner.has_stable_transcript_height()
    }
    fn warning_entries(&self) -> Vec<super::WarningEntry> {
        self.inner.warning_entries()
    }
    fn warning_keys(&self) -> Vec<super::WarningKey<'_>> {
        self.inner.warning_keys()
    }
    fn tool_call_ids(&self) -> Vec<&str> {
        vec![&self.call_id]
    }
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.inner.display_lines(width)
    }
    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.inner.raw_lines()
    }
    fn live_raw_lines(&self) -> Vec<Line<'static>> {
        self.inner.live_raw_lines()
    }
    fn desired_height(&self, width: u16) -> u16 {
        self.inner.desired_height(width)
    }
    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.inner.display_hyperlink_lines(width)
    }
    fn compact_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.inner.compact_hyperlink_lines(width)
    }
    fn retained_hyperlink_lines(&self, width: u16, detailed: bool) -> Vec<HyperlinkLine> {
        self.inner.retained_hyperlink_lines(width, detailed)
    }
    fn expanded_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.inner.expanded_hyperlink_lines(width)
    }
    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.inner.transcript_lines(width)
    }
    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.inner.transcript_hyperlink_lines(width)
    }
    fn nested_summary_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        if self.inner.as_any().is::<super::PlainHistoryCell>() {
            // historical_tool_fallback puts invocation first, raw execution receipt after it.
            return self
                .inner
                .compact_hyperlink_lines(width)
                .into_iter()
                .take(1)
                .collect();
        }
        self.inner.nested_summary_lines(width)
    }
    fn nested_detail_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        if self.inner.as_any().is::<super::PlainHistoryCell>() {
            let mut invocation = self.nested_summary_lines(u16::MAX);
            super::output_provenance::mark_lines(
                &mut invocation,
                super::output_provenance::OutputProvenance::Client,
            );
            return crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines(
                &invocation,
                crate::wrapping::RtOptions::new(usize::from(width)),
            );
        }
        self.inner.nested_detail_lines(width)
    }
    fn nested_activity_parts(
        &self,
        width: u16,
        expanded: &HashSet<String>,
    ) -> Option<ActivityParts> {
        if self.inner.as_any().is::<super::PlainHistoryCell>() {
            return None;
        }
        self.inner.nested_activity_parts(width, expanded)
    }
    fn nested_summary_parts(&self, width: u16, expanded: &HashSet<String>) -> ActivityParts {
        if self.inner.as_any().is::<super::PlainHistoryCell>() {
            return ActivityParts {
                lines: self.nested_summary_lines(width),
                header: self.activity_header(),
                output: None,
                controls: Vec::new(),
            };
        }
        self.inner.nested_summary_parts(width, expanded)
    }
    fn has_nested_details(&self, width: u16) -> bool {
        self.inner.has_nested_details(width)
    }
    fn activity_ids(&self) -> Vec<String> {
        self.inner.activity_ids()
    }
    fn is_tool_entry(&self) -> bool {
        self.inner.is_tool_entry()
    }
    fn outer_call_id(&self) -> Option<&str> {
        self.inner.outer_call_id()
    }
    fn activity_header(&self) -> Option<ActivityHeader> {
        self.inner.activity_header()
    }
    fn activity_header_tokens(&self) -> Option<u64> {
        self.inner.activity_header_tokens()
    }
    fn expanded_activity_header(&self) -> Option<ActivityHeader> {
        self.inner.expanded_activity_header()
    }
    fn activity_disclosure(&self, width: u16) -> Option<ActivityDisclosure> {
        self.inner.activity_disclosure(width)
    }
    fn activity_parts(
        &self,
        width: u16,
        reserve: usize,
        expanded: &HashSet<String>,
    ) -> Option<ActivityParts> {
        self.inner.activity_parts(width, reserve, expanded)
    }
    fn activity_argument_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.inner.activity_argument_lines(width)
    }
    fn has_formatted_output(&self) -> bool {
        self.inner.has_formatted_output()
    }
    fn expanded_output_lines(
        &self,
        width: u16,
        mode: super::output_provenance::OutputMode,
    ) -> Vec<HyperlinkLine> {
        if let Some(fallback) = self
            .inner
            .as_any()
            .downcast_ref::<super::PlainHistoryCell>()
        {
            let mut lines =
                crate::terminal_hyperlinks::plain_hyperlink_lines(fallback.lines.clone());
            super::output_provenance::mark_lines(
                &mut lines,
                super::output_provenance::OutputProvenance::Client,
            );
            if lines.len() > 1 {
                lines.insert(
                    1,
                    super::output_provenance::client_notice(
                        "Stored result · exact capture unavailable",
                    ),
                );
            }
            crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines(
                &lines,
                crate::wrapping::RtOptions::new(usize::from(width)),
            )
        } else {
            self.inner.expanded_output_lines(width, mode)
        }
    }
    fn output_parts(
        &self,
        width: u16,
        reserve: usize,
        expanded: &std::collections::HashSet<String>,
        mode: super::output_provenance::OutputMode,
    ) -> Option<ActivityParts> {
        self.inner.output_parts(width, reserve, expanded, mode)
    }
    fn activity_arguments_truncated(&self, width: u16) -> bool {
        self.inner.activity_arguments_truncated(width)
    }
    fn is_stream_continuation(&self) -> bool {
        self.inner.is_stream_continuation()
    }
    fn transcript_animation_tick(&self) -> Option<u64> {
        self.inner.transcript_animation_tick()
    }
}
