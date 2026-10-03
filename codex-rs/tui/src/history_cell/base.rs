//! Shared history-cell building blocks reused across transcript concerns.
//! Wrapped prefixes remain display-only while annotated rows retain their logical source.

use super::*;

#[derive(Debug)]
pub(crate) struct PlainHistoryCell {
    pub(super) lines: Vec<Line<'static>>,
    pub(crate) timestamp: Option<HistoryTimestamp>,
    pub(crate) tool_entry: bool,
}

impl PlainHistoryCell {
    pub(crate) fn new(lines: Vec<Line<'static>>) -> Self {
        Self {
            lines,
            timestamp: None,
            tool_entry: false,
        }
    }
}

impl HistoryCell for PlainHistoryCell {
    fn nested_summary_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        plain_hyperlink_lines(
            self.lines
                .first()
                .map(|line| {
                    crate::line_truncation::truncate_line_with_ellipsis_if_overflow(
                        line.clone(),
                        usize::from(width),
                    )
                })
                .into_iter()
                .collect(),
        )
    }
    fn nested_detail_lines(&self, _width: u16) -> Vec<HyperlinkLine> {
        plain_hyperlink_lines(self.lines.clone())
    }
    fn has_nested_details(&self, width: u16) -> bool {
        self.lines.len() > 1
            || self
                .lines
                .first()
                .is_some_and(|line| line.width() > usize::from(width))
    }
    fn is_tool_entry(&self) -> bool {
        self.tool_entry
    }

    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let mut lines = self.lines.clone();
        if let Some(timestamp) = self.timestamp
            && let Some(header) = lines.first_mut()
        {
            // Timestamped plain cells have a single leading result-marker span ("• ").
            header
                .spans
                .insert(1.min(header.spans.len()), timestamp.span());
        }
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.lines.clone())
    }
}

#[derive(Debug)]
pub(crate) struct WebHyperlinkHistoryCell {
    lines: Vec<HyperlinkLine>,
}

impl WebHyperlinkHistoryCell {
    pub(crate) fn new(lines: Vec<Line<'static>>) -> Self {
        Self {
            lines: crate::terminal_hyperlinks::annotate_web_urls(lines),
        }
    }

    pub(crate) fn new_hyperlink_lines(lines: Vec<HyperlinkLine>) -> Self {
        Self { lines }
    }
}

impl HistoryCell for WebHyperlinkHistoryCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines.iter().map(|line| line.line.clone()).collect()
    }

    fn display_hyperlink_lines(&self, _width: u16) -> Vec<HyperlinkLine> {
        self.lines.clone()
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.lines.iter().map(|line| line.line.clone()))
    }
}
#[derive(Debug)]
pub(crate) struct PrefixedWrappedHistoryCell {
    text: Text<'static>,
    initial_prefix: Line<'static>,
    subsequent_prefix: Line<'static>,
}

impl PrefixedWrappedHistoryCell {
    pub(crate) fn new(
        text: impl Into<Text<'static>>,
        initial_prefix: impl Into<Line<'static>>,
        subsequent_prefix: impl Into<Line<'static>>,
    ) -> Self {
        Self {
            text: text.into(),
            initial_prefix: initial_prefix.into(),
            subsequent_prefix: subsequent_prefix.into(),
        }
    }
}

impl HistoryCell for PrefixedWrappedHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.display_hyperlink_lines(width))
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        if width == 0 {
            return Vec::new();
        }
        let opts = RtOptions::new(usize::from(width))
            .initial_indent(self.initial_prefix.clone())
            .subsequent_indent(self.subsequent_prefix.clone());
        crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines(
            &plain_hyperlink_lines(self.text.lines.clone()),
            opts,
        )
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.text.clone().lines)
    }
}
#[derive(Debug)]
pub(crate) struct CompositeHistoryCell {
    pub(super) parts: Vec<Box<dyn HistoryCell>>,
}

impl CompositeHistoryCell {
    pub(crate) fn new(parts: Vec<Box<dyn HistoryCell>>) -> Self {
        Self { parts }
    }
}

impl HistoryCell for CompositeHistoryCell {
    fn warning_entries(&self) -> Vec<WarningEntry> {
        self.parts
            .iter()
            .flat_map(|part| part.warning_entries())
            .collect()
    }

    fn live_raw_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for part in &self.parts {
            let part = part.live_raw_lines();
            if !part.is_empty() {
                if !lines.is_empty() {
                    lines.push(Line::default());
                }
                lines.extend(part);
            }
        }
        lines
    }

    fn warning_keys(&self) -> Vec<WarningKey<'_>> {
        self.parts
            .iter()
            .flat_map(|part| part.warning_keys())
            .collect()
    }

    fn compact_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut out = Vec::new();
        let mut previous = None;
        for part in &self.parts {
            let lines = part.compact_hyperlink_lines(width);
            if !lines.is_empty() {
                if crate::transcript_geometry::separated(
                    previous,
                    part.is_tool_entry(),
                    part.is_stream_continuation(),
                ) {
                    out.push(HyperlinkLine::from(""));
                }
                out.extend(lines);
                previous = Some(part.as_ref());
            }
        }
        out
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();
        let mut previous = None;
        for part in &self.parts {
            let mut lines = part.display_lines(width);
            if !lines.is_empty() {
                if crate::transcript_geometry::separated(
                    previous,
                    part.is_tool_entry(),
                    part.is_stream_continuation(),
                ) {
                    out.push(Line::from(""));
                }
                out.append(&mut lines);
                previous = Some(part.as_ref());
            }
        }
        out
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut out = Vec::new();
        let mut previous = None;
        for part in &self.parts {
            let mut lines = part.display_hyperlink_lines(width);
            if !lines.is_empty() {
                if crate::transcript_geometry::separated(
                    previous,
                    part.is_tool_entry(),
                    part.is_stream_continuation(),
                ) {
                    out.push(HyperlinkLine::from(""));
                }
                out.append(&mut lines);
                previous = Some(part.as_ref());
            }
        }
        out
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut out = Vec::new();
        let mut previous = None;
        for part in &self.parts {
            let mut lines = part.transcript_hyperlink_lines(width);
            if !lines.is_empty() {
                if crate::transcript_geometry::separated(
                    previous,
                    part.is_tool_entry(),
                    part.is_stream_continuation(),
                ) {
                    out.push(HyperlinkLine::from(""));
                }
                out.append(&mut lines);
                previous = Some(part.as_ref());
            }
        }
        out
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();
        let mut first = true;
        for part in &self.parts {
            let mut lines = part.raw_lines();
            if !lines.is_empty() {
                if !first {
                    out.push(Line::from(""));
                }
                out.append(&mut lines);
                first = false;
            }
        }
        out
    }

    fn has_stable_transcript_height(&self) -> bool {
        false
    }
}

#[cfg(test)]
#[path = "base_tests.rs"]
mod tests;
