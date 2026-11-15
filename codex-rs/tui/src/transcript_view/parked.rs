//! Passive panes reuse prepared immutable revisions without acknowledging output.

use super::ActiveCellTranscriptKey;
use super::Arc;
use super::Buffer;
use super::HistoryCell;
use super::HistoryRenderMode;
use super::OwnedLiveTail;
use super::Rect;
use super::TranscriptView;

impl TranscriptView {
    /// TELOS(fast-ui-thread): parked panes reuse owned source and worker layout preparation.
    pub(crate) fn prepare_parked(
        &mut self,
        width: u16,
        cells: &[Arc<dyn HistoryCell>],
        source: Option<ActiveCellTranscriptKey>,
        tail: &OwnedLiveTail,
        frames: &crate::tui::FrameRequester,
    ) {
        self.enable_background_preparation(frames.clone());
        let unseen_activity = self.unseen_activity;
        let ids = tail
            .active
            .as_ref()
            .map_or_else(Vec::new, |cell| cell.activity_ids());
        let expanded = self.sync_live_activity(cells, ids) && self.mode == HistoryRenderMode::Rich;
        let _ = self.sync_owned_live_tail(width, source, expanded, || tail.clone());
        self.unseen_activity = unseen_activity;
    }

    /// A passive paint neither acknowledges output nor reconstructs its captured source.
    pub(crate) fn render_parked(
        &mut self,
        area: Rect,
        buffer: &mut Buffer,
        cells: &[Arc<dyn HistoryCell>],
        frames: &crate::tui::FrameRequester,
    ) {
        self.enable_background_preparation(frames.clone());
        let unseen_activity = self.unseen_activity;
        self.render(area, buffer, cells);
        self.unseen_activity = unseen_activity;
    }
}
