//! Brackets: the stroke hung in a transcript block's gutter says who speaks there. Each speaker
//! draws a stroke of its own, in the pack's color for it.

use crate::warpaint::Bracket;

/// The glyphs one speaker's bracket is drawn with.
pub(crate) struct Strokes {
    /// A run's first row; alone, a folded block whose rows lie hidden beneath it.
    pub(crate) top: &'static str,
    pub(crate) stem: &'static str,
    pub(crate) bottom: &'static str,
    /// A row within a run that holds a fold of its own.
    pub(crate) junction: &'static str,
    /// A whole block on one row.
    pub(crate) closed: &'static str,
}

impl Strokes {
    /// The response rides a double rail, a message hangs from a serif, a call is stitched, reasoning
    /// is dashed; the user's prompts are rounded, and the harness that speaks in the user's stead
    /// rounded and dotted.
    pub(crate) const fn of(bracket: Bracket) -> Self {
        let (top, stem, bottom, junction, closed) = match bracket {
            Bracket::Response => ("╓", "║", "╙", "╟", "["),
            Bracket::Message => ("┍", "│", "┕", "┝", "["),
            Bracket::Call => ("┏", "┇", "┗", "┣", "["),
            Bracket::Reasoning => ("┌", "┆", "└", "├", "["),
            Bracket::Prompt => ("╭", "│", "╰", "├", "("),
            Bracket::Harness => ("╭", "┊", "╰", "├", "("),
        };
        Self {
            top,
            stem,
            bottom,
            junction,
            closed,
        }
    }
}
