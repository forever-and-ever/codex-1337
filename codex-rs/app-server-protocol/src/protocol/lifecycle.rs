//! The original completion remains stable when a request bill is sealed by a later response.

use codex_protocol::items::TurnItem;

pub(crate) fn completed_at_ms(item: &TurnItem, event_time: i64) -> i64 {
    match item {
        TurnItem::StepAdmission(item) => item.completed_at_ms,
        _ => event_time,
    }
}
