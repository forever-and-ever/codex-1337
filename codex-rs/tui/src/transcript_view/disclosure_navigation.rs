//! Keyboard traversal follows current fold positions, including several folds in one entry.

use super::*;
use crate::keymap::ListAction;

impl TranscriptView {
    pub(super) fn navigate_folds(
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
            let controls = self
                .current_layout(cells, index)
                .map(|layout| layout.fold_targets())
                .unwrap_or_default();
            let current = controls.iter().position(|candidate| *candidate == target);
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
                let controls = self
                    .current_layout(cells, candidate)
                    .map(|layout| layout.fold_targets())
                    .unwrap_or_default();
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
