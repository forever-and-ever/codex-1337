//! Attach transparent page-boundary details to a freshly rebuilt activity group.

use crate::history_cell::ComputerActivityCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::ReasoningSummaryCell;
use crate::history_cell::ToolOutputCell;
use crate::history_cell::ToolOutputGroup;
use std::sync::Arc;

pub(crate) fn is_hidden_activity_detail(cell: &Arc<dyn HistoryCell>) -> bool {
    cell.as_any().is::<ToolOutputCell>()
        || cell
            .as_any()
            .downcast_ref::<ToolOutputGroup>()
            .is_some_and(|group| group.detail_members().is_some())
        || cell
            .as_any()
            .downcast_ref::<ReasoningSummaryCell>()
            .is_some_and(ReasoningSummaryCell::is_transcript_only)
}

/// Fold standalone reasoning and returned batches using persisted identities from one turn.
///
/// An older page can arrive after the final reasoning page even with no newer group or live tail.
/// Rebuild only known completed calls and share their immutable details; never infer adjacency
/// from identical reasoning text or attach a detail from the next turn.
pub(crate) fn fold_trailing_activity_details(
    older: &Arc<dyn HistoryCell>,
    details: &[Arc<dyn HistoryCell>],
    turns: &[codex_app_server_protocol::Turn],
) -> Option<Arc<dyn HistoryCell>> {
    let details = details
        .iter()
        .flat_map(|cell| {
            cell.as_any()
                .downcast_ref::<ToolOutputGroup>()
                .and_then(ToolOutputGroup::detail_members)
                .unwrap_or(std::slice::from_ref(cell))
        })
        .cloned()
        .collect::<Vec<_>>();
    if (!older.as_any().is::<ComputerActivityCell>() || older.outer_call_id().is_some())
        && details
            .iter()
            .any(|cell| cell.as_any().is::<ToolOutputCell>())
    {
        return ToolOutputGroup::join(
            older,
            &ToolOutputGroup::from_cells(details.iter().cloned()),
            turns,
        );
    }
    let group = older.as_any().downcast_ref::<ComputerActivityCell>()?;
    let mut ids = group.call_ids().collect::<Vec<_>>();
    let count = ids.len();
    for detail in &details {
        if let Some(output) = detail.as_any().downcast_ref::<ToolOutputCell>() {
            ids.push(output.id());
        } else {
            let detail = detail.as_any().downcast_ref::<ReasoningSummaryCell>()?;
            if !detail.is_transcript_only() {
                return None;
            }
            ids.push(detail.source_item_id()?);
        }
    }
    let items = super::computer_groups::adjacent_activity_items(&ids, turns)?;
    let mut retained = group.group.details.clone();
    for detail in &details {
        retained.push(count, Arc::clone(detail));
    }
    let mut group = super::computer_groups::completed_group(items)?;
    group.group.details = retained;
    Some(Arc::new(group))
}
