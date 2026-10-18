//! Allocation weights and equality for off-thread layout preparation.

use super::LogicalLine;
use super::TextLayout;
use super::TextRow;

impl TextLayout {
    pub(in crate::transcript_view) fn width(&self) -> u16 {
        self.width
    }

    pub(in crate::transcript_view) fn retained_bytes(&self) -> usize {
        // A conservative allocation weight, available without walking a large cell on
        // the UI task. Logical/display/source strings are charged independently.
        size_of::<Self>()
            + self.text.capacity() * 4
            + self.logical.capacity() * size_of::<LogicalLine>()
            + self.rows.capacity()
                * (size_of::<TextRow>() + 4 * size_of::<ratatui::text::Span<'static>>())
    }

    pub(in crate::transcript_view) fn same_presentation(&self, other: &Self) -> bool {
        self.text == other.text
            && self.width == other.width
            && self.separated == other.separated
            && self.disclosure == other.disclosure
            && self.rows.len() == other.rows.len()
            && self.rows.iter().zip(&other.rows).all(|(a, b)| {
                a.line.line == b.line.line
                    && a.line.hyperlinks == b.line.hyperlinks
                    && a.source == b.source
                    && a.line_end == b.line_end
                    && a.first_column == b.first_column
                    && a.prefix_columns == b.prefix_columns
            })
            && self.disclosure_controls.len() == other.disclosure_controls.len()
            && self
                .disclosure_controls
                .iter()
                .zip(&other.disclosure_controls)
                .all(|(a, b)| {
                    a.target == b.target
                        && a.label == b.label
                        && a.row == b.row
                        && a.source_offset == b.source_offset
                })
    }
}
