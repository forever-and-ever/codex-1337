//! Heading decorations are visual only: disclosure and estimates never enter copied source.

use super::*;

impl TextRow {
    pub(super) fn estimate_column(&self, column: usize) -> usize {
        column
            + self
                .estimate
                .filter(|(start, _)| column >= *start)
                .map_or(0, |(_, width)| width)
    }
}

impl TextLayout {
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
        self.disclosure_controls
            .iter()
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

    pub(in crate::transcript_view) fn with_estimate_at(
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
        row.estimate = Some((column, width));
        self.activity_estimate = Some((offset, label));
        self
    }
}
