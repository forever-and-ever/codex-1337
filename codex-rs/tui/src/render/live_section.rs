//! Shared inset for content below the transcript rule.

use super::Insets;
use super::RectExt;
use crate::ui_consts::LIVE_PREFIX_COLS;
use ratatui::layout::Rect;

pub(crate) fn content_area(area: Rect) -> Rect {
    area.inset(Insets::tlbr(0, LIVE_PREFIX_COLS, 0, 0))
}

pub(crate) fn content_width(width: u16) -> u16 {
    width.saturating_sub(LIVE_PREFIX_COLS)
}
