//! A rich entry's bracket. Each row hangs from the speaker its line names, by its producer or by its
//! provenance, or from the entry's own bracket until a line names one; the client's words hang from
//! whoever spoke before them. A speaker's consecutive rows are one run: its stroke spans the run's
//! words, opening on the first row that shows any and closing on the last, and branches where a row
//! holds a fold of its own. Blank rows beyond a run's words are spacing, unless its stroke runs on
//! through them into the entry beside.

use std::ops::Range;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::style::Style;

use super::DisclosurePlacement;
use super::FoldTarget;
use super::TextLayout;
use super::TextRow;
use super::line_width;
use crate::render::bracket::Strokes;
use crate::terminal_hyperlinks::LogicalLineSource;
use crate::transcript_geometry::CONTROL_COLUMN;
use crate::warpaint::Bracket;

/// The brackets of the rows beside a layout's edges that its stroke may run on into: the last row
/// of the entry above, when this one continues it, and the first row of the entry below, when that
/// one continues this. A stroke runs on only into its own bracket.
#[derive(Clone, Copy, Default)]
pub(in crate::transcript_view) struct Joins {
    pub(in crate::transcript_view) above: Option<Bracket>,
    pub(in crate::transcript_view) below: Option<Bracket>,
}

/// What a row hangs from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Hang {
    /// Nothing: the spacer between entries.
    Outside,
    /// The entry's own bracket, until a line names a speaker.
    Block,
    /// The speaker a line named, and every row after it until another line names one.
    Speaker(Bracket),
}

impl Hang {
    /// What the rows of `source`'s line hang from, after rows that hung from `self`.
    pub(super) fn after(self, source: &LogicalLineSource) -> Self {
        source
            .bracket
            .or_else(|| source.output_provenance.bracket())
            .map_or(self, Self::Speaker)
    }
}

impl TextLayout {
    /// Hang this entry's rows from `bracket` in the gutter; an entry with none hangs no bracket.
    pub(in crate::transcript_view) fn with_bracket(mut self, bracket: Option<Bracket>) -> Self {
        self.bracket = bracket;
        self
    }

    /// Where a focused fold that a bracket draws is underlined: its row, and the row's words beside
    /// the glyph. Folds the bracket does not draw keep their own controls.
    pub(in crate::transcript_view) fn bracket_focus(
        &self,
        target: &FoldTarget,
    ) -> Option<(usize, Range<u16>)> {
        self.bracket?;
        let control = self.disclosure_controls.iter().find(|control| {
            control.target.same_control(target)
                && matches!(
                    control.placement,
                    DisclosurePlacement::Fold { .. } | DisclosurePlacement::Leading { .. }
                )
        })?;
        let row = self.rows.get(control.row)?;
        let end =
            u16::try_from(line_width(&row.line.line)).map_or(self.width, |end| end.min(self.width));
        let start = u16::try_from(row.first_column).map_or(end, |start| start.min(end));
        Some((control.row, start..end))
    }

    /// The bracket whose stroke reaches the first row, from words below it in its run.
    pub(in crate::transcript_view) fn first_bracket(&self) -> Option<Bracket> {
        let bracket = self.bracket_at(0)?;
        self.speaks_along(bracket, 0..self.rows.len(), None)
            .then_some(bracket)
    }

    /// The bracket the first row hangs from, whether or not its run shows words.
    pub(in crate::transcript_view) fn top_bracket(&self) -> Option<Bracket> {
        self.bracket_at(0)
    }

    /// The bracket every row hangs from when none shows words: a stream's blank fragment.
    pub(in crate::transcript_view) fn blank_bracket(&self) -> Option<Bracket> {
        let bracket = self.bracket_at(0)?;
        (0..self.rows.len())
            .all(|row| self.bracket_at(row) == Some(bracket) && !self.rows[row].inked())
            .then_some(bracket)
    }

    /// The bracket whose stroke reaches the last row, from words above it in its run.
    pub(in crate::transcript_view) fn last_bracket(&self) -> Option<Bracket> {
        let last = self.rows.len().checked_sub(1)?;
        let bracket = self.bracket_at(last)?;
        self.speaks_along(bracket, (0..=last).rev(), None)
            .then_some(bracket)
    }

    /// Whether this layout, drawn directly below `upper`, is the body of the open header `upper`
    /// ends in: nothing of `upper`'s own entry shows below its header, and this one heads nothing.
    pub(in crate::transcript_view) fn headed_by(&self, upper: &TextLayout) -> bool {
        self.feed_header.is_none()
            && upper.feed_header.as_ref().is_some_and(|header| {
                header.fold.as_ref().is_none_or(|(_, unfolded)| *unfolded)
                    && upper
                        .rows
                        .last()
                        .and_then(|row| row.logical_line)
                        .is_some_and(|line| line < header.lines.len())
            })
    }

    fn bracket_at(&self, row: usize) -> Option<Bracket> {
        self.bracket?;
        match self.rows.get(row)?.hang {
            Hang::Outside => None,
            Hang::Block => self.bracket,
            Hang::Speaker(bracket) => Some(bracket),
        }
    }

    /// The bracket whose stroke `row` draws: its own, where it shows words or lies between its
    /// speaker's words, here or beyond an edge the stroke runs on across.
    fn stroke_at(&self, row: usize, joins: Joins) -> Option<Bracket> {
        let bracket = self.bracket_at(row)?;
        (self.rows[row].inked()
            || self.speaks_along(bracket, (0..row).rev(), joins.above)
                && self.speaks_along(bracket, row + 1..self.rows.len(), joins.below))
        .then_some(bracket)
    }

    /// Whether `bracket`'s words lie along `rows` before another speaker's begin, or, where they run
    /// out at the layout's edge, its stroke runs on into `beyond`.
    fn speaks_along(
        &self,
        bracket: Bracket,
        mut rows: impl Iterator<Item = usize>,
        beyond: Option<Bracket>,
    ) -> bool {
        rows.find(|row| self.bracket_at(*row) != Some(bracket) || self.rows[*row].inked())
            .map_or(beyond == Some(bracket), |row| {
                self.bracket_at(row) == Some(bracket)
            })
    }

    /// Whether a fold sits on `row`, and whether it is open.
    fn fold_on(&self, row: usize) -> Option<bool> {
        self.disclosure_controls
            .iter()
            .find_map(|control| match control.placement {
                DisclosurePlacement::Fold { expanded }
                | DisclosurePlacement::Leading { expanded, .. }
                    if control.row == row =>
                {
                    Some(expanded)
                }
                _ => None,
            })
    }

    /// Repaint an edge row's bracket where its stroke runs on into the entry beside it.
    pub(in crate::transcript_view) fn join_brackets(
        &self,
        area: Rect,
        buf: &mut Buffer,
        row: usize,
        joins: Joins,
    ) {
        self.paint_joined(area, buf, row, joins);
    }

    /// Paint each visible row's bracket in the control column.
    pub(super) fn paint_brackets(&self, area: Rect, buf: &mut Buffer, start_row: usize) {
        self.paint_joined(area, buf, start_row, Joins::default());
    }

    fn paint_joined(&self, area: Rect, buf: &mut Buffer, start_row: usize, joins: Joins) {
        if self.bracket.is_none() || area.width <= CONTROL_COLUMN {
            return;
        }
        for (screen_row, text) in self.visible_rows(area, start_row) {
            let row = start_row + usize::from(screen_row);
            // A row too narrow for its gutter starts its words at the edge; they keep the column.
            let Some(bracket) = self
                .stroke_at(row, joins)
                .filter(|_| text.first_column > usize::from(CONTROL_COLUMN))
            else {
                continue;
            };
            let strokes = Strokes::of(bracket);
            let above = row
                .checked_sub(1)
                .map_or(joins.above, |above| self.stroke_at(above, joins));
            let below = if row + 1 < self.rows.len() {
                self.stroke_at(row + 1, joins)
            } else {
                joins.below
            };
            let opens = above != Some(bracket);
            let closes = below != Some(bracket);
            let glyph = match (opens, closes, self.fold_on(row)) {
                (true, true, Some(false)) | (true, false, _) => strokes.top,
                (true, true, _) => strokes.closed,
                (false, _, Some(_)) => strokes.junction,
                (false, true, None) => strokes.bottom,
                (false, false, None) => strokes.stem,
            };
            buf[(area.x + CONTROL_COLUMN, area.y + screen_row)]
                .set_symbol(glyph)
                .set_style(
                    Style::new()
                        .fg(bracket.color())
                        .remove_modifier(Modifier::all()),
                );
        }
    }
}

impl TextRow {
    /// Whether the row shows any words; a blank row draws a stroke only between them.
    fn inked(&self) -> bool {
        self.line
            .line
            .spans
            .iter()
            .any(|span| span.content.chars().any(|ch| !ch.is_whitespace()))
    }
}
