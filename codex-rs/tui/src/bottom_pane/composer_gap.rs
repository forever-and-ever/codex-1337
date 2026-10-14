//! Close the owned transcript with a rule, pin live work under it, and reserve the hint row
//! above the composer or its current activity. The app paints its shared hints and reading
//! controls into the per-frame hint rectangle.

use std::cell::Cell;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::render::renderable::Renderable;
use crate::render::ruled_sections::HorizontalRule;

#[derive(Default)]
pub(crate) struct ComposerGap {
    /// The app paints its interactive live view into this reserved space after layout.
    pub(crate) live_height: u16,
    pub(crate) live_area: Cell<Rect>,
    /// Visible text needs a separator after activity when the viewport has room for it.
    pub(crate) needs_separator: bool,
    /// The hint row, filled during rendering; a fresh instance prevents reusing a hidden or
    /// clipped row.
    pub(crate) area: Cell<Rect>,
}

impl Renderable for ComposerGap {
    fn render(&self, mut area: Rect, buf: &mut Buffer) {
        if area.height < 2 {
            self.area.set(Rect::default());
            self.live_area.set(Rect::default());
            return;
        }
        HorizontalRule.render(area, buf);
        area.y += 1;
        area.height -= 1;
        // Live work yields its last rows before the hint row.
        let live = self.live_height.min(area.height - 1);
        self.live_area.set(Rect {
            height: live,
            ..area
        });
        area.y += live;
        area.height -= live;
        self.area.set(area);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        self.live_height.saturating_add(2)
    }
}
