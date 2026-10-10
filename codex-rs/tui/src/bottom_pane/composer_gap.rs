//! Close the owned transcript with a rule, pin live work under it, and reserve the hint row
//! above the composer or its current activity. The app paints its shared hints and reading
//! controls into the per-frame hint rectangle.

use std::cell::Cell;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

use crate::render::renderable::Renderable;
use crate::render::ruled_sections::HorizontalRule;

#[derive(Default)]
pub(crate) struct ComposerGap {
    /// Live work pinned under the rule, which also draws in place in the transcript.
    pub(crate) live: Vec<Line<'static>>,
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
            return;
        }
        HorizontalRule.render(area, buf);
        area.y += 1;
        area.height -= 1;
        // Live work yields its last rows before the hint row.
        let live = u16::try_from(self.live.len())
            .unwrap_or(u16::MAX)
            .min(area.height - 1);
        Paragraph::new(self.live[..usize::from(live)].to_vec()).render(
            Rect {
                height: live,
                ..area
            },
            buf,
        );
        area.y += live;
        area.height -= live;
        self.area.set(area);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        u16::try_from(self.live.len())
            .unwrap_or(u16::MAX)
            .saturating_add(/*rhs*/ 2)
    }
}
