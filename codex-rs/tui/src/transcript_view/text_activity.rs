//! Headings retain selectable text; interactive controls stay outside copied source.

use super::DisclosureControl;
use super::DisclosurePlacement;
use super::FoldTarget;
use super::HeaderControl;
use super::HeaderRows;
use super::HyperlinkLine;
use super::InlineAnchor;
use super::Line;
use super::LogicalLineSource;
use super::OutputMode;
use super::Range;
use super::Stylize;
use super::TextLayout;
use super::TextRow;
use super::display_width;
use super::logical;
use super::truncate_line_to_width;

fn byte_at_column(text: &str, column: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    let mut width = 0;
    for (offset, grapheme) in text.grapheme_indices(true) {
        if width >= column {
            return offset;
        }
        width += display_width(grapheme);
    }
    text.len()
}

impl HeaderRows {
    /// Source belongs only to the label; clocks, figures and controls are decoration.
    pub(in crate::transcript_view) fn label(
        line: Line<'static>,
        columns: Range<usize>,
    ) -> HyperlinkLine {
        let text = line.to_string();
        let start = byte_at_column(&text, columns.start);
        let end = byte_at_column(&text, columns.end);
        let mut source = LogicalLineSource::from_line(&logical::slice_line(&line, start..end));
        source.prefix_bytes = start;
        let mut label = HyperlinkLine::from(line);
        label.source = Some(source);
        crate::history_cell::output_provenance::mark_lines(
            std::slice::from_mut(&mut label),
            crate::history_cell::output_provenance::OutputProvenance::Client,
        );
        label
    }
}

impl TextRow {
    pub(super) fn decoration_column(&self, column: usize) -> usize {
        column
            + self
                .inline_decoration
                .filter(|(start, _)| column >= *start)
                .map_or(0, |(_, width)| width)
    }
}

impl TextLayout {
    pub(in crate::transcript_view) fn with_header_tokens(
        mut self,
        tokens: Option<u64>,
        seat: crate::transcript_geometry::TokenSeat,
    ) -> Self {
        self.heading.tokens = tokens.map(|tokens| (tokens, seat));
        self
    }

    pub(in crate::transcript_view) fn with_output_controls(
        mut self,
        offset: usize,
        mode: OutputMode,
    ) -> Self {
        let mut decoration = String::new();
        let mut controls = Vec::new();
        for (mode, label) in [(OutputMode::Raw, "[Raw]"), (OutputMode::Fmt, "[Fmt]")] {
            decoration.push(' ');
            let start = display_width(&decoration);
            decoration.push_str(label);
            controls.push((mode, label, start..display_width(&decoration)));
        }
        self = self.with_inline_decoration_at(offset, decoration);
        for (mode, label, columns) in controls {
            self = self.with_output_target(label.into(), offset, mode, columns);
        }
        self.output_mode = Some(mode);
        self
    }

    pub(super) fn with_output_target(
        mut self,
        label: String,
        source_offset: usize,
        mode: OutputMode,
        columns: Range<usize>,
    ) -> Self {
        let row = self.row_for_offset(source_offset.saturating_sub(1));
        self.disclosure_controls.push(DisclosureControl {
            target: FoldTarget::OutputMode(mode),
            label,
            source_offset,
            row,
            placement: DisclosurePlacement::Output {
                start: columns.start,
                end: columns.end,
            },
        });
        self
    }
    pub(in crate::transcript_view) fn with_leading_disclosure_control(
        self,
        label: String,
        source_line: usize,
    ) -> Self {
        self.with_leading_disclosure_on_line(label, source_line, FoldTarget::Activity)
    }

    /// Selectable-source offset of an input row.
    pub(in crate::transcript_view) fn line_offset(&self, line: usize) -> usize {
        self.row_for_input_line(line)
            .map_or(self.text.len(), |row| self.rows[row].source.start)
    }

    fn row_for_input_line(&self, line: usize) -> Option<usize> {
        let logical = self
            .logical
            .iter()
            .position(|entry| entry.input_rows.contains(&line))?;
        self.rows
            .iter()
            .position(|row| row.logical_line == Some(logical))
    }

    pub(super) fn with_leading_disclosure_on_line(
        mut self,
        label: String,
        line: usize,
        target: FoldTarget,
    ) -> Self {
        // A source offset cannot distinguish empty rows, and wrapped input
        // rows may coalesce. Retain the supplied row through that projection.
        let Some(row) = self.row_for_input_line(line) else {
            return self;
        };
        self.disclosure = true;
        self.disclosure_controls.push(DisclosureControl {
            target,
            label,
            source_offset: self.rows[row].source.start,
            row,
            placement: DisclosurePlacement::Leading { line },
        });
        self
    }

    pub(in crate::transcript_view) fn with_part_control(
        self,
        part: crate::history_cell::activity_parts::PartControl,
    ) -> Self {
        self.with_leading_disclosure_on_line(
            crate::render::disclosure::glyph(part.expanded).into(),
            part.line,
            FoldTarget::GroupedPart {
                id: part.id,
                group: part.group,
            },
        )
    }

    pub(in crate::transcript_view) fn with_invocation_control(
        mut self,
        control: HeaderControl,
    ) -> Self {
        if let Some(row) = self.row_for_input_line(control.row) {
            self.disclosure = true;
            self.disclosure_controls.push(DisclosureControl {
                target: control.target,
                label: String::new(),
                source_offset: self.rows[row].source.start,
                row,
                placement: DisclosurePlacement::Invocation {
                    line: control.row,
                    start: control.columns.start,
                    end: control.columns.end,
                },
            });
        }
        self
    }

    /// Head the layout with a Feed Item's header rows, its fold after the clock.
    pub(in crate::transcript_view) fn with_fold_header(self, rows: HeaderRows) -> Self {
        // Headers are already laid out; frozen headers clip on resize rather than rewrapping chrome.
        let mut header = TextLayout::new(rows.lines.clone(), u16::MAX);
        let mut layout = self;
        let prefix = header.text.len() + usize::from(!layout.logical.is_empty());
        for line in &mut header.logical {
            line.input_rows = usize::MAX..usize::MAX;
        }
        for row in &mut header.rows {
            row.content_width = layout.width;
        }
        for row in &mut layout.rows {
            row.source.start += prefix;
            row.source.end += prefix;
            row.line_end = row.line_end.map(|offset| offset + prefix);
            row.logical_line = row.logical_line.map(|index| index + header.logical.len());
        }
        header.logical.extend(layout.logical);
        layout.logical = header.logical;
        if !layout.text.is_empty() || layout.logical.len() > rows.lines.len() {
            header.text.push('\n');
        }
        header.text.push_str(&layout.text);
        layout.text = header.text;
        header.rows.extend(layout.rows);
        layout.rows = header.rows;
        for (anchor, _) in &mut layout.heading.inline {
            if let InlineAnchor::Source(offset) = anchor {
                *offset += prefix;
            }
        }
        for control in &mut layout.disclosure_controls {
            control.row += rows.lines.len();
            control.source_offset += prefix;
        }
        for (index, line) in rows.lines.iter().enumerate() {
            layout.rows[index].line = line.clone();
        }
        if let Some((target, unfolded)) = &rows.fold {
            layout.disclosure = true;
            layout.disclosure_controls.push(DisclosureControl {
                target: target.clone(),
                label: crate::render::disclosure::glyph(*unfolded).into(),
                source_offset: 0,
                row: 0,
                placement: DisclosurePlacement::Fold,
            });
        }
        for control in &rows.controls {
            layout.disclosure = true;
            layout.disclosure_controls.push(DisclosureControl {
                target: control.target.clone(),
                label: String::new(),
                source_offset: 0,
                row: control.row,
                placement: DisclosurePlacement::Inline {
                    start: control.columns.start,
                    end: control.columns.end,
                },
            });
        }
        layout.feed_header = Some(rows);
        layout
    }

    pub(in crate::transcript_view) fn fold_targets(&self) -> Vec<FoldTarget> {
        let mut controls: Vec<_> = self.disclosure_controls.iter().collect();
        controls.sort_by_key(|control| (control.row, self.control_columns(control).start));
        controls
            .into_iter()
            .map(|control| control.target.clone())
            .collect()
    }

    pub(in crate::transcript_view) fn is_feed_header_control(&self, target: &FoldTarget) -> bool {
        self.feed_header.as_ref().is_some_and(|header| {
            header
                .controls
                .iter()
                .any(|control| control.target.same_control(target))
        })
    }

    pub(in crate::transcript_view) fn fold_at(
        &self,
        row: usize,
        column: u16,
    ) -> Option<FoldTarget> {
        self.disclosure_controls
            .iter()
            .find(|control| control.row == row && self.control_columns(control).contains(&column))
            .map(|control| control.target.clone())
    }

    pub(in crate::transcript_view) fn fold_position(
        &self,
        target: &FoldTarget,
    ) -> Option<(usize, Range<u16>, usize)> {
        self.disclosure_controls
            .iter()
            .find(|control| control.target.same_control(target))
            .map(|control| {
                (
                    control.row,
                    self.control_columns(control),
                    control.source_offset,
                )
            })
    }

    pub(in crate::transcript_view) fn with_inline_decoration_at(
        self,
        offset: usize,
        label: String,
    ) -> Self {
        self.with_inline_line_at(InlineAnchor::Source(offset), Line::from(label).dim())
    }

    pub(in crate::transcript_view) fn with_invocation_rows(
        mut lines: Vec<HyperlinkLine>,
        width: u16,
        controls: Vec<HeaderControl>,
    ) -> Self {
        let mut decorations = Vec::new();
        for (index, original) in lines.iter_mut().enumerate() {
            let Some(source) = &original.source else {
                continue;
            };
            let end = source.prefix_bytes + source.range.len();
            if end >= original.line.to_string().len() {
                continue;
            }
            let label = logical::slice_line(&original.line, end..usize::MAX);
            original.line = logical::slice_line(&original.line, 0..end);
            decorations.push((index, source.range.len(), label));
        }
        let mut layout = Self::new(lines, width);
        for (line, column, label) in decorations {
            layout = layout.with_inline_line_at(
                InlineAnchor::InputLine {
                    line,
                    offset: column,
                },
                label,
            );
        }
        for control in controls {
            layout = layout.with_invocation_control(control);
        }
        layout
    }

    pub(super) fn with_inline_line_at(
        mut self,
        anchor: InlineAnchor,
        label: Line<'static>,
    ) -> Self {
        let (offset, row_index) = match anchor {
            InlineAnchor::Source(offset) => (offset, self.row_for_offset(offset.saturating_sub(1))),
            InlineAnchor::InputLine { line, offset } => {
                let Some(row) = self.row_for_input_line(line) else {
                    return self;
                };
                (self.rows[row].source.start + offset, row)
            }
        };
        let Some(row) = self.rows.get_mut(row_index) else {
            return self;
        };
        let offset = offset.clamp(row.source.start, row.source.end);
        let byte = row
            .line
            .source
            .as_ref()
            .map_or(0, |source| source.prefix_bytes)
            + offset
            - row.source.start;
        let column = row.first_column + display_width(&self.text[row.source.start..offset]);
        let width = label.width();
        let mut line = logical::slice_line(&row.line.line, 0..byte);
        line.extend(label.spans.clone());
        line.extend(logical::slice_line(&row.line.line, byte..usize::MAX).spans);
        row.line.line = truncate_line_to_width(line, usize::from(self.width));
        for link in &mut row.line.hyperlinks {
            if link.columns.start >= column {
                link.columns.start += width;
            }
            if link.columns.end > column {
                link.columns.end += width;
            }
        }
        row.inline_decoration = Some((column, width));
        self.heading.inline.push((anchor, label));
        self
    }
}
