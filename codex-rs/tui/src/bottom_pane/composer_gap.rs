//! Reserve the owned transcript's hint row above the composer or its current activity.
//! The app paints its shared hints and reading controls into the per-frame layout rectangle.

use std::cell::Cell;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::render::renderable::Renderable;
use crate::render::ruled_sections::HorizontalRule;

#[derive(Default)]
pub(crate) struct ComposerGap {
    /// Hidden transcript rows need a ruled reading boundary before current activity.
    pub(crate) separates_transcript: bool,
    /// Visible text needs a separator after activity when the viewport has room for it.
    pub(crate) needs_separator: bool,
    /// Filled during rendering; a fresh instance prevents reusing a hidden or clipped row.
    pub(crate) area: Cell<Rect>,
}

impl Renderable for ComposerGap {
    fn render(&self, mut area: Rect, buf: &mut Buffer) {
        if self.separates_transcript {
            if area.height < 2 {
                self.area.set(Rect::default());
                return;
            }
            HorizontalRule.render(area, buf);
            area.y += 1;
            area.height -= 1;
        }
        self.area.set(area);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        1 + u16::from(self.separates_transcript)
    }
}
