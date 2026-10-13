//! Complete retained calls in place, including calls displaced into returned-output groups.

use super::ActivityGroup;
use super::ComputerActivityCell;
use super::DynamicToolCallCell;
use super::HistoryCell;
use super::McpToolCallCell;
use super::ToolOutputGroup;
use super::WebSearchCell;
use crate::exec_cell::ExecCell;
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) fn has_running_tool_calls(cell: &dyn HistoryCell) -> bool {
    if let Some(block) = cell.as_any().downcast_ref::<super::CodeModeCell>() {
        return block.is_active();
    }
    if let Some(group) = cell.as_any().downcast_ref::<ToolOutputGroup>() {
        return group
            .members
            .iter()
            .any(|member| has_running_tool_calls(member.as_ref()));
    }
    cell.as_any()
        .downcast_ref::<McpToolCallCell>()
        .is_some_and(McpToolCallCell::is_active)
        || cell
            .as_any()
            .downcast_ref::<ComputerActivityCell>()
            .is_some_and(ComputerActivityCell::is_active)
        || cell
            .as_any()
            .downcast_ref::<ExecCell>()
            .is_some_and(ExecCell::is_active)
        || cell
            .as_any()
            .downcast_ref::<WebSearchCell>()
            .is_some_and(WebSearchCell::is_active)
        || cell
            .as_any()
            .downcast_ref::<DynamicToolCallCell>()
            .is_some_and(DynamicToolCallCell::is_active)
}

enum Revision<'a> {
    Mcp(&'a McpToolCallCell),
    Exec(&'a ExecCell, usize),
    Web(&'a WebSearchCell),
    AgentWait(&'a Arc<dyn HistoryCell>),
}

pub(crate) struct ToolCallReconciliation {
    pub(crate) replacements: BTreeMap<usize, Arc<dyn HistoryCell>>,
    pub(crate) remainder: Vec<Arc<dyn HistoryCell>>,
}

impl Revision<'_> {
    fn replace(&self, cell: &Arc<dyn HistoryCell>) -> Option<Arc<dyn HistoryCell>> {
        if let Some(group) = cell.as_any().downcast_ref::<ToolOutputGroup>() {
            let (index, updated) = group
                .members
                .iter()
                .enumerate()
                .find_map(|(index, member)| self.replace(member).map(|updated| (index, updated)))?;
            if Arc::ptr_eq(&group.members[index], &updated) {
                return Some(Arc::clone(cell));
            }
            let mut members = group.members.clone();
            members[index] = updated;
            return Some(ToolOutputGroup::from_cells(members));
        }
        match self {
            Self::AgentWait(newer) => {
                let new_wait = crate::multi_agents::AgentToolHistoryCell::wait_for(newer.as_ref())?;
                let old_wait = crate::multi_agents::AgentToolHistoryCell::wait_for(cell.as_ref())?;
                (old_wait.id == new_wait.id).then(|| {
                    if new_wait.state.is_running() {
                        Arc::clone(cell)
                    } else {
                        Arc::clone(newer)
                    }
                })
            }
            Self::Mcp(newer) => {
                if cell
                    .as_any()
                    .downcast_ref::<McpToolCallCell>()
                    .is_some_and(|old| old.call_id() == newer.call_id())
                {
                    return Some(if newer.is_active() {
                        Arc::clone(cell)
                    } else {
                        Arc::new((*newer).clone())
                    });
                }
                let old = cell.as_any().downcast_ref::<ComputerActivityCell>()?;
                let index = old
                    .group
                    .calls
                    .iter()
                    .position(|call| call.call_id() == newer.call_id())?;
                if newer.is_active() {
                    return Some(Arc::clone(cell));
                }
                let mut updated = old.clone();
                updated.group.calls[index] = (*newer).clone();
                Some(Arc::new(updated))
            }
            Self::Exec(newer, call_index) => {
                let call = &newer.group.calls[*call_index];
                let old = cell.as_any().downcast_ref::<ExecCell>()?;
                let index = old
                    .group
                    .calls
                    .iter()
                    .position(|old| old.call_id == call.call_id)?;
                if call.duration.is_none() {
                    return Some(Arc::clone(cell));
                }
                let mut updated = old.clone();
                updated.group.calls[index] = call.clone();
                updated.completed_at = newer.completed_at;
                Some(Arc::new(updated))
            }
            Self::Web(newer) => cell
                .as_any()
                .downcast_ref::<WebSearchCell>()
                .filter(|old| old.call_id() == newer.call_id())
                .map(|_| {
                    if newer.is_active() {
                        Arc::clone(cell)
                    } else {
                        Arc::new((*newer).clone()) as Arc<dyn HistoryCell>
                    }
                }),
        }
    }
}

/// Replace matching invocations without moving their headers or discarding their returned batches.
/// Unmatched calls and newly returned details retain their chronological insertion point.
pub(crate) fn reconcile_tool_calls(
    cells: &[Arc<dyn HistoryCell>],
    incoming: &Arc<dyn HistoryCell>,
) -> ToolCallReconciliation {
    let revisions = if let Some(cell) = incoming.as_any().downcast_ref::<McpToolCallCell>() {
        vec![Revision::Mcp(cell)]
    } else if let Some(cell) = incoming.as_any().downcast_ref::<ComputerActivityCell>() {
        cell.group.calls.iter().map(Revision::Mcp).collect()
    } else if let Some(cell) = incoming.as_any().downcast_ref::<ExecCell>() {
        (0..cell.group.calls.len())
            .map(|index| Revision::Exec(cell, index))
            .collect()
    } else if let Some(cell) = incoming.as_any().downcast_ref::<WebSearchCell>() {
        vec![Revision::Web(cell)]
    } else if crate::multi_agents::AgentToolHistoryCell::wait_for(incoming.as_ref()).is_some() {
        vec![Revision::AgentWait(incoming)]
    } else {
        return ToolCallReconciliation {
            replacements: BTreeMap::new(),
            remainder: vec![Arc::clone(incoming)],
        };
    };
    let mut replacements = BTreeMap::new();
    let retained = revisions
        .iter()
        .map(|revision| {
            let mut matched = false;
            for (index, cell) in cells.iter().enumerate() {
                let cell = replacements.get(&index).unwrap_or(cell);
                if let Some(updated) = revision.replace(cell) {
                    replacements.insert(index, updated);
                    matched = true;
                }
            }
            !matched
        })
        .collect::<Vec<_>>();
    if replacements.is_empty() {
        return ToolCallReconciliation {
            replacements,
            remainder: vec![Arc::clone(incoming)],
        };
    }
    let remainder = if let Some(cell) = incoming.as_any().downcast_ref::<ComputerActivityCell>() {
        let mut cell = cell.clone();
        let mut remainder = retain_calls(&mut cell.group, &retained);
        if !cell.group.calls.is_empty() {
            remainder.push(Arc::new(cell) as Arc<dyn HistoryCell>);
        }
        remainder
    } else if let Some(cell) = incoming.as_any().downcast_ref::<ExecCell>() {
        let mut cell = cell.clone();
        let mut remainder = retain_calls(&mut cell.group, &retained);
        if !cell.group.calls.is_empty() {
            remainder.push(Arc::new(cell) as Arc<dyn HistoryCell>);
        }
        remainder
    } else {
        Vec::new()
    };
    ToolCallReconciliation {
        replacements,
        remainder,
    }
}

fn retain_calls<T>(group: &mut ActivityGroup<T>, retained: &[bool]) -> Vec<Arc<dyn HistoryCell>> {
    let mut index = 0;
    group.calls.retain(|_| {
        let keep = retained[index];
        index += 1;
        keep
    });
    if group.calls.is_empty() {
        return std::mem::take(&mut group.details.entries)
            .into_iter()
            .map(|(_, cell)| cell)
            .collect();
    }
    for (position, _) in &mut group.details.entries {
        *position = retained[..*position].iter().filter(|keep| **keep).count();
    }
    // Removing a leading call can leave its details before the first surviving call.
    // Emit that prefix before the remainder; grouped renderers only read details after calls.
    let prefix = group
        .details
        .entries
        .partition_point(|(position, _)| *position == 0);
    group
        .details
        .entries
        .drain(..prefix)
        .map(|(_, cell)| cell)
        .collect()
}
