//! Exact ownership reconciliation across live updates and independently projected pages.

use super::CodeModeCell;
use super::CodeModeFactCell;
use crate::history_cell::HistoryCell;
use codex_app_server_protocol::CodeModeEvent;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

type ProjectionChange = (Range<usize>, Vec<Arc<dyn HistoryCell>>);

/// Existing exploration batching may straddle scripts or include unattributed reasoning.
/// Split those presentation groups before attributing their concrete invocations.
pub(crate) fn split_code_mode_members(
    cells: &[Arc<dyn HistoryCell>],
    incoming: &Arc<dyn HistoryCell>,
) -> Option<Vec<Arc<dyn HistoryCell>>> {
    let ids = incoming.tool_call_ids();
    if !cells
        .iter()
        .filter_map(|cell| cell.as_any().downcast_ref::<CodeModeCell>())
        .any(|block| ids.iter().any(|id| block.calls().contains(id)))
    {
        return None;
    }
    if let Some(group) = incoming
        .as_any()
        .downcast_ref::<crate::history_cell::ComputerActivityCell>()
    {
        if group.group.calls.len() == 1 && group.group.details.entries.is_empty() {
            return None;
        }
        return Some(
            group
                .group
                .calls
                .iter()
                .map(|call| {
                    if cells
                        .iter()
                        .filter_map(|cell| cell.as_any().downcast_ref::<CodeModeCell>())
                        .any(|block| block.owns(call))
                    {
                        Arc::new(call.clone()) as Arc<dyn HistoryCell>
                    } else {
                        let mut cell = group.clone();
                        cell.group.calls = vec![call.clone()];
                        cell.group.details = Default::default();
                        Arc::new(cell) as Arc<dyn HistoryCell>
                    }
                })
                .chain(
                    group
                        .group
                        .details
                        .entries
                        .iter()
                        .map(|(_, detail)| Arc::clone(detail)),
                )
                .collect(),
        );
    }
    let group = incoming
        .as_any()
        .downcast_ref::<crate::exec_cell::ExecCell>()?;
    if group.group.calls.len() == 1 && group.group.details.entries.is_empty() {
        return None;
    }
    Some(
        group
            .group
            .calls
            .iter()
            .map(|call| {
                let mut cell = group.clone();
                cell.group.calls = vec![call.clone()];
                cell.group.details = Default::default();
                Arc::new(cell) as Arc<dyn HistoryCell>
            })
            .chain(
                group
                    .group
                    .details
                    .entries
                    .iter()
                    .map(|(_, detail)| Arc::clone(detail)),
            )
            .collect(),
    )
}

/// Reconcile explicit facts or a known child; unrelated cells retain their own identities and order.
pub(crate) fn reconcile_code_mode(
    cells: &[Arc<dyn HistoryCell>],
    incoming: &Arc<dyn HistoryCell>,
) -> Option<ProjectionChange> {
    let fact = incoming.as_any().downcast_ref::<CodeModeFactCell>();
    let owner = cells.iter().enumerate().find_map(|(index, cell)| {
        let block = cell.as_any().downcast_ref::<CodeModeCell>()?;
        if fact.map_or_else(
            || block.owns(incoming.as_ref()),
            |fact| fact.0.exec_call_id == block.exec_call_id,
        ) {
            Some((index, block.clone()))
        } else {
            None
        }
    });
    let (owner_index, mut block) = match owner {
        Some(owner) => owner,
        None => (
            cells.len(),
            CodeModeCell::from_fact(fact?.0.clone(), fact?.1),
        ),
    };
    let mut terminal = false;
    let mut reference = None;
    if let Some(fact) = fact {
        terminal = matches!(fact.0.event, CodeModeEvent::Finished { .. })
            && (owner_index == cells.len() || block.status().is_none());
        block.insert_fact(fact.0.clone());
        if matches!(
            fact.0.event,
            CodeModeEvent::WaitStarted { .. } | CodeModeEvent::WaitCompleted { .. }
        ) {
            reference = Some(Arc::clone(incoming));
        }
    } else {
        block.insert_member(Arc::clone(incoming));
    }
    let start = if let Some(fact) = fact.filter(|_| reference.is_some()) {
        cells
            .iter()
            .position(|cell| same_wait_reference(cell.as_ref(), fact))
            .map_or(owner_index, |index| index.min(owner_index))
    } else {
        owner_index
    };
    let mut retained = Vec::new();
    let mut block_position = 0;
    let mut reference_position = None;
    for (index, cell) in cells.iter().enumerate().skip(start) {
        if index == owner_index && cell.as_any().is::<CodeModeCell>() {
            block_position = retained.len();
            continue;
        }
        if let Some(old) = cell.as_any().downcast_ref::<CodeModeCell>()
            && old.exec_call_id == block.exec_call_id
        {
            for fact in &old.facts {
                block.insert_fact(fact.clone());
            }
            for member in &old.members {
                block.insert_member(Arc::clone(member));
            }
        } else if block.owns(cell.as_ref()) {
            block.insert_member(Arc::clone(cell));
        } else if reference.is_some() && same_wait_reference(cell.as_ref(), fact?) {
            // The completed reference replaces its pending row rather than creating a second bill.
            reference_position = Some(retained.len());
        } else {
            retained.push(Arc::clone(cell));
        }
    }
    let block: Arc<dyn HistoryCell> = Arc::new(block);
    if terminal {
        retained.push(block);
    } else {
        retained.insert(block_position, block);
    }
    if let Some(reference) = reference {
        let index = reference_position.map_or(retained.len(), |index| {
            index + usize::from(!terminal && index >= block_position)
        });
        retained.insert(index, reference);
    }
    Some((start..cells.len(), retained))
}

fn same_wait_reference(cell: &dyn HistoryCell, incoming: &CodeModeFactCell) -> bool {
    let Some(old) = cell.as_any().downcast_ref::<CodeModeFactCell>() else {
        return false;
    };
    match (&old.0.event, &incoming.0.event) {
        (
            CodeModeEvent::WaitStarted { call_id: old },
            CodeModeEvent::WaitCompleted { call_id: new },
        ) => old == new,
        _ => old.0.id == incoming.0.id,
    }
}

/// Reunite exact execution ownership across independently projected transport pages.
pub(crate) fn reunite_code_mode(cells: &[Arc<dyn HistoryCell>]) -> Option<ProjectionChange> {
    let split = cells
        .iter()
        .flat_map(|cell| {
            split_code_mode_members(cells, cell).unwrap_or_else(|| vec![Arc::clone(cell)])
        })
        .collect::<Vec<_>>();
    if split.len() != cells.len() || split.iter().zip(cells).any(|(a, b)| !Arc::ptr_eq(a, b)) {
        let mut normalized = split;
        if let Some((range, replacements)) = reunite_code_mode(&normalized) {
            normalized.splice(range, replacements);
        }
        return Some((0..cells.len(), normalized));
    }
    let mut blocks: BTreeMap<String, (usize, CodeModeCell)> = BTreeMap::new();
    let mut references: BTreeMap<(String, String), (usize, Arc<dyn HistoryCell>)> = BTreeMap::new();
    let mut changed = false;
    for (index, cell) in cells.iter().enumerate() {
        if let Some(fact) = cell.as_any().downcast_ref::<CodeModeFactCell>()
            && let CodeModeEvent::WaitStarted { call_id } | CodeModeEvent::WaitCompleted { call_id } =
                &fact.0.event
        {
            let key = (fact.0.exec_call_id.clone(), call_id.clone());
            if let Some((_, retained)) = references.get_mut(&key) {
                changed = true;
                if matches!(fact.0.event, CodeModeEvent::WaitCompleted { .. }) {
                    *retained = Arc::clone(cell);
                }
            } else {
                references.insert(key, (index, Arc::clone(cell)));
            }
        }
        let Some(block) = cell.as_any().downcast_ref::<CodeModeCell>() else {
            continue;
        };
        if let Some((anchor, retained)) = blocks.get_mut(&block.exec_call_id) {
            changed = true;
            if block.status().is_some()
                && block.status().map(|(_, at)| at) >= retained.status().map(|(_, at)| at)
            {
                *anchor = index;
            }
            for fact in &block.facts {
                retained.insert_fact(fact.clone());
            }
        } else {
            let mut retained = block.clone();
            retained.members.clear();
            blocks.insert(block.exec_call_id.clone(), (index, retained));
        }
    }
    if blocks.is_empty() {
        return None;
    }
    let mut absorbed = HashSet::new();
    for (index, cell) in cells.iter().enumerate() {
        if let Some(block) = cell.as_any().downcast_ref::<CodeModeCell>() {
            let (_, retained) = blocks.get_mut(&block.exec_call_id)?;
            for member in &block.members {
                retained.insert_member(Arc::clone(member));
            }
            continue;
        }
        if let Some((_, block)) = blocks
            .values_mut()
            .find(|(_, block)| block.owns(cell.as_ref()))
        {
            block.insert_member(Arc::clone(cell));
            absorbed.insert(index);
        }
    }
    if !changed && absorbed.is_empty() {
        return None;
    }
    let mut replacements = Vec::new();
    for (index, cell) in cells.iter().enumerate() {
        if let Some(fact) = cell.as_any().downcast_ref::<CodeModeFactCell>()
            && let CodeModeEvent::WaitStarted { call_id } | CodeModeEvent::WaitCompleted { call_id } =
                &fact.0.event
        {
            let (anchor, reference) = &references[&(fact.0.exec_call_id.clone(), call_id.clone())];
            if *anchor == index {
                replacements.push(Arc::clone(reference));
            }
        } else if let Some(block) = cell.as_any().downcast_ref::<CodeModeCell>() {
            let (anchor, merged) = &blocks[&block.exec_call_id];
            if *anchor == index {
                replacements.push(Arc::new(merged.clone()) as Arc<dyn HistoryCell>);
            }
        } else if !absorbed.contains(&index) {
            replacements.push(Arc::clone(cell));
        }
    }
    let start = cells
        .iter()
        .zip(&replacements)
        .take_while(|(old, new)| Arc::ptr_eq(old, new))
        .count();
    let suffix = cells[start..]
        .iter()
        .rev()
        .zip(replacements[start..].iter().rev())
        .take_while(|(old, new)| Arc::ptr_eq(old, new))
        .count();
    let end = cells.len() - suffix;
    let replacement_end = replacements.len() - suffix;
    Some((start..end, replacements[start..replacement_end].to_vec()))
}
