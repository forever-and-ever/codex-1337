//! Brackets: who said what the model sees. A row hangs from the speaker its line names, as the
//! producer that quoted it into the transcript says; headings, summaries and every other word the
//! client writes hang from none, so the brackets together span what entered the model's context.
//! A speaker's consecutive rows are one run: its stroke spans the run's words, opening on the
//! first row that shows any and closing on the last. Blank rows beyond a run's words are spacing,
//! unless its stroke runs on through them into the entry beside.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::style::Style;

use super::TextLayout;
use super::TextRow;
use crate::history_cell::output_provenance::OutputProvenance;
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
    /// Nothing: the client's own words, and the spacing between entries.
    Outside,
    /// The speaker whose words, as the model saw them, the row shows.
    Speaker(Bracket),
}

impl Hang {
    /// What the rows of `source`'s line hang from: the speaker its producer named, unless the
    /// client wrote the line itself.
    pub(super) fn of(source: &LogicalLineSource) -> Self {
        match source.bracket {
            Some(bracket) if source.output_provenance != OutputProvenance::Client => {
                Self::Speaker(bracket)
            }
            _ => Self::Outside,
        }
    }
}

impl TextLayout {
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

    /// Hang the rows' brackets, or not, beside the words.
    pub(in crate::transcript_view) fn with_brackets(mut self, brackets: bool) -> Self {
        self.brackets = brackets;
        self
    }

    fn bracket_at(&self, row: usize) -> Option<Bracket> {
        match self.rows.get(row)?.hang {
            Hang::Speaker(bracket) if self.brackets => Some(bracket),
            Hang::Speaker(_) | Hang::Outside => None,
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

    /// Whether `bracket`'s words lie along `rows` before another speaker's begin, or, where they
    /// run out at the layout's edge, its stroke runs on into `beyond`.
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

    fn paint_joined(&self, area: Rect, buf: &mut Buffer, start_row: usize, joins: Joins) {
        if area.width <= CONTROL_COLUMN {
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
            let glyph = match (above != Some(bracket), below != Some(bracket)) {
                (true, true) => strokes.closed,
                (true, false) => strokes.top,
                (false, true) => strokes.bottom,
                (false, false) => strokes.stem,
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
