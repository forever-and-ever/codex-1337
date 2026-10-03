//! Rich transcript geometry. Cell widths exclude the disclosure gutter; source never includes it.
//! Marker-bearing entries start their body at column six. Markerless cards keep cell-owned geometry.

use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryRenderMode;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::prefix_hyperlink_lines;

pub(crate) const DISCLOSURE_COLUMNS: u16 = 3;
pub(crate) const GUTTER_COLUMNS: u16 = DISCLOSURE_COLUMNS + 1;
pub(crate) const MARKER_COLUMNS: u16 = 2;
pub(crate) const BODY_COLUMN: u16 = GUTTER_COLUMNS + MARKER_COLUMNS;
pub(crate) const CHILD_INDENT: u16 = BODY_COLUMN - GUTTER_COLUMNS + 2;
pub(crate) const CHILD_PREFIX: &str = "    ";
pub(crate) const CHILD_BRANCH: &str = "  └ ";

pub(crate) fn content_width(width: u16, mode: HistoryRenderMode) -> u16 {
    match mode {
        HistoryRenderMode::Rich => width.saturating_sub(GUTTER_COLUMNS).max(/*other*/ 1),
        HistoryRenderMode::Raw => width,
    }
}

pub(crate) fn rich_lines(lines: Vec<HyperlinkLine>) -> Vec<HyperlinkLine> {
    let prefix = " ".repeat(usize::from(GUTTER_COLUMNS));
    prefix_hyperlink_lines(lines, prefix.clone().into(), prefix.into())
}

pub(crate) fn separated(
    previous: Option<&dyn HistoryCell>,
    tool_entry: bool,
    continuation: bool,
) -> bool {
    previous.is_some_and(|previous| !(continuation || tool_entry && previous.is_tool_entry()))
}
