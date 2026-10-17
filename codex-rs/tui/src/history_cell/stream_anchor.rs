//! A stream's insertion locus before it has any unambiguous rows.

use super::*;

#[derive(Debug)]
pub(crate) struct StreamAnchorCell {
    pub(crate) item_id: String,
}

impl HistoryCell for StreamAnchorCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        Vec::new()
    }
    fn raw_lines(&self) -> Vec<Line<'static>> {
        Vec::new()
    }
    fn activity_disclosure(&self, _width: u16) -> Option<ActivityDisclosure> {
        None
    }
    fn is_stream_continuation(&self) -> bool {
        true
    }
}

pub(crate) fn streaming_item_id(cell: &dyn HistoryCell) -> Option<&str> {
    let any = cell.as_any();
    if let Some(rows) = any.downcast_ref::<StreamRowsCell>() {
        return rows.item_id.as_deref();
    }
    if let Some(anchor) = any.downcast_ref::<StreamAnchorCell>() {
        return Some(&anchor.item_id);
    }
    if let Some(message) = any.downcast_ref::<AgentMessageCell>() {
        return message.item_id.as_deref();
    }
    any.downcast_ref::<ProposedPlanStreamCell>()
        .and_then(|plan| plan.item_id.as_deref())
}
