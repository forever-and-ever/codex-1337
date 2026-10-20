//! Rich transcript geometry. Clocks, controls and token figures never enter selectable source.
//! Message text, Feed Item titles and Activity headings share `TITLE_COLUMN`. Other row content
//! starts at `GUTTER_COLUMNS`, two columns to their right, whether an item claims the row or not;
//! nothing moves sideways when the feed regroups a cell or the view turns detailed. A block's
//! bracket hangs in the control column, its folds drawn in its strokes. The normal view trails a
//! row's tokens after its first line; the detailed view puts them beside the bracket.
//! Marker-bearing content entries share the content column; cards retain cell-owned markers.

use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryRenderMode;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::prefix_hyperlink_lines;

/// A two-column margin, the clock and a space: no row starts against the edge.
pub(crate) const CLOCK_COLUMNS: u16 = 8;
/// A block's bracket, which draws its header's fold and the folds of its rows.
pub(crate) const CONTROL_COLUMN: u16 = CLOCK_COLUMNS;
pub(crate) const CONTROL_COLUMNS: u16 = 3;
/// The detailed view's tokens stand beside the bracket.
pub(crate) const TOKEN_COLUMN: u16 = CONTROL_COLUMN + 1;
pub(crate) const TOKEN_COLUMNS: u16 = 5;
/// A Feed Item header's title.
pub(crate) const TITLE_COLUMN: u16 = CONTROL_COLUMN + CONTROL_COLUMNS + 1;
/// Each entry's body hangs this far from its header.
pub(crate) const ENTRY_INDENT: u16 = 2;
/// Every top-level body starts here.
pub(crate) const GUTTER_COLUMNS: u16 = TITLE_COLUMN + ENTRY_INDENT;
pub(crate) const MARKER_COLUMNS: u16 = 2;
pub(crate) const BODY_COLUMN: u16 = GUTTER_COLUMNS + MARKER_COLUMNS;
pub(crate) const CHILD_INDENT: u16 = BODY_COLUMN - GUTTER_COLUMNS + 2;
/// The rows nested under a row that a header shows as its title hang from that title.
pub(crate) const NESTED_COLUMNS: u16 = GUTTER_COLUMNS - CHILD_INDENT;
pub(crate) const CHILD_PREFIX: &str = "    ";
pub(crate) const CHILD_BRANCH: &str = "  └ ";

/// Where a row's own tokens go.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum TokenSeat {
    /// The token column, which the detailed view leaves free of controls.
    Gutter,
    /// After the row's first line, where controls hold that column.
    Trailing,
}

pub(crate) fn content_width(width: u16, mode: HistoryRenderMode) -> u16 {
    match mode {
        HistoryRenderMode::Rich => width.saturating_sub(GUTTER_COLUMNS).max(/*other*/ 1),
        HistoryRenderMode::Raw => width,
    }
}

pub(crate) fn rich_lines(lines: Vec<HyperlinkLine>) -> Vec<HyperlinkLine> {
    gutter_lines(lines, GUTTER_COLUMNS)
}

/// Indent rows `columns` from the edge, drawing a row's clock at its start.
pub(crate) fn gutter_lines(lines: Vec<HyperlinkLine>, columns: u16) -> Vec<HyperlinkLine> {
    let prefix = " ".repeat(usize::from(columns));
    let mut lines = prefix_hyperlink_lines(lines, prefix.clone().into(), prefix.into());
    for line in &mut lines {
        if let Some(clock) = line.clock.take() {
            // Prefix provenance already excludes the entire gutter from copy/selection.
            let mut spans = vec![
                clock.span(),
                " ".repeat(usize::from(columns.saturating_sub(CLOCK_COLUMNS)))
                    .into(),
            ];
            spans.extend(line.line.spans.drain(1..));
            line.line.spans = spans;
        }
    }
    lines
}

pub(crate) fn clock_lines(
    mut lines: Vec<HyperlinkLine>,
    clock: Option<crate::history_cell::HistoryTimestamp>,
) -> Vec<HyperlinkLine> {
    if let Some(clock) = clock
        && let Some(line) = lines.iter_mut().find(|line| line.width() > 0)
    {
        line.clock = Some(clock);
    }
    lines
}

pub(crate) fn separated(
    previous: Option<&dyn HistoryCell>,
    tool_entry: bool,
    continuation: bool,
) -> bool {
    previous.is_some_and(|previous| !(continuation || tool_entry && previous.is_tool_entry()))
}
