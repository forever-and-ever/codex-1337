//! Source-backed text outside history shares transcript selection, snapshots and clipboard rules.

use super::*;

impl TranscriptView {
    pub(crate) fn render_surface(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        lines: Vec<HyperlinkLine>,
    ) {
        self.live = Some(Arc::new(TextLayout::surface(lines, area.width)));
        self.live_separated = None;
        self.primary_selection = cfg!(target_os = "linux");
        self.render(area, buf, &[]);
    }
}
