//! Hit targets become interactive only when their frame finishes terminal output.
use super::Focus;
use super::LiveWorkView;
use super::Rect;
use crate::custom_terminal::output;
use crate::custom_terminal::output::FrameId;

#[derive(Default)]
pub(super) struct FrameHits {
    completed: Option<(FrameId, Vec<(Rect, Focus)>)>,
    pending: Option<(FrameId, Vec<(Rect, Focus)>)>,
}

impl FrameHits {
    fn promote(&mut self) {
        if self
            .pending
            .as_ref()
            .is_some_and(|(id, _)| Some(*id) == output::presented_frame())
        {
            self.completed = self.pending.take();
        }
    }
}

impl LiveWorkView {
    pub(crate) fn capture_painted_frame(&mut self, visible: bool) {
        let Some(id) = output::current_frame() else {
            return;
        };
        self.painted_hits.promote();
        self.painted_hits.pending = Some((
            id,
            if visible {
                self.hits.clone()
            } else {
                Vec::new()
            },
        ));
        if visible {
            self.selection.capture_painted_frame();
        }
    }

    pub(crate) fn promote_presented_frame(&mut self) {
        if !output::is_active() {
            return;
        }
        self.painted_hits.promote();
        self.hits = self
            .painted_hits
            .completed
            .as_ref()
            .map(|(_, hits)| hits.clone())
            .unwrap_or_default();
        self.selection.promote_presented_frame();
    }
}
