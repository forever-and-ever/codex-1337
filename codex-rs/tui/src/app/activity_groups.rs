//! Repair activity groups split by transport pages without rewriting terminal scrollback.
//!
//! Only persisted member adjacency within one turn permits joins. Overlay callbacks retain the
//! displayed source revision before canonical cells are replaced or absorbed into the live tail.

use super::*;
use crate::thread_transcript::join_computer_groups;

impl App {
    pub(super) fn join_older_activity_group(&mut self, boundary: usize, turns: &[Turn]) {
        let Some(mut start) = self.transcript_cells[..boundary]
            .iter()
            .rposition(|cell| !crate::thread_transcript::is_hidden_activity_detail(cell))
            .or_else(|| boundary.checked_sub(1))
        else {
            return;
        };
        let mut newer_index = self.transcript_cells[boundary..]
            .iter()
            .position(|cell| !crate::thread_transcript::is_hidden_activity_detail(cell))
            .map_or(self.transcript_cells.len(), |offset| boundary + offset);
        // Multiple notify() outputs may share an invocation, but never cross into the next one.
        let mut outer_call = None;
        let mut returned_end = None;
        for (offset, cell) in self.transcript_cells[boundary..newer_index]
            .iter()
            .enumerate()
        {
            if let Some(call_id) = cell.outer_call_id() {
                if outer_call.is_some_and(|old| old != call_id) {
                    break;
                }
                outer_call = Some(call_id);
                returned_end = Some(boundary + offset + 1);
            }
        }
        if let Some(end) = returned_end {
            newer_index = end;
        }
        // Returned batches belong to Edited, not the final file in its contiguous suffix.
        if let Some(owner) =
            history_cell::ToolOutputGroup::patch_owner(&self.transcript_cells[..boundary])
            && boundary < newer_index
            && self.transcript_cells[boundary..newer_index]
                .iter()
                .any(|cell| {
                    cell.as_any().is::<history_cell::ToolOutputCell>()
                        || cell
                            .as_any()
                            .downcast_ref::<history_cell::ToolOutputGroup>()
                            .is_some_and(|group| group.detail_members().is_some())
                })
            && let Some(group) = history_cell::ToolOutputGroup::join(
                &history_cell::ToolOutputGroup::from_cells(
                    std::iter::once(Arc::clone(&self.transcript_cells[owner])).chain(
                        self.transcript_cells[owner + 1..boundary]
                            .iter()
                            .filter(|cell| {
                                crate::thread_transcript::is_hidden_activity_detail(cell)
                            })
                            .cloned(),
                    ),
                ),
                &history_cell::ToolOutputGroup::from_cells(
                    self.transcript_cells[boundary..newer_index].iter().cloned(),
                ),
                turns,
            )
        {
            let replacements = std::iter::once(Arc::clone(&group))
                .chain(
                    self.transcript_cells[owner + 1..boundary]
                        .iter()
                        .filter(|cell| !crate::thread_transcript::is_hidden_activity_detail(cell))
                        .cloned(),
                )
                .collect::<Vec<_>>();
            let range = owner..newer_index;
            let previous = self.transcript_cells[range.clone()]
                .iter()
                .filter(|cell| {
                    !replacements
                        .iter()
                        .any(|replacement| Arc::ptr_eq(cell, replacement))
                })
                .cloned()
                .collect::<Vec<_>>();
            self.native_history.consolidate(&previous, &group);
            self.transcript_view.replace_group_members(
                &self.transcript_cells,
                range.clone(),
                &replacements,
            );
            if let Some(Overlay::Transcript(overlay)) = self.overlay.as_mut() {
                overlay.regroup_cell_members(range.clone(), &replacements);
            }
            self.transcript_cells.splice(range, replacements);
            return;
        }
        // A reasoning-only page belongs to the preceding group even when an answer or turn
        // separator follows it. Verify identities before touching either side of that boundary.
        if start + 1 < newer_index {
            let Some(older) = crate::thread_transcript::fold_trailing_activity_details(
                &self.transcript_cells[start],
                &self.transcript_cells[start + 1..newer_index],
                turns,
            ) else {
                return;
            };
            let (merged_start, older) = history_cell::ToolOutputGroup::join_prefix(
                &self.transcript_cells,
                start,
                older,
                turns,
            );
            start = merged_start;
            let range = start..newer_index;
            self.native_history
                .consolidate(&self.transcript_cells[range.clone()], &older);
            self.transcript_view
                .replace_group(&self.transcript_cells, range.clone(), &older);
            if let Some(Overlay::Transcript(overlay)) = self.overlay.as_mut() {
                overlay.regroup_cells(range.clone(), Arc::clone(&older));
            }
            self.transcript_cells.splice(range, [older]);
            newer_index = start + 1;
        }
        if newer_index == self.transcript_cells.len() {
            let Some((previous_revision, hydrated_revision)) = self
                .chat_widget
                .prepend_active_computer_history(self.transcript_cells[start].as_ref(), turns)
            else {
                return;
            };
            self.transcript_view.absorb_tail_into_live(
                &self.transcript_cells,
                previous_revision,
                hydrated_revision,
            );
            if let Some(Overlay::Transcript(overlay)) = self.overlay.as_mut() {
                overlay.absorb_tail_into_live(previous_revision, hydrated_revision);
            }
            self.transcript_cells.pop();
            self.native_history.retain(&self.transcript_cells);
            return;
        }
        let Some(newer) = self.transcript_cells.get(newer_index) else {
            return;
        };
        let Some(group) = join_computer_groups(&self.transcript_cells[start], newer, turns)
            .or_else(|| {
                history_cell::ToolOutputGroup::join(&self.transcript_cells[start], newer, turns)
            })
        else {
            return;
        };
        let (start, group) =
            history_cell::ToolOutputGroup::join_prefix(&self.transcript_cells, start, group, turns);
        let range = start..newer_index + 1;
        self.native_history
            .consolidate(&self.transcript_cells[range.clone()], &group);
        self.transcript_view
            .replace_group(&self.transcript_cells, range.clone(), &group);
        if let Some(Overlay::Transcript(overlay)) = self.overlay.as_mut() {
            overlay.regroup_cells(range.clone(), Arc::clone(&group));
        }
        self.transcript_cells.splice(range, [group]);
    }
}

#[cfg(test)]
#[path = "activity_groups_tests.rs"]
mod tests;
