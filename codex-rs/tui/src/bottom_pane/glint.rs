//! The status row repaints alone for a glint, as does the rest row in its place between turns.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::BottomPane;
use crate::render::renderable::Renderable;

impl BottomPane {
    /// Repaints the status row, or the rest row in its place, into `area` while it still spans
    /// `height` lines there.
    pub(crate) fn paint_status_glint(&self, area: Rect, height: u16, buf: &mut Buffer) -> bool {
        if let Some(status) = self.status_widget() {
            glint(&status.with_timer(&self.status_timer), area, height, buf)
        } else if let Some(rest) = self.rest_row() {
            glint(&rest, area, height, buf)
        } else {
            false
        }
    }
}

fn glint(row: &impl Renderable, area: Rect, height: u16, buf: &mut Buffer) -> bool {
    if row.desired_height(area.width) != height {
        return false;
    }
    row.render(area, buf);
    true
}
