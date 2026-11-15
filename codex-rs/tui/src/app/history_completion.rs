//! Seat terminal metadata on the page owning its source-turn boundary.

use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnItemsView;
use codex_app_server_protocol::TurnStatus;
use std::collections::HashMap;
use std::ops::Range;

pub(super) fn group_turn_items<'a>(
    items: &[ThreadItem],
    turns: &'a [Turn],
) -> Vec<(Range<usize>, Option<&'a Turn>)> {
    let offsets: HashMap<_, _> = items
        .iter()
        .enumerate()
        .map(|(index, item)| (item.id(), index + 1))
        .collect();
    let first = turns.iter().find_map(|turn| turn.items.first());
    let mut end = first
        .filter(|item| offsets.get(item.id()) == Some(&1))
        .map(|_| 0);
    let mut start = 0;
    let mut groups = Vec::new();
    for turn in turns {
        if let Some(last) = turn.items.last() {
            end = offsets.get(last.id()).copied();
        } else if turn.status != TurnStatus::Failed
            && (turn.status != TurnStatus::Interrupted
                || turn.items_view == TurnItemsView::NotLoaded)
        {
            // An unloaded interruption has no admitted boundary of its own.
            continue;
        }
        if let Some(end) = end {
            groups.push((start..end, Some(turn)));
            start = end;
        }
    }
    if start < items.len() {
        groups.push((start..items.len(), None));
    }
    groups
}

#[cfg(test)]
#[path = "history_completion_tests.rs"]
mod tests;
