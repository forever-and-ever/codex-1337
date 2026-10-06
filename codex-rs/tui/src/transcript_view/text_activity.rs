//! Heading decorations are visual only; controls and token bills never enter copied source.

use super::*;

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
    pub(in crate::transcript_view) fn with_header_tokens(mut self, tokens: Option<u64>) -> Self {
        self.heading.tokens = tokens;
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
        let offset = self
            .logical
            .iter()
            .take(source_line)
            .map(|line| line.origin.range.len() + 1)
            .sum();
        self.with_leading_disclosure_at(label, offset, FoldTarget::Activity)
    }

    pub(super) fn with_leading_disclosure_at(
        mut self,
        label: String,
        source_offset: usize,
        target: FoldTarget,
    ) -> Self {
        if self.text.is_empty() {
            return self;
        }
        let row = self.row_for_offset(source_offset);
        self.disclosure = true;
        self.disclosure_controls.push(DisclosureControl {
            target,
            label,
            source_offset,
            row,
            placement: DisclosurePlacement::Leading,
        });
        self
    }

    pub(in crate::transcript_view) fn with_part_control(
        self,
        part: crate::history_cell::activity_parts::PartControl,
    ) -> Self {
        let offset = self
            .logical
            .iter()
            .take(part.line)
            .map(|line| line.origin.range.len() + 1)
            .sum();
        self.with_leading_disclosure_at(
            if part.expanded { "[-]" } else { "[+]" }.into(),
            offset,
            FoldTarget::Part(part.id),
        )
    }

    pub(in crate::transcript_view) fn fold_targets(&self) -> Vec<FoldTarget> {
        let mut controls: Vec<_> = self.disclosure_controls.iter().collect();
        controls.sort_by_key(|control| (control.row, self.control_columns(control).start));
        controls
            .into_iter()
            .map(|control| control.target.clone())
            .collect()
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
    ) -> Option<(usize, std::ops::Range<u16>, usize)> {
        self.disclosure_controls
            .iter()
            .find(|control| &control.target == target)
            .map(|control| {
                (
                    control.row,
                    self.control_columns(control),
                    control.source_offset,
                )
            })
    }

    pub(in crate::transcript_view) fn with_inline_decoration_at(
        mut self,
        offset: usize,
        label: String,
    ) -> Self {
        let row_index = self.row_for_offset(offset.saturating_sub(1));
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
        let width = display_width(&label);
        let mut line = logical::slice_line(&row.line.line, 0..byte);
        line.push_span(label.clone().dim());
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
        self.heading.output_controls = Some((offset, label));
        self
    }
}
