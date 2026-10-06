//! Fullscreen-only elision never removes captured history or its accounting identities.

use super::TranscriptView;
use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryRenderMode;
use crate::history_cell::ToolOutputCell;
use crate::history_cell::ToolOutputGroup;
use crate::multi_agents::AgentToolHistoryCell;
use codex_app_server_protocol::CollabAgentToolCallStatus;
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
        if let Some(wait) = AgentToolHistoryCell::wait_for(cell.as_ref()) {
            return wait.status == CollabAgentToolCallStatus::Completed
                && wait.terminal_lines.is_empty();
        }
        empty_return(cell.as_ref()) && self.activity_output_tokens(&cell.activity_ids()) == Some(0)
    }
}
