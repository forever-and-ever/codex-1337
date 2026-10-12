//! Seat terminal metadata at the source turn boundary, including unloaded failure witnesses.
//! A split turn receives its terminal metadata only on the page containing its last item.

use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnStatus;
use std::collections::HashMap;

pub(super) fn group_turn_items(
    items: Vec<ThreadItem>,
    turns: &[Turn],
) -> Vec<(Vec<ThreadItem>, Option<&Turn>)> {
    let owners: HashMap<_, _> = turns
        .iter()
        .enumerate()
        .flat_map(|(index, turn)| turn.items.iter().map(move |item| (item.id(), index)))
        .collect();
    let mut groups = Vec::new();
    let mut pending = Vec::new();
    let mut next_empty = 0;
    for item in items {
        let owner = owners.get(item.id()).copied();
        if let Some(index) = owner {
            // NotLoaded cannot distinguish a real zero-item failure from an unloaded
            // turn. Keep its witness before this page until its own items arrive.
            for turn in &turns[next_empty..index.max(next_empty)] {
                if turn.items.is_empty() && turn.status == TurnStatus::Failed {
                    if !pending.is_empty() {
                        groups.push((std::mem::take(&mut pending), None));
                    }
                    groups.push((Vec::new(), Some(turn)));
                }
            }
            next_empty = next_empty.max(index + 1);
        }
        let boundary = owner
            .map(|index| &turns[index])
            .filter(|turn| turn.items.last().is_some_and(|last| last.id() == item.id()));
        pending.push(item);
        if let Some(turn) = boundary {
            let completed_turn =
                matches!(turn.status, TurnStatus::Completed | TurnStatus::Failed).then_some(turn);
            groups.push((std::mem::take(&mut pending), completed_turn));
        }
    }
    if !pending.is_empty() {
        groups.push((pending, None));
    }
    groups
}

#[cfg(test)]
#[path = "history_completion_tests.rs"]
mod tests;
