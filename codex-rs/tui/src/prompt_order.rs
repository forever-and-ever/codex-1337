//! Human-only source aliases and prompt-relative context placement.
use crate::history_cell::ArrivalCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::HistoryTimestamp;
use crate::history_cell::UserHistoryCell;
use codex_app_server_protocol::ThreadItemEntry;
use codex_app_server_protocol::ThreadItemPayload;
use codex_protocol::step_admission::ArrivalSide;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct UserPromptIdentity {
    pub(crate) client_id: Option<String>,
    pub(crate) source_response_item_id: Option<codex_protocol::ResponseItemId>,
}

/// Admission clocks retained from history-page envelopes, never inferred from turn times.
#[derive(Clone, Debug, Default)]
pub(crate) struct UserPromptClocks(HashMap<UserPromptIdentity, HistoryTimestamp>);

impl UserPromptClocks {
    pub(crate) fn record(&mut self, entry: &ThreadItemEntry) {
        let ThreadItemPayload::UserMessage {
            client_id,
            source_response_item_id,
            ..
        } = &entry.item.payload
        else {
            return;
        };
        if client_id.is_none() && source_response_item_id.is_none() {
            return;
        }
        let timestamp = entry
            .item
            .completed_at_ms
            .and_then(HistoryTimestamp::from_millis)
            .or_else(|| {
                entry
                    .item
                    .started_at_ms
                    .and_then(HistoryTimestamp::from_millis)
            });
        if let Some(timestamp) = timestamp {
            self.0.insert(
                UserPromptIdentity {
                    client_id: client_id.clone(),
                    source_response_item_id: source_response_item_id.clone(),
                },
                timestamp,
            );
        }
    }

    pub(crate) fn stamped(&self, cell: &dyn HistoryCell) -> Option<UserHistoryCell> {
        let user = cell.as_any().downcast_ref::<UserHistoryCell>()?;
        if user.timestamp.is_some() {
            return None;
        }
        let timestamp = *self.0.get(&user.prompt_identity)?;
        let mut user = user.clone();
        user.timestamp = Some(timestamp);
        Some(user)
    }

    /// Stamp only fresh projections, before they enter the immutable transcript.
    pub(crate) fn stamp(&self, cells: &mut [Arc<dyn HistoryCell>]) {
        for cell in cells {
            if let Some(user) = self.stamped(cell.as_ref()) {
                *cell = Arc::new(user);
            }
        }
    }
}

#[derive(Default)]
struct Interval {
    arrivals: Vec<(usize, Arc<dyn HistoryCell>)>,
    cursor: usize,
}

pub(crate) fn reordered(cells: &[Arc<dyn HistoryCell>]) -> Vec<Arc<dyn HistoryCell>> {
    let users = cells
        .iter()
        .enumerate()
        .filter_map(|(index, cell)| {
            cell.as_any()
                .downcast_ref::<UserHistoryCell>()
                .map(|user| (index, &user.prompt_identity))
        })
        .collect::<Vec<_>>();
    let source_users = users
        .iter()
        .enumerate()
        .filter_map(|(position, (_, user))| {
            user.source_response_item_id
                .as_deref()
                .map(|id| (id, position))
        })
        .collect::<BTreeMap<_, _>>();
    let mut intervals = BTreeMap::<Option<usize>, Interval>::new();
    let mut membership = BTreeMap::new();
    let mut unresolved = std::collections::BTreeSet::new();
    for (index, cell) in cells.iter().enumerate() {
        let Some(placement) = cell
            .as_any()
            .downcast_ref::<ArrivalCell>()
            .and_then(ArrivalCell::placement)
        else {
            continue;
        };
        let Some(&anchor) = source_users.get(placement.user_response_item_id.as_str()) else {
            unresolved.insert(index);
            continue;
        };
        let (left, right) = match placement.side {
            ArrivalSide::Before => (
                anchor.checked_sub(1).map(|index| users[index].0),
                Some(users[anchor].0),
            ),
            ArrivalSide::After => (
                Some(users[anchor].0),
                users.get(anchor + 1).map(|(index, _)| *index),
            ),
        };
        intervals
            .entry(right)
            .or_default()
            .arrivals
            .push((index, Arc::clone(cell)));
        let stationary =
            left.is_none_or(|left| index > left) && right.is_none_or(|right| index < right);
        membership.insert(index, (right, stationary));
    }
    // Do not cross an unresolved source anchor: a partial page could otherwise
    // reverse peers before their common user interval becomes known. Block the
    // whole cohort, including cohorts which would cross one already held back.
    loop {
        let blocked = intervals
            .iter()
            .filter_map(|(right, interval)| {
                // These are exactly the slots used below: retained peer slots,
                // then the remaining peers immediately before the right user.
                let slots = interval
                    .arrivals
                    .iter()
                    .filter(|(index, _)| membership[index].1)
                    .map(|(index, _)| *index)
                    .chain(std::iter::repeat(right.unwrap_or(cells.len())));
                interval
                    .arrivals
                    .iter()
                    .zip(slots)
                    .any(|((index, _), destination)| {
                        unresolved
                            .range((*index).min(destination)..=(*index).max(destination))
                            .next()
                            .is_some()
                    })
                    .then_some(*right)
            })
            .collect::<Vec<_>>();
        if blocked.is_empty() {
            break;
        }
        for right in blocked {
            let Some(interval) = intervals.remove(&right) else {
                unreachable!("blocked keys were collected from distinct live intervals");
            };
            unresolved.extend(interval.arrivals.into_iter().map(|(index, _)| index));
        }
    }
    membership.retain(|_, (right, _)| intervals.contains_key(right));
    // Admission emits peers in source order. Keep that persisted order across
    // batches; sampled indices are not comparable after a history replacement.
    let mut ordered = Vec::with_capacity(cells.len());
    for (index, cell) in cells.iter().enumerate() {
        if let Some(interval) = intervals.get_mut(&Some(index)) {
            ordered.extend(
                interval.arrivals[interval.cursor..]
                    .iter()
                    .map(|(_, cell)| Arc::clone(cell)),
            );
            interval.cursor = interval.arrivals.len();
        }
        if let Some((right, stationary)) = membership.get(&index) {
            if *stationary {
                let Some(interval) = intervals.get_mut(right) else {
                    unreachable!("membership retains only live intervals");
                };
                ordered.push(Arc::clone(&interval.arrivals[interval.cursor].1));
                interval.cursor += 1;
            }
        } else {
            ordered.push(Arc::clone(cell));
        }
    }
    if let Some(interval) = intervals.get(&None) {
        ordered.extend(
            interval.arrivals[interval.cursor..]
                .iter()
                .map(|(_, cell)| Arc::clone(cell)),
        );
    }
    ordered
}

pub(crate) fn normalize(cells: &mut Vec<Arc<dyn HistoryCell>>) {
    *cells = reordered(cells);
}
