//! The bottom pane owns the status row a glint repaints.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::ChatWidget;

impl ChatWidget {
    /// Repaints the status row into `area` while it still spans `height` lines there.
    pub(crate) fn paint_status_glint(&self, area: Rect, height: u16, buf: &mut Buffer) -> bool {
        self.bottom_pane.paint_status_glint(area, height, buf)
    }
}
