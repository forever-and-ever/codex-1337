//! Exact invocation reunion is deliberately restricted to agent waits.
//! Code Mode blocks are opaque: their execution, not a nested wait, owns captured JS output.

use super::HistoryCell;
use super::ProjectionChange;
use super::ToolOutputCell;
use super::ToolOutputGroup;
use crate::multi_agents::AgentToolHistoryCell;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::sync::Arc;

type Cell = Arc<dyn HistoryCell>;

#[derive(Default)]
struct WaitMembers {
    owner: Option<(usize, usize, Cell)>,
    outputs: Vec<(usize, usize, Cell)>,
}

fn eligible(cell: &Cell, calls: &mut HashSet<String>) {
    if let Some(wait) = AgentToolHistoryCell::wait_for(cell.as_ref()) {
        calls.insert(wait.id.clone());
    } else if let Some(output) = cell.as_any().downcast_ref::<ToolOutputCell>() {
        if output.is_agent_wait()
            && let Some(call) = output.outer_call_id()
        {
            calls.insert(call.to_owned());
        }
    } else if let Some(group) = cell.as_any().downcast_ref::<ToolOutputGroup>() {
        for member in &group.members {
            eligible(member, calls);
        }
    }
}

fn extract(
    cell: &Cell,
    index: usize,
    calls: &HashSet<String>,
    waits: &mut BTreeMap<String, WaitMembers>,
    position: &mut usize,
) -> Option<Cell> {
    let order = *position;
    *position += 1;
    if let Some(wait) = AgentToolHistoryCell::wait_for(cell.as_ref()) {
        let members = waits.entry(wait.id.clone()).or_default();
        // A late start never replaces observed terminal evidence.
        if members.owner.as_ref().is_none_or(|(_, _, owner)| {
            let Some(old) = AgentToolHistoryCell::wait_for(owner.as_ref()) else {
                unreachable!("wait extraction only stores lifecycle owners");
            };
            old.state.is_running() || !wait.state.is_running()
        }) {
            let (anchor, order) = members
                .owner
                .as_ref()
                .map_or((index, order), |(anchor, order, _)| (*anchor, *order));
            members.owner = Some((anchor, order, Arc::clone(cell)));
        }
        return None;
    }
    if let Some(output) = cell.as_any().downcast_ref::<ToolOutputCell>()
        && let Some(call) = output.outer_call_id()
        && calls.contains(call)
    {
        waits
            .entry(call.to_owned())
            .or_default()
            .outputs
            .push((index, order, Arc::clone(cell)));
        return None;
    }
    if let Some(group) = cell.as_any().downcast_ref::<ToolOutputGroup>() {
        let members = group
            .members
            .iter()
            .filter_map(|member| extract(member, index, calls, waits, position))
            .collect::<Vec<_>>();
        if members.len() == group.members.len() {
            return Some(Arc::clone(cell));
        }
        return match members.len() {
            0 => None,
            1 => members.into_iter().next(),
            _ => Some(ToolOutputGroup::from_cells(members)),
        };
    }
    Some(Arc::clone(cell))
}

fn normalize(cells: &[Cell]) -> Option<Vec<Cell>> {
    let mut calls = HashSet::new();
    for cell in cells {
        eligible(cell, &mut calls);
    }
    if calls.is_empty() {
        return None;
    }
    let mut waits = BTreeMap::new();
    let mut position = 0;
    let mut remaining = cells
        .iter()
        .enumerate()
        .map(|(index, cell)| extract(cell, index, &calls, &mut waits, &mut position))
        .collect::<Vec<_>>();
    let mut groups: BTreeMap<usize, Vec<(usize, Cell)>> = BTreeMap::new();
    for members in waits.into_values() {
        let Some((anchor, order)) = members
            .owner
            .as_ref()
            .map(|(index, order, _)| (*index, *order))
            .or_else(|| {
                members
                    .outputs
                    .first()
                    .map(|(index, order, _)| (*index, *order))
            })
        else {
            unreachable!("wait extraction creates an entry with an owner or output");
        };
        let mut members = members
            .owner
            .into_iter()
            .map(|(_, _, cell)| cell)
            .chain(members.outputs.into_iter().map(|(_, _, cell)| cell))
            .collect::<Vec<_>>();
        let group = match members.len() {
            1 => members.remove(0),
            _ => ToolOutputGroup::from_cells(members),
        };
        // Reuse immutable canonical groups so unrelated insertions do not churn their layout.
        let group = if same_members(&cells[anchor], &group) {
            Arc::clone(&cells[anchor])
        } else {
            group
        };
        groups.entry(anchor).or_default().push((order, group));
    }
    let mut normalized = Vec::new();
    for (index, cell) in remaining.iter_mut().enumerate() {
        if let Some(mut groups) = groups.remove(&index) {
            groups.sort_by_key(|(order, _)| *order);
            normalized.extend(groups.into_iter().map(|(_, group)| group));
        }
        normalized.extend(cell.take());
    }
    (!same_sequence(cells, &normalized)).then_some(normalized)
}

fn same_members(old: &Cell, new: &Cell) -> bool {
    if Arc::ptr_eq(old, new) {
        return true;
    }
    match (
        old.as_any().downcast_ref::<ToolOutputGroup>(),
        new.as_any().downcast_ref::<ToolOutputGroup>(),
    ) {
        (Some(old), Some(new)) => same_sequence(&old.members, &new.members),
        _ => false,
    }
}

fn same_sequence(old: &[Cell], new: &[Cell]) -> bool {
    old.len() == new.len() && old.iter().zip(new).all(|(old, new)| Arc::ptr_eq(old, new))
}

fn change(cells: &[Cell], mut normalized: Vec<Cell>) -> ProjectionChange {
    let prefix = cells
        .iter()
        .zip(&normalized)
        .take_while(|(old, new)| Arc::ptr_eq(old, new))
        .count();
    let suffix = cells[prefix..]
        .iter()
        .rev()
        .zip(normalized[prefix..].iter().rev())
        .take_while(|(old, new)| Arc::ptr_eq(old, new))
        .count();
    normalized.truncate(normalized.len() - suffix);
    (
        prefix..cells.len() - suffix,
        normalized.into_iter().skip(prefix).collect(),
    )
}

fn owns_wait(cell: &Cell, call: &str) -> bool {
    if let Some(wait) = AgentToolHistoryCell::wait_for(cell.as_ref()) {
        wait.id == call
    } else if let Some(output) = cell.as_any().downcast_ref::<ToolOutputCell>() {
        output.is_agent_wait() && output.outer_call_id() == Some(call)
    } else if let Some(group) = cell.as_any().downcast_ref::<ToolOutputGroup>() {
        group.members.iter().any(|member| owns_wait(member, call))
    } else {
        false
    }
}

fn concerns_wait(cell: &Cell, cells: &[Cell]) -> bool {
    if AgentToolHistoryCell::wait_for(cell.as_ref()).is_some() {
        true
    } else if let Some(output) = cell.as_any().downcast_ref::<ToolOutputCell>() {
        output.outer_call_id().is_some_and(|call| {
            output.is_agent_wait() || cells.iter().any(|owner| owns_wait(owner, call))
        })
    } else if let Some(group) = cell.as_any().downcast_ref::<ToolOutputGroup>() {
        group
            .members
            .iter()
            .any(|member| concerns_wait(member, cells))
    } else {
        false
    }
}

pub(crate) fn reconcile_agent_wait(cells: &[Cell], incoming: &Cell) -> Option<ProjectionChange> {
    if !concerns_wait(incoming, cells) {
        return None;
    }
    let extended = cells
        .iter()
        .cloned()
        .chain([Arc::clone(incoming)])
        .collect::<Vec<_>>();
    normalize(&extended).map(|normalized| change(cells, normalized))
}

pub(crate) fn reunite_agent_wait(cells: &[Cell]) -> Option<ProjectionChange> {
    normalize(cells).map(|normalized| change(cells, normalized))
}

pub(super) fn wait_output_parts(
    owner: Option<&dyn HistoryCell>,
    outputs: &[&ToolOutputCell],
    width: u16,
    expanded: &HashSet<String>,
    mode: super::output_provenance::OutputMode,
) -> super::activity_parts::ActivityParts {
    use super::ActivityDisclosure;
    use super::activity_parts::PartControl;
    use super::output_provenance::client_notice;
    let first = match (owner, outputs.first()) {
        (Some(owner), _) => owner,
        (None, Some(output)) => *output,
        (None, None) => unreachable!("wait disclosure requires a lifecycle or captured output"),
    };
    let mut lines = first.compact_hyperlink_lines(width);
    super::output_provenance::mark_lines(
        &mut lines,
        super::output_provenance::OutputProvenance::Client,
    );
    let mut controls = Vec::new();
    let open = owner
        .into_iter()
        .flat_map(HistoryCell::activity_ids)
        .chain(outputs.iter().flat_map(|output| output.activity_ids()))
        .any(|id| expanded.contains(&id));
    if open {
        if let Some(owner) = owner {
            lines.extend(owner.activity_argument_lines(width));
        }
        for output in outputs {
            let id = format!("result:{}", output.id());
            let open = expanded.contains(&id);
            controls.push(PartControl {
                id,
                line: lines.len(),
                expanded: open,
            });
            lines.push(client_notice("  Result"));
            if open {
                lines.extend(output.output_lines(width, mode));
            }
        }
    }
    super::activity_parts::ActivityParts {
        lines,
        header: first.activity_header(),
        output: Some(ActivityDisclosure::Generic),
        controls,
    }
}
