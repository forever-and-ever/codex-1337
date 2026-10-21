//! The status row repaints alone for a glint.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::BottomPane;
use crate::render::renderable::Renderable;

impl BottomPane {
    /// Repaints the status row into `area` while it still spans `height` lines there.
    pub(crate) fn paint_status_glint(&self, area: Rect, height: u16, buf: &mut Buffer) -> bool {
        let Some(status) = self.status_widget() else {
            return false;
        };
        let row = status.with_timer(&self.status_timer);
        if row.desired_height(area.width) != height {
            return false;
        }
        row.render(area, buf);
        true
    }
}
