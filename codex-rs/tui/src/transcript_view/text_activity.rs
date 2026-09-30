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
        self.with_leading_disclosure_at(label, offset)
    }

    pub(super) fn with_leading_disclosure_at(
        mut self,
        label: String,
        source_offset: usize,
    ) -> Self {
        if self.text.is_empty() {
            return self;
        }
        let row = self.row_for_offset(source_offset);
        self.disclosure = true;
        self.disclosure_control = Some(DisclosureControl {
            label,
            source_offset,
            row,
            placement: DisclosurePlacement::Leading,
        });
        self
    }

    pub(in crate::transcript_view) fn with_activity_estimate(self, label: String) -> Self {
        let Some(header) = self.logical.first() else {
            return self;
        };
        let mut offset = 0;
        for span in &header.line.line.spans {
            let action = span.content.split_whitespace().next().unwrap_or_default();
            if span.style.add_modifier.contains(Modifier::BOLD)
                && action.chars().any(char::is_alphabetic)
            {
                let offset = offset + action.len();
                return self.with_estimate_at(offset, label);
            }
            offset += span.content.len();
        }
        // Plain history cells can supply headers without semantic styling.
        if let Some(action) = self.text.split_whitespace().next()
            && matches!(action, "Called" | "Ran" | "Explored")
        {
            let offset = action.len();
            return self.with_estimate_at(offset, label);
        }
        self
    }

    pub(super) fn with_estimate_at(mut self, offset: usize, label: String) -> Self {
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
