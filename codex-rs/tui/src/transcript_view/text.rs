//! Wrapped transcript text with source positions shared by painting and selection.
//!
//! Input lines with shared source provenance belong to the same logical line. Other input
//! lines introduce hard breaks. Display wrapping and synthetic controls never enter `text`.
//! Layout offsets use `usize`; terminal coordinates are narrowed only for visible rows.
//! Disclosure controls follow their activity's source text without changing its indentation.
//! Selected hard breaks on nonempty rows highlight one trailing cell when space permits.

use crate::history_cell::activity_parts::FoldTarget;
use crate::history_cell::output_provenance::OutputMode;
use std::borrow::Cow;
use std::ops::Range;

#[path = "text_logical.rs"]
mod logical;

#[path = "text_tabs.rs"]
mod tabs;

#[path = "text_activity.rs"]
mod activity;

#[path = "text_gutter.rs"]
mod gutter;

pub(super) use gutter::token_figure;
pub(super) use gutter::token_label;

use ratatui::buffer::Buffer;
use ratatui::layout::Alignment;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::widgets::Widget;
use unicode_segmentation::UnicodeSegmentation;

use crate::line_truncation::line_width;
use crate::line_truncation::truncate_line_to_width;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::HyperlinkParagraph;
use crate::terminal_hyperlinks::LineWrapPolicy;
use crate::terminal_hyperlinks::LogicalLineSource;
use crate::terminal_hyperlinks::remap_source_wrapped_line;
use crate::width::display_width;
use crate::wrapping::RtOptions;
use crate::wrapping::WrappedLine;
use crate::wrapping::adaptive_wrap_line_to_width;
use crate::wrapping::word_wrap_line_with_source;
use logical::LogicalLine;
use logical::logical_lines;
use ratatui::text::Line;

/// One cell's logical text and its width-dependent display rows.
pub(super) struct TextLayout {
    logical: Vec<LogicalLine>,
    text: String,
    rows: Vec<TextRow>,
    width: u16,
    pub(super) separated: bool,
    /// The leading gap is drawn as the rule above live work.
    ruled: bool,
    pub(super) disclosure: bool,
    disclosure_controls: Vec<DisclosureControl>,
    heading: HeadingDecorations,
    output_mode: Option<OutputMode>,
    feed_header: Option<HeaderRows>,
}

/// A Feed Item's synthetic header rows: its fold on the first, and a control their text draws.
#[derive(Clone)]
pub(super) struct HeaderRows {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) fold: FoldTarget,
    pub(super) unfolded: bool,
    pub(super) control: Option<HeaderControl>,
}

/// A control drawn in a header row's own text, such as a Code Mode title's `[js]`.
#[derive(Clone)]
pub(super) struct HeaderControl {
    pub(super) row: usize,
    pub(super) columns: Range<u16>,
    pub(super) target: FoldTarget,
}

#[derive(Default)]
struct HeadingDecorations {
    /// A loose row's tokens, and where they go.
    tokens: Option<(u64, crate::transcript_geometry::TokenSeat)>,
    output_controls: Option<(usize, String)>,
}

struct DisclosureControl {
    target: FoldTarget,
    label: String,
    source_offset: usize,
    row: usize,
    placement: DisclosurePlacement,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DisclosurePlacement {
    Row,
    Leading,
    /// A Feed Item's fold, after the clock on its header row.
    Fold,
    Output {
        start: usize,
        end: usize,
    },
    /// Drawn in its row's own text, which owns its label and style.
    Inline {
        start: u16,
        end: u16,
    },
}

struct TextRow {
    line: HyperlinkLine,
    source: Range<usize>,
    /// Source offset at the logical line end; absent on soft wraps and synthetic rows.
    line_end: Option<usize>,
    content_width: u16,
    first_column: usize,
    prefix_columns: usize,
    tabs: tabs::TabStops,
    inline_decoration: Option<(usize, usize)>,
}

impl TextLayout {
    pub(super) fn new(lines: Vec<HyperlinkLine>, width: u16) -> Self {
        let logical = logical_lines(&lines);
        Self::from_logical(logical, width)
    }

    /// Reflow the same displayed content revision, retaining styles, links and disclosure label.
    pub(super) fn rewrap(&self, width: u16) -> Self {
        let mut layout = Self::from_logical(self.logical.clone(), width);
        layout.heading.tokens = self.heading.tokens;
        if let Some((offset, label)) = &self.heading.output_controls {
            layout = layout.with_inline_decoration_at(*offset, label.clone());
        }
        for control in &self.disclosure_controls {
            layout = match control.placement {
                DisclosurePlacement::Row => {
                    layout.with_disclosure_control_at(control.label.clone(), control.source_offset)
                }
                DisclosurePlacement::Leading => layout.with_leading_disclosure_at(
                    control.label.clone(),
                    control.source_offset,
                    control.target.clone(),
                ),
                // The header row is synthetic and returns with its line below.
                DisclosurePlacement::Fold | DisclosurePlacement::Inline { .. } => layout,
                DisclosurePlacement::Output { start, end } => {
                    if let FoldTarget::OutputMode(mode) = control.target {
                        layout.with_output_target(
                            control.label.clone(),
                            control.source_offset,
                            mode,
                            start..end,
                        )
                    } else {
                        layout
                    }
                }
            };
        }
        layout.output_mode = self.output_mode;
        if let Some(rows) = &self.feed_header {
            layout = layout.with_fold_header(rows.clone());
        }
        match (self.separated, self.ruled) {
            (true, true) => layout.with_leading_rule(),
            (true, false) => layout.with_leading_separator(),
            (false, _) => layout,
        }
    }

    /// Place a control after an activity, before any following auxiliary source text.
    pub(super) fn with_disclosure_control_at(
        mut self,
        label: String,
        source_offset: usize,
    ) -> Self {
        if self.text.is_empty() || label.is_empty() {
            return self;
        }
        if let Some(control) = self.disclosure_controls.pop()
            && control.placement == DisclosurePlacement::Row
        {
            self.rows.remove(control.row);
        }
        let source_offset = self
            .text
            .floor_char_boundary(source_offset.min(self.text.len()));
        let control_row = self
            .rows
            .iter()
            .rposition(|row| row.source.end <= source_offset)
            .map_or(/*default*/ 0, |row| row + 1);
        let indent = usize::from(self.width / 4).min(/*other*/ 4);
        let line = truncate_line_to_width(
            Line::from(format!("{}{label}", " ".repeat(indent))).dim(),
            usize::from(self.width),
        );
        self.rows.insert(
            control_row,
            TextRow {
                line: HyperlinkLine::from(line),
                source: source_offset..source_offset,
                line_end: None,
                content_width: self.width,
                first_column: indent,
                prefix_columns: indent,
                tabs: tabs::TabStops::default(),
                inline_decoration: None,
            },
        );
        self.disclosure = true;
        self.disclosure_controls.push(DisclosureControl {
            target: FoldTarget::Activity,
            label,
            source_offset,
            row: control_row,
            placement: DisclosurePlacement::Row,
        });
        self
    }

    /// Add visual spacing between entries without changing any source position.
    pub(super) fn with_leading_separator(mut self) -> Self {
        if !self.separated {
            self = self.with_leading_spacer();
            self.separated = !self.rows.is_empty();
        }
        self
    }

    /// Reserve one presentation-only row before existing spacing and source text.
    pub(super) fn with_leading_spacer(mut self) -> Self {
        if !self.rows.is_empty() {
            self = self.with_leading_row(Line::default());
        }
        self
    }

    /// Insert a presentation-only row before all others, even in an entry without source.
    pub(super) fn with_leading_row(mut self, line: Line<'static>) -> Self {
        self.rows.insert(
            /*index*/ 0,
            TextRow {
                line: HyperlinkLine::from(line),
                source: 0..0,
                line_end: None,
                content_width: self.width,
                first_column: 0,
                prefix_columns: 0,
                tabs: tabs::TabStops::default(),
                inline_decoration: None,
            },
        );
        for control in &mut self.disclosure_controls {
            control.row += 1;
        }
        self
    }

    /// Draw the gap above the entry as a rule: live work follows it.
    pub(super) fn with_leading_rule(mut self) -> Self {
        if !self.separated && !self.rows.is_empty() {
            let width = usize::from(self.width);
            self = self.with_leading_row(Line::from("─".repeat(width)).dim());
            self.separated = true;
            self.ruled = true;
        }
        self
    }

    /// Source text between adjacent retained fragments; distinct logical lines have a hard break.
    pub(super) fn separator_after(&self, next: &Self) -> &str {
        if let Some(previous) = self.logical.last()
            && let Some(next) = next.logical.first()
            && std::sync::Arc::ptr_eq(&previous.origin.text, &next.origin.text)
            && let Some(gap) = previous
                .origin
                .text
                .get(previous.origin.range.end..next.origin.range.start)
        {
            gap
        } else {
            "\n"
        }
    }

    pub(super) fn copy_lines(
        &self,
        range: Range<usize>,
        separator: &str,
        output: &mut Vec<crate::markdown_copy::SelectedLine>,
    ) {
        let mut offset = 0;
        for (index, line) in self.logical.iter().enumerate() {
            let end = offset + line.origin.range.len();
            if range.start <= end && range.end >= offset {
                let selected = line.origin.range.start + range.start.saturating_sub(offset)
                    ..line.origin.range.start + range.end.min(end) - offset;
                crate::markdown_copy::SelectedLine::append(
                    output,
                    line.origin.clone(),
                    selected,
                    if index == 0 { separator } else { "\n" },
                );
            }
            offset = end + 1;
        }
    }

    pub(super) fn text(&self) -> &str {
        &self.text
    }

    pub(super) fn row_count(&self) -> usize {
        self.rows.len()
    }

    #[cfg(test)]
    pub(super) fn disclosure_row(&self) -> Option<usize> {
        self.disclosure_controls.first().map(|control| control.row)
    }

    #[cfg(test)]
    pub(super) fn disclosure_columns(&self) -> Range<u16> {
        let Some(control) = self.disclosure_controls.first() else {
            return 0..0;
        };
        self.control_columns(control)
    }

    fn control_columns(&self, control: &DisclosureControl) -> Range<u16> {
        let row = &self.rows[control.row];
        match control.placement {
            DisclosurePlacement::Row => {
                row.first_column as u16
                    ..line_width(&row.line.line).min(usize::from(self.width)) as u16
            }
            DisclosurePlacement::Leading | DisclosurePlacement::Fold => {
                crate::transcript_geometry::CONTROL_COLUMN.min(self.width)
                    ..(crate::transcript_geometry::CONTROL_COLUMN
                        + crate::transcript_geometry::CONTROL_COLUMNS)
                        .min(self.width)
            }
            DisclosurePlacement::Output { start, end } => {
                let anchor = row.inline_decoration.map_or(0, |(column, _)| column);
                (anchor + start).min(usize::from(self.width)) as u16
                    ..(anchor + end).min(usize::from(self.width)) as u16
            }
            DisclosurePlacement::Inline { start, end } => {
                start.min(self.width)..end.min(self.width)
            }
        }
    }

    /// Paint visible rows, including inherited styles on empty rows and trailing columns.
    pub(super) fn render(&self, area: Rect, buf: &mut Buffer, start_row: usize) {
        for (screen_row, row) in self.visible_rows(area, start_row) {
            let mut row_area =
                Rect::new(area.x, area.y + screen_row, area.width, /*height*/ 1);
            buf.set_style(row_area, row.line.line.style);
            row_area.width = row.content_width;
            HyperlinkParagraph::new(std::slice::from_ref(&row.line), row.line.line.style)
                .render(row_area, buf);
            if let Some((tokens, seat)) = self.heading.tokens
                && start_row + usize::from(screen_row) == self.row_for_offset(/*offset*/ 0)
            {
                let (span, column, columns) = match seat {
                    crate::transcript_geometry::TokenSeat::Gutter => (
                        gutter::token_span(tokens),
                        usize::from(crate::transcript_geometry::TOKEN_COLUMN),
                        usize::from(crate::transcript_geometry::TOKEN_COLUMNS),
                    ),
                    crate::transcript_geometry::TokenSeat::Trailing => {
                        let span = gutter::token_figure(
                            gutter::token_label(tokens),
                            tokens,
                            /*width*/ 0,
                        );
                        let columns = span.width();
                        (span, line_width(&row.line.line) + 2, columns)
                    }
                };
                if column + columns <= usize::from(area.width) {
                    buf.set_span(
                        area.x + column as u16,
                        area.y + screen_row,
                        &span,
                        columns as u16,
                    );
                }
            }
            for control in self.disclosure_controls.iter().filter(|control| {
                matches!(
                    control.placement,
                    DisclosurePlacement::Leading
                        | DisclosurePlacement::Fold
                        | DisclosurePlacement::Output { .. }
                ) && control.row == start_row + usize::from(screen_row)
            }) {
                let columns = self.control_columns(control);
                let mut label = Line::from(control.label.as_str()).dim();
                if let FoldTarget::OutputMode(mode) = control.target
                    && self.output_mode == Some(mode)
                {
                    label = label.style(crate::style::accent_style().underlined());
                }
                buf.set_line(
                    area.x + columns.start,
                    area.y + screen_row,
                    &label,
                    columns.end - columns.start,
                );
            }
        }
    }

    /// Resolve a cell coordinate to a grapheme boundary, clamping padding to the row's text.
    pub(super) fn position_at(&self, row: usize, column: u16) -> usize {
        let Some(row) = self.rows.get(row) else {
            return self.text.len();
        };
        let mut current_column = row.first_column;
        for (offset, grapheme) in
            self.text[row.source.clone()].grapheme_indices(/*is_extended*/ true)
        {
            if let Some((start, width)) = row.inline_decoration
                && current_column == start
            {
                current_column += width;
            }
            current_column += row.tabs.grapheme_width(offset, grapheme);
            if usize::from(column) < current_column {
                return row.source.start + offset;
            }
        }
        row.source.end
    }

    /// Find the display row containing a source position, including omitted wrapping whitespace.
    pub(super) fn row_for_offset(&self, offset: usize) -> usize {
        let before_control = if let Some(control) = self.disclosure_controls.first()
            && control.placement == DisclosurePlacement::Row
        {
            let control_row = control.row;
            let after_control = &self.rows[control_row + 1..];
            let matching_rows = after_control.partition_point(|row| row.source.start <= offset);
            if matching_rows > 0 {
                return control_row + matching_rows;
            }
            control_row
        } else {
            self.rows.len()
        };
        self.rows[..before_control]
            .partition_point(|row| row.source.start <= offset)
            .saturating_sub(/*rhs*/ 1)
    }

    pub(super) fn column_for_offset(&self, offset: usize) -> u16 {
        let Some(row) = self.rows.get(self.row_for_offset(offset)) else {
            return 0;
        };
        let offset = self
            .text
            .floor_char_boundary(offset.clamp(row.source.start, row.source.end));
        let column = row.first_column
            + row.tabs.column_for_offset(
                display_width(&self.text[row.source.start..offset]),
                offset - row.source.start,
            );
        row.decoration_column(column).min(usize::from(self.width)) as u16
    }

    /// Highlight source graphemes only, leaving alignment and viewport padding untouched.
    pub(super) fn highlight(
        &self,
        range: Range<usize>,
        area: Rect,
        buf: &mut Buffer,
        start_row: usize,
    ) {
        for (screen_row, row) in self.visible_rows(area, start_row) {
            let mut column = row.first_column;
            for (offset, grapheme) in
                self.text[row.source.clone()].grapheme_indices(/*is_extended*/ true)
            {
                if let Some((start, width)) = row.inline_decoration
                    && column == start
                {
                    column += width;
                }
                let start = row.source.start + offset;
                let width = row.tabs.grapheme_width(offset, grapheme);
                if start < range.end && start + grapheme.len() > range.start {
                    for selected_column in
                        column..(column + width).min(usize::from(row.content_width))
                    {
                        buf[(area.x + selected_column as u16, area.y + screen_row)]
                            .set_style(Style::default().add_modifier(Modifier::REVERSED));
                    }
                }
                column += width;
            }
        }
    }

    /// Mark selected hard breaks on nonempty rows, including a copied separator to `next`.
    pub(super) fn highlight_selection(
        &self,
        range: Range<usize>,
        next: Option<&Self>,
        area: Rect,
        buf: &mut Buffer,
        start_row: usize,
    ) {
        self.highlight(range.clone(), area, buf, start_row);
        for (screen_row, row) in self.visible_rows(area, start_row) {
            if row.source.is_empty() {
                continue;
            }
            let Some(end) = row.line_end else { continue };
            let selected = range.contains(&end)
                || (end == self.text.len()
                    && next.is_some_and(|next| self.separator_after(next).contains('\n')));
            let column = row.decoration_column(
                row.first_column
                    + row.tabs.column_for_offset(
                        display_width(&self.text[row.source.clone()]),
                        row.source.len(),
                    ),
            );
            if selected && column < usize::from(self.width) {
                buf[(area.x + column as u16, area.y + screen_row)]
                    .set_style(Style::default().add_modifier(Modifier::REVERSED));
            }
        }
    }

    /// Resolve the displayed link using the same destination policy as terminal OSC-8 output.
    pub(super) fn link_at(&self, row: usize, column: u16) -> Option<String> {
        let row = self.rows.get(row)?;
        let column = usize::from(column).checked_sub(row.first_column - row.prefix_columns)?;
        row.line
            .hyperlinks
            .iter()
            .find(|link| link.columns.contains(&column))?
            .terminal_destination()
    }

    fn visible_rows(&self, area: Rect, start_row: usize) -> impl Iterator<Item = (u16, &TextRow)> {
        let height = if area.width == 0 { 0 } else { area.height };
        debug_assert!(area.width == 0 || area.width == self.width);
        self.rows
            .iter()
            .skip(start_row)
            .take(usize::from(height))
            .enumerate()
            .map(|(row, text)| (row as u16, text))
    }

    fn from_logical(logical: Vec<LogicalLine>, width: u16) -> Self {
        let width = width.max(/*other*/ 1);
        let mut text = String::new();
        let mut rows = Vec::new();
        let mut has_source_line = false;
        for line in &logical {
            let client_label = line.is_client_label();
            if has_source_line && !client_label {
                text.push('\n');
            }
            let start = text.len();
            text.extend(
                line.line
                    .line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref()),
            );
            rows.extend(layout_line(line, start, width));
            has_source_line |= !client_label;
            if !client_label && let Some(row) = rows.last_mut() {
                row.line_end = Some(text.len());
            }
        }
        Self {
            logical,
            text,
            rows,
            width,
            separated: false,
            ruled: false,
            disclosure: false,
            disclosure_controls: Vec::new(),
            output_mode: None,
            heading: HeadingDecorations::default(),
            feed_header: None,
        }
    }
}

fn layout_line(logical: &LogicalLine, text_start: usize, width: u16) -> Vec<TextRow> {
    let content_width = width.saturating_sub(logical.right_reserve).max(/*other*/ 1);
    // Client labels are painted text, not selectable source. Wrap the synthetic text itself,
    // rather than discarding it as an oversized indentation on narrow terminals.
    if logical.is_client_label() {
        let subsequent_indent = if logical.subsequent_indent.width() < usize::from(content_width) {
            logical.subsequent_indent.clone()
        } else {
            Line::default()
        };
        return word_wrap_line_with_source(
            &logical.initial_indent,
            RtOptions::new(usize::from(content_width)).subsequent_indent(subsequent_indent),
        )
        .into_iter()
        .map(|wrapped| {
            let mut line =
                HyperlinkLine::new(crate::render::line_utils::line_to_static(&wrapped.line));
            let mut source = logical.origin.clone();
            source.prefix_bytes = line.line.spans.iter().map(|span| span.content.len()).sum();
            line.source = Some(source);
            TextRow {
                line,
                source: text_start..text_start,
                line_end: None,
                content_width,
                first_column: 0,
                prefix_columns: 0,
                tabs: tabs::TabStops::default(),
                inline_decoration: None,
            }
        })
        .collect();
    }
    let mut logical = Cow::Borrowed(logical);
    if line_width(&logical.initial_indent) >= usize::from(content_width)
        || line_width(&logical.subsequent_indent) >= usize::from(content_width)
    {
        let logical = logical.to_mut();
        logical.initial_indent = Line::default();
        logical.subsequent_indent = Line::default();
    }
    let mut line = logical.line.clone();
    line.source = Some(LogicalLineSource::new(
        line.line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect(),
    ));
    let options = RtOptions::new(usize::from(content_width))
        .initial_indent(logical.initial_indent.clone())
        .subsequent_indent(logical.subsequent_indent.clone());
    let (wrapped, tab_stops) = if line
        .line
        .spans
        .iter()
        .any(|span| span.content.contains('\t'))
    {
        tabs::wrap_line(&logical, usize::from(content_width))
    } else {
        let wrapped = match logical.wrap_policy {
            LineWrapPolicy::Word => word_wrap_line_with_source(&line.line, options),
            LineWrapPolicy::UrlAware => adaptive_wrap_line_to_width(&line.line, options),
            LineWrapPolicy::Hard => hard_wrap_line(&line.line, options),
        };
        let stops = (0..wrapped.len())
            .map(|_| tabs::TabStops::default())
            .collect();
        (wrapped, stops)
    };
    // This source covers the whole logical line, so wrapping already supplies final row offsets.
    let positions = wrapped
        .iter()
        .map(|row| (row.range.clone(), row.prefix_bytes))
        .collect::<Vec<_>>();
    remap_source_wrapped_line(&line, wrapped)
        .into_iter()
        .zip(positions)
        .zip(tab_stops)
        .map(|((mut wrapped, (source, prefix_bytes)), tabs)| {
            wrapped.line.alignment = line.line.alignment;
            for link in &mut wrapped.hyperlinks {
                link.columns = tabs.project_link(link.columns.clone());
            }
            let displayed: String = wrapped
                .line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            let prefix_columns = display_width(&displayed[..prefix_bytes]);
            let columns = usize::from(content_width);
            let text_width = display_width(&displayed);
            let first_column = match line.line.alignment.unwrap_or(Alignment::Left) {
                Alignment::Left => 0,
                Alignment::Center => (columns / 2).saturating_sub(text_width / 2),
                Alignment::Right => columns.saturating_sub(text_width),
            };
            let range = text_start + source.start..text_start + source.end;
            TextRow {
                line: wrapped,
                source: range,
                line_end: None,
                content_width,
                first_column: first_column + prefix_columns,
                prefix_columns,
                tabs,
                inline_decoration: None,
            }
        })
        .collect()
}

/// Reuse the diff renderer's hard wrapping while preserving its distinct first/continuation gutters.
fn hard_wrap_line(line: &Line<'static>, options: RtOptions<'static>) -> Vec<WrappedLine<'static>> {
    let first_width = options
        .width
        .saturating_sub(options.initial_indent.width())
        .max(/*other*/ 1);
    let first = crate::diff_render::wrap_styled_spans(&line.spans, first_width)
        .into_iter()
        .next()
        .unwrap_or_default();
    let mut offset = first.iter().map(|span| span.content.len()).sum();
    let prefix_bytes = options
        .initial_indent
        .spans
        .iter()
        .map(|span| span.content.len())
        .sum();
    let mut initial = options.initial_indent.clone().style(line.style);
    initial.spans.extend(first);
    let mut out = vec![WrappedLine {
        line: initial,
        range: 0..offset,
        prefix_bytes,
    }];
    let total = line.spans.iter().map(|span| span.content.len()).sum();
    let remainder = logical::slice_line(line, offset..total);
    let continuation_width = options
        .width
        .saturating_sub(options.subsequent_indent.width())
        .max(/*other*/ 1);
    for chunk in crate::diff_render::wrap_styled_spans(&remainder.spans, continuation_width) {
        let length: usize = chunk.iter().map(|span| span.content.len()).sum();
        if length == 0 {
            continue;
        }
        let mut continuation = options.subsequent_indent.clone().style(line.style);
        let prefix_bytes = continuation
            .spans
            .iter()
            .map(|span| span.content.len())
            .sum();
        continuation.spans.extend(chunk);
        out.push(WrappedLine {
            line: continuation,
            range: offset..offset + length,
            prefix_bytes,
        });
        offset += length;
    }
    out
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "diff_source_tests.rs"]
mod diff_source_tests;

#[cfg(test)]
#[path = "source_cell_tests.rs"]
mod source_cell_tests;

#[cfg(test)]
#[path = "prompt_style_tests.rs"]
mod prompt_style_tests;

#[cfg(test)]
#[path = "text_tabs_tests.rs"]
mod tab_tests;
