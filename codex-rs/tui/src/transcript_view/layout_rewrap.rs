//! Pinned revisions and search results use the same off-thread width preparation.

use super::Arc;
use super::LayoutCache;
use super::TextLayout;
use std::sync::Weak;

pub(super) struct Rewrap {
    source: Weak<TextLayout>,
    width: u16,
    separated: bool,
    state: State,
}

enum State {
    Preparing(super::super::preparation::Pending<Arc<TextLayout>>),
    Ready(Arc<TextLayout>),
}

impl LayoutCache {
    pub(in crate::transcript_view) fn rewrap(
        &mut self,
        source: &Arc<TextLayout>,
        width: u16,
        separated: bool,
    ) -> Option<Arc<TextLayout>> {
        if source.width() == width && (!separated || source.separated) {
            return Some(Arc::clone(source));
        }
        let weak = Arc::downgrade(source);
        self.rewraps
            .retain(|entry| entry.source.strong_count() != 0 && entry.width == width);
        let index = self.rewraps.iter().position(|entry| {
            entry.source.ptr_eq(&weak) && entry.width == width && entry.separated == separated
        });
        let index = index.unwrap_or_else(|| {
            let source = Arc::clone(source);
            let render = move || {
                let layout = source.rewrap(width);
                Arc::new(if separated {
                    layout.with_leading_separator()
                } else {
                    layout
                })
            };
            let state = match &self.frames {
                Some(frames) => State::Preparing(super::super::preparation::Pending::new(
                    frames.clone(),
                    render,
                )),
                None => State::Ready(render()),
            };
            self.rewraps.push(Rewrap {
                source: weak,
                width,
                separated,
                state,
            });
            self.rewraps.len() - 1
        });
        let entry = &mut self.rewraps[index];
        if let State::Preparing(task) = &mut entry.state {
            let Some(layout) = task.poll() else {
                self.waiting = true;
                return None;
            };
            entry.state = State::Ready(layout);
        }
        let State::Ready(layout) = &entry.state else {
            unreachable!()
        };
        Some(Arc::clone(layout))
    }
}
