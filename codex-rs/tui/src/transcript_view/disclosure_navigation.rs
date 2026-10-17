//! Keyboard traversal follows current fold positions, including several folds in one entry.

use super::*;
use crate::keymap::ListAction;

impl TranscriptView {
    pub(super) fn navigate_folds(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
        action: ListAction,
    ) {
        if !self.deferred_folds.is_empty() {
            self.deferred_folds
                .push_back((Arc::from(cells), index, action));
            return;
        }
        let focus = self.disclosure.focused.clone();
        let position = self.position;
        let waiting = std::mem::replace(&mut self.cache.waiting, false);
        self.navigate_folds_prepared(cells, index, action);
        if self.cache.waiting {
            self.disclosure.focused = focus;
            self.position = position;
            self.deferred_folds
                .push_back((Arc::from(cells), index, action));
        }
        self.cache.waiting |= waiting;
    }

    pub(super) fn resume_fold_navigation(&mut self) {
        let source = std::mem::replace(
            &mut self.layout_source,
            super::presented::LayoutSource::Presented,
        );
        for _ in 0..8 {
            let Some((cells, index, action)) = self.deferred_folds.pop_front() else {
                break;
            };
            let focus = self.disclosure.focused.clone();
            let index = focus
                .as_ref()
                .and_then(|focus| self.canonical_fold_index(&cells, &focus.ids, &focus.target))
                .unwrap_or(index);
            let focus = self.disclosure.focused.clone();
            let position = self.position;
            let waiting = std::mem::replace(&mut self.cache.waiting, false);
            self.navigate_folds_prepared(&cells, index, action);
            if self.cache.waiting {
                self.disclosure.focused = focus;
                self.position = position;
                self.deferred_folds.push_front((cells, index, action));
                self.cache.waiting |= waiting;
                break;
            }
            self.cache.waiting |= waiting;
        }
        self.layout_source = source;
    }

    fn navigate_folds_prepared(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        mut index: usize,
        action: ListAction,
    ) {
        let backwards = matches!(
            action,
            ListAction::MoveUp | ListAction::PageUp | ListAction::JumpBottom
        );
        let edge = matches!(action, ListAction::JumpTop | ListAction::JumpBottom);
        let steps = if matches!(action, ListAction::PageUp | ListAction::PageDown) {
            5
        } else {
            1
        };
        let Some(mut target) = self
            .disclosure
            .focused
            .as_ref()
            .map(|focus| focus.target.clone())
        else {
            return;
        };
        if edge {
            index = if backwards { cells.len() } else { 0 };
        }
        for step in 0..steps {
            let Some(layout) = self.current_layout(cells, index) else {
                return;
            };
            let controls = layout.fold_targets();
            let current = controls
                .iter()
                .position(|candidate| candidate.same_control(&target));
            let next = if edge && step == 0 {
                if backwards {
                    controls.last()
                } else {
                    controls.first()
                }
            } else {
                current
                    .and_then(|current| {
                        if backwards {
                            current.checked_sub(1)
                        } else {
                            current.checked_add(1)
                        }
                    })
                    .and_then(|next| controls.get(next))
            };
            if let Some(next) = next {
                target = next.clone();
                self.focus_fold(cells, index, target.clone());
                continue;
            }
            let mut candidate = index;
            loop {
                let next = if backwards {
                    candidate.checked_sub(1)
                } else {
                    candidate.checked_add(1).filter(|next| *next <= cells.len())
                };
                let Some(next) = next else {
                    return;
                };
                candidate = next;
                let Some(layout) = self.current_layout(cells, candidate) else {
                    return;
                };
                let controls = layout.fold_targets();
                if let Some(next) = if backwards {
                    controls.last()
                } else {
                    controls.first()
                } {
                    index = candidate;
                    target = next.clone();
                    self.focus_fold(cells, index, target.clone());
                    break;
                }
            }
        }
    }
}
