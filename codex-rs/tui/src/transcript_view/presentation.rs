//! Fullscreen-only elision never removes captured history or its accounting identities.

use super::TranscriptView;
use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryRenderMode;
use crate::history_cell::ToolOutputCell;
use crate::history_cell::ToolOutputGroup;
use std::sync::Arc;

fn empty_return(cell: &dyn HistoryCell) -> bool {
    if let Some(output) = cell.as_any().downcast_ref::<ToolOutputCell>() {
        return output.is_empty_completed_return();
    }
    cell.as_any()
        .downcast_ref::<ToolOutputGroup>()
        .and_then(ToolOutputGroup::detail_members)
        .is_some_and(|members| {
            !members.is_empty() && members.iter().all(|member| empty_return(member.as_ref()))
        })
}

impl TranscriptView {
    pub(super) fn elides_cell(&self, cells: &[Arc<dyn HistoryCell>], index: usize) -> bool {
        if self.detailed || self.mode != HistoryRenderMode::Rich {
            return false;
        }
        let Some(cell) = cells.get(index) else {
            return false;
        };
        empty_return(cell.as_ref())
            && !crate::history_cell::invocation::collect(cell.as_ref())
                .iter()
                .any(|call| self.invocations.input(call.id).is_some())
            && self.activity_output_tokens(&cell.activity_ids()) == Some(0)
    }
}
