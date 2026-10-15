//! Local activity disclosure keyed by retained tool identities, independent of text selection.
//! Controls appear only while activity is focused; copied text and full-transcript search remain source based.

use crate::history_cell::activity_parts::FoldTarget;
use crate::history_cell::output_provenance::OutputMode;
use std::collections::HashMap;
use std::collections::HashSet;

use crate::bottom_pane::TranscriptFooter;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::ListAction;
use crate::keymap::RuntimeKeymap;
use crossterm::event::KeyEvent;

use super::*;

pub(super) struct Disclosure {
    pub(super) expanded: HashSet<String>,
    output_modes: HashMap<String, OutputChoice>,
    output_revision: u64,
    pub(super) focused: Option<ActivityFocus>,
    pub(super) live_ids: Vec<String>,
    pub(super) keymap: RuntimeKeymap,
}

#[derive(Clone, Copy)]
struct OutputChoice {
    // Existing aliases can prepend or regroup. Only explicit mode actions advance this clock.
    revision: u64,
    mode: OutputMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ActivityFocus {
    pub(super) ids: Vec<String>,
    pub(super) target: FoldTarget,
}

impl Default for Disclosure {
    fn default() -> Self {
        Self {
            expanded: HashSet::new(),
            output_modes: HashMap::new(),
            output_revision: 0,
            focused: None,
            live_ids: Vec::new(),
            keymap: RuntimeKeymap::defaults(),
        }
    }
}

impl Disclosure {
    pub(super) fn output_mode(&self, ids: &[String]) -> OutputMode {
        ids.iter()
            .filter_map(|id| self.output_modes.get(id))
            .max_by_key(|choice| choice.revision)
            .map_or(OutputMode::Raw, |choice| choice.mode)
    }
    pub(super) fn is_expanded(&self, ids: &[String]) -> bool {
        ids.iter().any(|id| self.expanded.contains(id))
    }
}

impl TranscriptView {
    pub(crate) fn is_activity_focused(&self) -> bool {
        self.disclosure.focused.is_some() && !self.detailed && self.mode == HistoryRenderMode::Rich
    }

    pub(crate) fn clear_activity_focus(&mut self) {
        self.disclosure.focused = None;
    }

    /// Live member identities survive the transition from active widget to committed history.
    pub(crate) fn sync_live_activity(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        ids: Vec<String>,
    ) -> bool {
        // Disclosure follows its activity into history instead of an unrelated next live tail.
        if self.snapshot().is_none()
            && let Position::Reading(anchor) = self.position
            && anchor.key == EntryKey::Live
            && let Some(index) = self.canonical_activity_index(cells, &self.disclosure.live_ids)
            && let Some(cell) = cells.get(index)
        {
            self.position = Position::Reading(Anchor {
                key: EntryKey::cell(cell),
                index,
                offset: 0,
                row_bias: 0,
            });
        }
        if self.disclosure.live_ids != ids {
            self.live_key = None;
        }
        let expanded = self.disclosure.is_expanded(&ids);
        if expanded {
            self.disclosure.expanded.extend(ids.iter().cloned());
        }
        self.disclosure.live_ids = ids;
        expanded
    }

    pub(super) fn activity_ids(&self, cells: &[Arc<dyn HistoryCell>], index: usize) -> Vec<String> {
        let Some(cell) = cells.get(index) else {
            return self.disclosure.live_ids.clone();
        };
        let ids = cell.activity_ids();
        // A Feed Item's header over a cell without identities is focused through its fold.
        if ids.is_empty()
            && self.indexed_seat(cells, index).header.is_some()
            && let Some(fold) = self.feed.fold_of(index)
        {
            return vec![fold.to_string()];
        }
        ids
    }

    /// Retired live entries and replaced groups keep the identities of their displayed revision.
    pub(super) fn displayed_activity_ids(
        &self,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
    ) -> Arc<[String]> {
        let key = cells.get(index).map_or(EntryKey::Live, EntryKey::cell);
        self.snapshot()
            .and_then(|snapshot| snapshot.activities.get(&key))
            .cloned()
            .unwrap_or_else(|| self.activity_ids(cells, index).into())
    }

    pub(super) fn render_disclosure(
        &self,
        ids: &[String],
        layout: &TextLayout,
        row: usize,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let Some(focused) = self.disclosure.focused.as_ref() else {
            return;
        };
        if !layout.disclosure || !ids.iter().any(|id| focused.ids.contains(id)) {
            return;
        }
        let target = &focused.target;
        if let Some((control_row, columns, _)) = layout.fold_position(target)
            && control_row == row
        {
            buf.set_style(
                Rect::new(
                    area.x + columns.start,
                    area.y,
                    columns.end - columns.start,
                    /*height*/ 1,
                ),
                crate::render::disclosure::style(true),
            );
        } else if *target == FoldTarget::Activity && row == usize::from(layout.separated) {
            buf.set_style(area, crate::render::disclosure::style(true));
        }
    }

    pub(super) fn toggle_disclosure_at(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        column: u16,
        row: u16,
    ) -> bool {
        if self.detailed || self.mode != HistoryRenderMode::Rich {
            return false;
        }
        let Some(visible) = row
            .checked_sub(self.area.y)
            .and_then(|row| self.visible.get(usize::from(row)))
        else {
            return false;
        };
        let Some(target) = column
            .checked_sub(self.area.x)
            .and_then(|column| visible.layout.fold_at(visible.row, column))
        else {
            return false;
        };
        let ids = Arc::clone(&visible.activity_ids);
        let Some(index) = self.canonical_fold_index(cells, &ids, &target) else {
            return false;
        };
        self.toggle_activity(cells, index, target);
        true
    }

    pub(super) fn handle_disclosure_key(
        &mut self,
        key: KeyEvent,
        cells: &[Arc<dyn HistoryCell>],
    ) -> Option<ViewAction> {
        if self.detailed || self.mode != HistoryRenderMode::Rich || self.is_search_editing() {
            return None;
        }
        if self.disclosure.keymap.app.focus_activity.is_pressed(key) {
            if self.is_activity_focused() {
                self.disclosure.focused = None;
            } else {
                self.end_selection(cells);
                let visible = self
                    .visible
                    .iter()
                    .rev()
                    .find_map(|row| self.canonical_activity_index(cells, &row.activity_ids));
                for index in visible.into_iter().chain((0..=cells.len()).rev()) {
                    if !self.activity_ids(cells, index).is_empty()
                        && self.focus_activity(cells, index)
                    {
                        break;
                    }
                }
            }
            return Some(ViewAction::Changed);
        }
        let focus = self.disclosure.focused.clone()?;
        let Some(focused) = self.canonical_fold_index(cells, &focus.ids, &focus.target) else {
            self.disclosure.focused = None;
            return None;
        };
        let Some(action) = self.disclosure.keymap.list.action_for(key) else {
            // Ordinary text resumes composing; activity navigation never becomes a text editor.
            self.disclosure.focused = None;
            return None;
        };
        match action {
            ListAction::Cancel => self.disclosure.focused = None,
            ListAction::Accept => self.toggle_activity(cells, focused, focus.target),
            ListAction::MoveLeft | ListAction::MoveRight => {
                let target = focus.target;
                if let FoldTarget::Invocation {
                    call_id,
                    action: invocation_action,
                } = &target
                {
                    use crate::history_cell::invocation::{Action, Panel};
                    let action = match *invocation_action {
                        Action::Toggle(port, next) => {
                            let shown = action == ListAction::MoveRight;
                            Action::Toggle(
                                port,
                                if shown {
                                    Panel::Visible(next.mode())
                                } else {
                                    Panel::Hidden(next.mode())
                                },
                            )
                        }
                        Action::Format(port, _) => Action::Format(
                            port,
                            if action == ListAction::MoveLeft {
                                OutputMode::Raw
                            } else {
                                OutputMode::Fmt
                            },
                        ),
                    };
                    let target = FoldTarget::Invocation {
                        call_id: call_id.clone(),
                        action,
                    };
                    if !matches!(action, Action::Format(_, _))
                        || self
                            .current_layout(cells, focused)
                            .is_some_and(|layout| layout.fold_position(&target).is_some())
                    {
                        self.toggle_activity(cells, focused, target);
                    }
                    return Some(ViewAction::Changed);
                }
                if matches!(target, FoldTarget::OutputMode(_)) {
                    let mode = if action == ListAction::MoveLeft {
                        OutputMode::Raw
                    } else {
                        OutputMode::Fmt
                    };
                    self.toggle_activity(cells, focused, FoldTarget::OutputMode(mode));
                    return Some(ViewAction::Changed);
                }
                let ids = target.ids(&self.activity_ids(cells, focused));
                let expanded = match &target {
                    FoldTarget::Part(fold) => {
                        self.feed.is_unfolded(fold, &self.disclosure.expanded)
                    }
                    _ => None,
                }
                .unwrap_or_else(|| self.disclosure.is_expanded(&ids))
                    || self.is_search_expanded(self.entry_key(cells, focused));
                if expanded != (action == ListAction::MoveRight) {
                    self.toggle_activity(cells, focused, target);
                }
            }
            ListAction::MoveUp
            | ListAction::MoveDown
            | ListAction::PageUp
            | ListAction::PageDown
            | ListAction::JumpTop
            | ListAction::JumpBottom => {
                self.navigate_folds(cells, focused, action);
            }
        }
        Some(ViewAction::Changed)
    }

    pub(super) fn canonical_activity_index(
        &self,
        cells: &[Arc<dyn HistoryCell>],
        ids: &[String],
    ) -> Option<usize> {
        if ids.is_empty() {
            return None;
        }
        // A visible immutable cell resolves directly; replaced groups fall back to member IDs.
        if let Some(row) = self.visible.iter().find(|row| {
            row.index <= cells.len()
                && row.activity_ids.iter().any(|id| ids.contains(id))
                && cells.get(row.index).map_or(EntryKey::Live, EntryKey::cell) == row.key
                && self
                    .activity_ids(cells, row.index)
                    .iter()
                    .any(|id| ids.contains(id))
        }) {
            return Some(row.index);
        }
        (0..=cells.len()).find(|index| {
            self.activity_ids(cells, *index)
                .iter()
                .any(|id| ids.contains(id))
        })
    }

    fn canonical_fold_index(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        ids: &[String],
        target: &FoldTarget,
    ) -> Option<usize> {
        let candidate = self.canonical_activity_index(cells, ids)?;
        if !self.elides_cell(cells, candidate)
            && (*target == FoldTarget::Activity
                || self
                    .current_layout(cells, candidate)
                    .is_some_and(|layout| layout.fold_position(target).is_some()))
        {
            return Some(candidate);
        }
        // A historical group can split across pages. Resolve its part, not the first shared ID.
        (0..=cells.len()).find(|index| {
            self.activity_ids(cells, *index)
                .iter()
                .any(|id| ids.contains(id))
                && self
                    .current_layout(cells, *index)
                    .is_some_and(|layout| layout.fold_position(target).is_some())
        })
    }

    pub(crate) fn reveal_call(&mut self, cells: &[Arc<dyn HistoryCell>], call_id: &str) -> bool {
        use crate::history_cell::invocation::Action;
        use crate::history_cell::invocation::Panel;
        use crate::history_cell::invocation::Port;

        let Some(index) = cells
            .iter()
            .position(|cell| cell.tool_call_ids().contains(&call_id))
        else {
            return false;
        };
        self.end_selection(cells);
        self.jump_to_entry(cells, index);
        let controls = self
            .current_layout(cells, index)
            .map(|layout| layout.fold_targets())
            .unwrap_or_default();
        let input = |target: &FoldTarget| {
            matches!(
                target,
                FoldTarget::Invocation {
                    action: Action::Toggle(Port::Input, _),
                    ..
                }
            )
        };
        let target = controls
            .iter()
            .find(|target| {
                input(target)
                    && matches!(target, FoldTarget::Invocation { call_id: id, .. } if id == call_id)
            })
            .or_else(|| controls.iter().find(|target| input(target)))
            .or_else(|| controls.first())
            .cloned();
        if let Some(target) = target {
            if matches!(
                target,
                FoldTarget::Invocation {
                    action: Action::Toggle(Port::Input, Panel::Visible(_)),
                    ..
                }
            ) {
                self.toggle_activity(cells, index, target);
            } else {
                self.focus_fold(cells, index, target);
            }
        }
        true
    }

    fn focus_activity(&mut self, cells: &[Arc<dyn HistoryCell>], index: usize) -> bool {
        let Some(target) = self
            .current_layout(cells, index)
            .and_then(|layout| layout.fold_targets().into_iter().next())
        else {
            return false;
        };
        self.focus_fold(cells, index, target)
    }

    pub(super) fn focus_fold(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
        target: FoldTarget,
    ) -> bool {
        let Some((control_row, _, offset)) = self
            .current_layout(cells, index)
            .and_then(|layout| layout.fold_position(&target))
        else {
            return false;
        };
        // Activity traversal follows current entries, not the aliases in a frozen live/group revision.
        self.release_live_reading();
        let ids = self.activity_ids(cells, index);
        let key = cells.get(index).map_or(EntryKey::Live, EntryKey::cell);
        let header_visible = self
            .visible
            .iter()
            .any(|row| row.key == key && row.row == control_row);
        self.disclosure.focused = Some(ActivityFocus { ids, target });
        if !header_visible {
            self.position = Position::Reading(Anchor {
                key,
                index,
                offset,
                row_bias: 0,
            });
        }
        true
    }

    #[expect(
        clippy::expect_used,
        reason = "An overflowing explicit-action sequence violates local mode ordering"
    )]
    fn toggle_activity(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        index: usize,
        target: FoldTarget,
    ) {
        if let FoldTarget::Part(fold) = &target
            && self
                .feed
                .is_unfolded(fold, &self.disclosure.expanded)
                .is_some()
        {
            let fold = fold.clone();
            self.toggle_feed(cells, index, &fold);
            return;
        }
        let activity_ids = self.activity_ids(cells, index);
        let ids = target.ids(&activity_ids);
        if ids.is_empty() {
            return;
        }
        let key = self.entry_key(cells, index);
        let matched = self.is_search_expanded(key);
        let previous_top = self.visible.first().map(|visible| {
            let offset = visible.layout.position_at(visible.row, /*column*/ 0);
            (
                visible.key,
                Arc::clone(&visible.activity_ids),
                offset,
                visible.layout.row_for_offset(offset) as isize - visible.row as isize,
            )
        });
        self.end_selection(cells);
        if self.search.has_active_query() && !matched {
            if let Some(snapshot) = &mut self.held_reading {
                snapshot.pinned.remove(&key);
            }
        } else {
            self.release_live_reading();
        }
        // Source offsets only survive an unchanged cell. Regrouped/retired live entries resolve
        // through their member IDs and anchor at their header, never a stale positional index.
        let mut anchor = previous_top
            .and_then(|(key, top_ids, offset, row_bias)| {
                let unchanged = cells.iter().position(|cell| EntryKey::cell(cell) == key);
                let top = unchanged.or_else(|| self.canonical_activity_index(cells, &top_ids))?;
                let preserve_offset = unchanged.is_some() && top != index;
                Some(Anchor {
                    key: cells.get(top).map_or(EntryKey::Live, EntryKey::cell),
                    index: top,
                    offset: if preserve_offset { offset } else { 0 },
                    row_bias: if preserve_offset { row_bias } else { 0 },
                })
            })
            .unwrap_or(Anchor {
                key: cells.get(index).map_or(EntryKey::Live, EntryKey::cell),
                index,
                offset: 0,
                row_bias: 0,
            });
        if let FoldTarget::Invocation { call_id, action } = &target {
            self.invocations.apply(call_id, *action);
        } else if let FoldTarget::OutputMode(mode) = target {
            self.disclosure.output_revision = self
                .disclosure
                .output_revision
                .checked_add(1)
                .expect("output mode revision exhausted");
            let choice = OutputChoice {
                revision: self.disclosure.output_revision,
                mode,
            };
            self.disclosure
                .output_modes
                .extend(ids.iter().cloned().map(|id| (id, choice)));
        } else if matched || self.disclosure.is_expanded(&ids) {
            self.disclosure.expanded.retain(|id| !ids.contains(id));
        } else {
            self.disclosure.expanded.extend(ids.iter().cloned());
        }
        let target = match target {
            FoldTarget::Invocation {
                call_id,
                action: crate::history_cell::invocation::Action::Toggle(port, panel),
            } => FoldTarget::Invocation {
                call_id,
                action: crate::history_cell::invocation::Action::Toggle(port, panel.toggled()),
            },
            target => target,
        };
        self.disclosure.focused = Some(ActivityFocus {
            ids: activity_ids,
            target,
        });
        self.cache.clear();
        self.live_key = None;
        self.live_separated = None;
        if anchor.index == index
            && let Some(layout) = self.current_layout(cells, index)
        {
            // Offset zero is the first selectable byte, not a synthetic Feed header.
            // Keep that header and its controls in view when reshaping its body.
            anchor.row_bias = layout.row_for_offset(0) as isize;
        }
        self.position = Position::Reading(anchor);
        if matched {
            self.invalidate_held_search();
        }
    }

    pub(super) fn disclosure_footer(&self, width: u16) -> Option<TranscriptFooter> {
        if !self.is_activity_focused() {
            return None;
        }
        let keymap = &self.disclosure.keymap;
        let hints: Vec<_> = [
            (ListAction::MoveUp, "previous"),
            (ListAction::MoveDown, "next"),
            (ListAction::Accept, "details"),
            (ListAction::Cancel, "back"),
        ]
        .into_iter()
        .filter_map(|(action, label)| {
            keymap.list.primary_hint(action).map(|key| {
                let mut spans = key.spans();
                spans.push(format!(" {label}").dim());
                Line::from(spans)
            })
        })
        .collect();
        // Drop navigation before the details/back controls, retaining whole remapped chords.
        let text = crate::footer_hint::first_fitting_line(
            (0..hints.len()).map(|start| {
                let mut spans = Vec::new();
                for (index, hint) in hints[start..].iter().enumerate() {
                    if index > 0 {
                        spans.push(" · ".dim());
                    }
                    spans.extend(hint.spans.clone());
                }
                Line::from(spans)
            }),
            width,
        );
        Some(TranscriptFooter {
            text: text.into(),
            cursor_column: None,
            is_interactive: true,
        })
    }
}
