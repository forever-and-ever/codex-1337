//! Completion preserves source across presentation changes without publishing partial rows.

use super::*;
use pretty_assertions::assert_eq;

pub(super) struct Trial {
    background: StreamController,
}

impl Trial {
    pub(super) fn new(width: Option<usize>) -> Self {
        Self {
            background: stream_controller(width)
                .with_background(crate::tui::FrameRequester::test_dummy()),
        }
    }

    pub(super) fn observe(&mut self, delta: &str) {
        assert!(!self.background.push(delta));
        let (cell, idle) = self.background.on_commit_tick_batch(usize::MAX);
        assert!(cell.is_none());
        assert!(idle);
        assert_eq!(
            self.background.current_tail_lines(),
            [] as [HyperlinkLine; 0]
        );
        assert_eq!(self.background.queued_lines(), 0);
    }

    pub(super) async fn finish(mut self, source: &str) {
        // Width/mode changes must not parse, publish, truncate, or reflow an unfinished reply.
        self.background.set_width(Some(48));
        self.background.set_render_mode(HistoryRenderMode::Raw);
        assert!(self.background.on_commit_tick_batch(usize::MAX).0.is_none());
        let ui = std::thread::current().id();
        let (tx, rx) = tokio::sync::oneshot::channel();
        assert!(
            self.background
                .finish_background(move |source| {
                    let _ = tx.send((source, std::thread::current().id()));
                })
                .is_ok()
        );
        let (final_source, worker) = rx.await.unwrap();
        assert_eq!(final_source, source);
        assert_ne!(worker, ui);
    }
}
