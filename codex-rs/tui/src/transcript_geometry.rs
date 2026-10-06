//! Rich transcript geometry. Clocks and disclosures never enter selectable source.
//! Marker-bearing entries share a fixed content column; cards retain cell-owned markers.

use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryRenderMode;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::prefix_hyperlink_lines;

pub(crate) const DISCLOSURE_COLUMNS: u16 = 3;
pub(crate) const CLOCK_COLUMNS: u16 = 6;
pub(crate) const TOKEN_COLUMN: u16 = CLOCK_COLUMNS;
pub(crate) const TOKEN_COLUMNS: u16 = 5;
pub(crate) const DISCLOSURE_COLUMN: u16 = TOKEN_COLUMN + TOKEN_COLUMNS + 1;
pub(crate) const GUTTER_COLUMNS: u16 = DISCLOSURE_COLUMN + DISCLOSURE_COLUMNS + 1;
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
    let mut lines = prefix_hyperlink_lines(lines, prefix.clone().into(), prefix.into());
    for line in &mut lines {
        if let Some(clock) = line.clock.take() {
            // Prefix provenance already excludes the entire gutter from copy/selection.
            let mut spans = vec![
                clock.span(),
                " ".repeat(usize::from(GUTTER_COLUMNS - CLOCK_COLUMNS))
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
