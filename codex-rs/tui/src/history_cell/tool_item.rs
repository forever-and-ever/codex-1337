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
    fn tool_call_ids(&self) -> Vec<&str> {
        vec![&self.call_id]
    }
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.inner.display_lines(width)
    }
    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.inner.raw_lines()
    }
    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.inner.display_hyperlink_lines(width)
    }
    fn compact_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.inner.compact_hyperlink_lines(width)
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
            return crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines(
                &self.nested_summary_lines(u16::MAX),
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
